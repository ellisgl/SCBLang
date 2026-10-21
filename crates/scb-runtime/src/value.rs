//! Runtime values (SPEC "Collections and values", CLAUDE.md "unboxed primitives").
//!
//! Primitives are stored inline in [`Value`] (no allocation). Everything else lives
//! behind an `Rc`:
//!
//! - value types (strings, bytes, lists, maps, sets, tuples, structs, enums with
//!   payloads) use `Rc::make_mut` for copy-on-write: a write mutates in place when
//!   the value is not shared (refcount 1) and copies that one level otherwise;
//! - class instances are `Rc<RefCell<…>>`: a reference shared by every holder.
//!
//! `Clone` on a `Value` is therefore always cheap (a copy or a refcount increment).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use scb_types::decls::DeclId;
use scb_types::tir::{FuncId, LocalId};
use scb_types::ty::{FloatTy, IntTy};

#[derive(Debug, Clone, Default)]
pub enum Value {
    /// Also the content of a slot that has not been assigned yet.
    #[default]
    Null,
    Bool(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    Str(Rc<String>),
    Bytes(Rc<Vec<u8>>),
    List(Rc<Vec<Value>>),
    Tuple(Rc<Vec<Value>>),
    Map(Rc<OrderedMap>),
    Set(Rc<OrderedMap>),
    Range(Rc<RangeValue>),
    Struct(Rc<StructValue>),
    Enum(Rc<EnumValue>),
    Object(Rc<RefCell<ObjectValue>>),
    Func(Rc<FuncValue>),
}

#[derive(Debug, Clone)]
pub struct StructValue {
    pub decl: DeclId,
    pub fields: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    pub decl: DeclId,
    pub case: u32,
    pub payload: Vec<Value>,
}

#[derive(Debug)]
pub struct ObjectValue {
    /// The runtime class (dispatch uses it, not the static type).
    pub decl: DeclId,
    pub fields: Vec<Value>,
    /// State of natively implemented classes (`Throwable`'s message, code, …).
    pub native: Option<NativeState>,
    /// Set for instances of a class with `__destruct`: the isolate's queue of
    /// objects whose destructor is due (see [`DestructQueue`]).
    pub destructor: Option<DestructQueue>,
}

/// Objects whose last reference is gone and whose `__destruct` has not run yet
/// (SPEC "Destructors"). Owned by the isolate; each destructible object holds a
/// handle so its `Drop` can enqueue it, since `Drop` cannot call into the
/// interpreter. The interpreter runs the queue at statement boundaries and when a
/// block ends, so destructors still run before the next statement.
pub type DestructQueue = Rc<RefCell<VecDeque<ObjectValue>>>;

impl ObjectValue {
    pub fn new(decl: DeclId, fields: Vec<Value>, native: Option<NativeState>) -> ObjectValue {
        ObjectValue {
            decl,
            fields,
            native,
            destructor: None,
        }
    }
}

impl Drop for ObjectValue {
    /// The last reference is gone: if a destructor is due, move the contents to the
    /// queue (the queued object has no handle, so it is freed for good after its
    /// destructor runs, its fields then released in declaration order).
    fn drop(&mut self) {
        if let Some(queue) = self.destructor.take() {
            let obj = ObjectValue::new(
                self.decl,
                std::mem::take(&mut self.fields),
                self.native.take(),
            );
            queue.borrow_mut().push_back(obj);
        }
    }
}

#[derive(Debug, Clone)]
pub enum NativeState {
    Throwable(ThrowableState),
}

#[derive(Debug, Clone)]
pub struct ThrowableState {
    pub message: String,
    pub code: i64,
    pub previous: Option<Value>,
    /// Where it was created: file and line.
    pub file: String,
    pub line: i64,
    /// Call stack at creation, innermost first: `function (file:line)`.
    pub trace: Vec<String>,
}

/// An integer range as an arithmetic sequence: `count` values from `first`, `step`
/// apart (SPEC "Ranges"). `5..=1` has `count` 0; `reversed()` flips the direction.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeValue {
    pub first: i128,
    pub step: i128,
    pub count: i128,
    pub elem: IntTy,
}

impl RangeValue {
    pub fn new(start: i128, end: i128, inclusive: bool, elem: IntTy) -> RangeValue {
        let last = if inclusive { end } else { end - 1 };
        RangeValue {
            first: start,
            step: 1,
            count: (last - start + 1).max(0),
            elem,
        }
    }

    pub fn get(&self, i: i128) -> i128 {
        self.first + i * self.step
    }

    pub fn contains(&self, v: i128) -> bool {
        if self.count == 0 {
            return false;
        }
        let offset = v - self.first;
        offset % self.step == 0 && (0..self.count).contains(&(offset / self.step))
    }
}

/// A function value: a closure or a reference to a function or method.
#[derive(Debug)]
pub enum FuncValue {
    /// `fn(…) => …`: the function and the captured copies (slot, value).
    Closure {
        func: FuncId,
        captures: Vec<(LocalId, Value)>,
    },
    /// `f(...)`, `C::m(...)`.
    Func(FuncId),
    /// `$obj->m(...)`: a method bound to its receiver.
    Method { receiver: Value, func: FuncId },
    /// A core-library function or method (`$list->count(...)`): its stub.
    Builtin {
        func: FuncId,
        receiver: Option<Value>,
    },
}

/// An insertion-ordered hash map (also the storage of sets, with `Null` values).
///
/// Entries live in a vector in insertion order; removal leaves a hole (`None`) that
/// is compacted away once holes outnumber entries. The index maps a key's hash to
/// the positions with that hash; key equality is decided by the caller (it may need
/// to run user `equals()` methods), so lookups take a comparison function.
#[derive(Debug, Clone, Default)]
pub struct OrderedMap {
    entries: Vec<Option<(Value, Value)>>,
    hashes: Vec<u64>,
    index: HashMap<u64, Vec<usize>>,
    len: usize,
}

impl OrderedMap {
    pub fn new() -> OrderedMap {
        OrderedMap::default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The position of `key`, given its hash and an equality test.
    pub fn find(
        &self,
        hash: u64,
        key: &Value,
        eq: &mut dyn FnMut(&Value, &Value) -> bool,
    ) -> Option<usize> {
        let slots = self.index.get(&hash)?;
        slots
            .iter()
            .copied()
            .find(|&i| self.entries[i].as_ref().is_some_and(|(k, _)| eq(k, key)))
    }

    pub fn get(&self, pos: usize) -> Option<&(Value, Value)> {
        self.entries[pos].as_ref()
    }

    pub fn value_mut(&mut self, pos: usize) -> &mut Value {
        &mut self.entries[pos].as_mut().expect("a live entry").1
    }

    /// Inserts or replaces; returns the previous value.
    pub fn insert(
        &mut self,
        hash: u64,
        key: Value,
        value: Value,
        eq: &mut dyn FnMut(&Value, &Value) -> bool,
    ) -> Option<Value> {
        if let Some(pos) = self.find(hash, &key, eq) {
            return Some(std::mem::replace(self.value_mut(pos), value));
        }
        let pos = self.entries.len();
        self.entries.push(Some((key, value)));
        self.hashes.push(hash);
        self.index.entry(hash).or_default().push(pos);
        self.len += 1;
        None
    }

    pub fn remove(
        &mut self,
        hash: u64,
        key: &Value,
        eq: &mut dyn FnMut(&Value, &Value) -> bool,
    ) -> Option<Value> {
        let pos = self.find(hash, key, eq)?;
        let (_, v) = self.entries[pos].take().expect("found entries are live");
        if let Some(slots) = self.index.get_mut(&hash) {
            slots.retain(|&i| i != pos);
            if slots.is_empty() {
                self.index.remove(&hash);
            }
        }
        self.len -= 1;
        if self.entries.len() > 16 && self.len * 2 < self.entries.len() {
            self.compact();
        }
        Some(v)
    }

    pub fn clear(&mut self) {
        *self = OrderedMap::default();
    }

    /// Live entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &(Value, Value)> {
        self.entries.iter().flatten()
    }

    fn compact(&mut self) {
        let old = std::mem::take(&mut self.entries);
        let hashes = std::mem::take(&mut self.hashes);
        self.index.clear();
        for (e, h) in old.into_iter().zip(hashes) {
            if let Some(e) = e {
                let pos = self.entries.len();
                self.entries.push(Some(e));
                self.hashes.push(h);
                self.index.entry(h).or_default().push(pos);
            }
        }
    }
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::new(s.into()))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }

    pub fn as_bool(&self) -> bool {
        matches!(self, Value::Bool(true))
    }

    /// Any integer as `i128` (exact for every SCB integer type).
    pub fn as_i128(&self) -> Option<i128> {
        Some(match *self {
            Value::I8(v) => v.into(),
            Value::I16(v) => v.into(),
            Value::I32(v) => v.into(),
            Value::I64(v) => v.into(),
            Value::U8(v) => v.into(),
            Value::U16(v) => v.into(),
            Value::U32(v) => v.into(),
            Value::U64(v) => v.into(),
            _ => return None,
        })
    }

    /// `int` arguments (indexes, counts) as `i64`.
    pub fn as_i64(&self) -> i64 {
        self.as_i128().map_or(0, |v| v as i64)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::F32(v) => Some(v.into()),
            Value::F64(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Value::Str(s) => s,
            _ => "",
        }
    }

    pub fn int_ty(&self) -> Option<IntTy> {
        Some(match self {
            Value::I8(_) => IntTy::I8,
            Value::I16(_) => IntTy::I16,
            Value::I32(_) => IntTy::I32,
            Value::I64(_) => IntTy::I64,
            Value::U8(_) => IntTy::U8,
            Value::U16(_) => IntTy::U16,
            Value::U32(_) => IntTy::U32,
            Value::U64(_) => IntTy::U64,
            _ => return None,
        })
    }

    /// An integer of type `ty`; the caller guarantees `v` is in range.
    pub fn int(ty: IntTy, v: i128) -> Value {
        match ty {
            IntTy::I8 => Value::I8(v as i8),
            IntTy::I16 => Value::I16(v as i16),
            IntTy::I32 => Value::I32(v as i32),
            IntTy::I64 => Value::I64(v as i64),
            IntTy::U8 => Value::U8(v as u8),
            IntTy::U16 => Value::U16(v as u16),
            IntTy::U32 => Value::U32(v as u32),
            IntTy::U64 => Value::U64(v as u64),
        }
    }

    pub fn float(ty: FloatTy, v: f64) -> Value {
        match ty {
            FloatTy::F32 => Value::F32(v as f32),
            FloatTy::F64 => Value::F64(v),
        }
    }

    /// The declaration of a struct, enum or class value.
    pub fn decl(&self) -> Option<DeclId> {
        match self {
            Value::Struct(s) => Some(s.decl),
            Value::Enum(e) => Some(e.decl),
            Value::Object(o) => Some(o.borrow().decl),
            _ => None,
        }
    }
}

/// Float to string (SPEC "Casts and conversions"): the shortest digits that
/// round-trip at the value's own precision; integral values keep `.0`; plain decimal
/// for `1e-6 ≤ |x| < 1e21`, otherwise exponent form (`1.0e21`, `1.5e-7`); `NAN`,
/// `INF`, `-INF`.
pub fn format_float(v: f64, f32_precision: bool) -> String {
    if v.is_nan() {
        return "NAN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "INF".into() } else { "-INF".into() };
    }
    let abs = v.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        // Rust's `{:e}` is shortest round-trip too: `1e21`, `1.5e-7`.
        let s = if f32_precision {
            format!("{:e}", v as f32)
        } else {
            format!("{v:e}")
        };
        let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let mantissa = if mantissa.contains('.') {
            mantissa.to_string()
        } else {
            format!("{mantissa}.0")
        };
        return format!("{mantissa}e{exp}");
    }
    let s = if f32_precision {
        format!("{}", v as f32)
    } else {
        format!("{v}")
    };
    if s.contains('.') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_format_matches_the_spec() {
        assert_eq!(format_float(0.1 + 0.2, false), "0.30000000000000004");
        assert_eq!(format_float(3.0, false), "3.0");
        assert_eq!(format_float(-0.0, false), "-0.0");
        assert_eq!(format_float(1.1, true), "1.1");
        assert_eq!(format_float(1e21, false), "1.0e21");
        assert_eq!(format_float(1.5e-7, false), "1.5e-7");
        assert_eq!(format_float(1e-6, false), "0.000001");
        assert_eq!(format_float(123456789.0, false), "123456789.0");
        assert_eq!(format_float(f64::NAN, false), "NAN");
        assert_eq!(format_float(f64::NEG_INFINITY, false), "-INF");
    }

    #[test]
    fn ranges() {
        let r = RangeValue::new(0, 9, true, IntTy::I64);
        assert_eq!(r.count, 10);
        assert!(r.contains(9) && !r.contains(10));
        assert_eq!(RangeValue::new(5, 1, true, IntTy::I64).count, 0);
        assert_eq!(RangeValue::new(1, 3, false, IntTy::I64).count, 2);
    }

    #[test]
    fn ordered_map_keeps_insertion_order_across_removals() {
        let mut m = OrderedMap::new();
        let mut eq = |a: &Value, b: &Value| a.as_i128() == b.as_i128();
        for i in 0..40 {
            m.insert(i as u64 % 3, Value::I64(i), Value::Null, &mut eq);
        }
        for i in 0..30 {
            m.remove(i as u64 % 3, &Value::I64(i), &mut eq);
        }
        let keys: Vec<i128> = m.iter().map(|(k, _)| k.as_i128().unwrap()).collect();
        assert_eq!(keys, (30..40).collect::<Vec<_>>());
        assert!(m.find(31 % 3, &Value::I64(31), &mut eq).is_some());
    }
}
