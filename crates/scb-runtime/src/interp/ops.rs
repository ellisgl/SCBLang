//! Operators at runtime (SPEC "Numerics", "Equality and comparison").
//!
//! Integer arithmetic is computed in `i128` (exact for every SCB integer type) and
//! range-checked against the operands' type: a result that does not fit traps with
//! `OverflowError`, in every build mode. Division and remainder by zero trap with
//! `DivisionByZeroError`; `MIN ~/ -1` overflows. Float arithmetic is IEEE, done at the
//! operands' own precision (`f32` stays `f32`).

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use scb_syntax::Span;
use scb_types::tir::BinaryOp;
use scb_types::ty::{IntTy, Ty, TyKind};

use super::{Interp, R};
use crate::value::{Value, format_float};

/// Structural equality without user methods: what map keys and set elements use.
/// Class instances compare by identity here (SPEC requires `Hashable` for keys; a
/// user-defined `hash()` / `equals()` is not consulted for keys in the POC).
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::F32(x), Value::F32(y)) => x == y,
        (Value::F64(x), Value::F64(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bytes(x), Value::Bytes(y)) => x == y,
        (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_equal(p, q))
        }
        (Value::Map(x), Value::Map(y)) | (Value::Set(x), Value::Set(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.find(hash_value(k), k, &mut values_equal)
                        .is_some_and(|pos| values_equal(&y.get(pos).expect("found").1, v))
                })
        }
        (Value::Range(x), Value::Range(y)) => x == y,
        (Value::Struct(x), Value::Struct(y)) => {
            x.decl == y.decl
                && x.fields
                    .iter()
                    .zip(&y.fields)
                    .all(|(p, q)| values_equal(p, q))
        }
        (Value::Enum(x), Value::Enum(y)) => {
            x.decl == y.decl
                && x.case == y.case
                && x.payload
                    .iter()
                    .zip(&y.payload)
                    .all(|(p, q)| values_equal(p, q))
        }
        (Value::Object(x), Value::Object(y)) => Rc::ptr_eq(x, y),
        (Value::Func(x), Value::Func(y)) => Rc::ptr_eq(x, y),
        _ => match (a.as_i128(), b.as_i128()) {
            (Some(x), Some(y)) => a.int_ty() == b.int_ty() && x == y,
            _ => false,
        },
    }
}

/// A hash consistent with [`values_equal`]. Maps and sets hash order-independently.
pub fn hash_value(v: &Value) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_into(v, &mut h);
    h.finish()
}

fn hash_into(v: &Value, h: &mut impl Hasher) {
    std::mem::discriminant(v).hash(h);
    match v {
        Value::Null => {}
        Value::Bool(b) => b.hash(h),
        Value::F32(f) => f.to_bits().hash(h),
        Value::F64(f) => f.to_bits().hash(h),
        Value::Str(s) => s.hash(h),
        Value::Bytes(b) => b.hash(h),
        Value::List(xs) | Value::Tuple(xs) => {
            xs.len().hash(h);
            for x in xs.iter() {
                hash_into(x, h);
            }
        }
        Value::Map(m) | Value::Set(m) => {
            let sum = m.iter().fold(0u64, |acc, (k, v)| {
                acc.wrapping_add(hash_value(k) ^ hash_value(v).rotate_left(17))
            });
            sum.hash(h);
        }
        Value::Range(r) => (r.first, r.step, r.count).hash(h),
        Value::Struct(s) => {
            s.decl.hash(h);
            for f in &s.fields {
                hash_into(f, h);
            }
        }
        Value::Enum(e) => {
            (e.decl, e.case).hash(h);
            for p in &e.payload {
                hash_into(p, h);
            }
        }
        Value::Object(o) => (Rc::as_ptr(o) as usize).hash(h),
        Value::Func(f) => (Rc::as_ptr(f) as *const u8 as usize).hash(h),
        _ => v.as_i128().hash(h),
    }
}

/// Wraps an integer to `ty`'s width (two's complement).
pub(crate) fn wrap(ty: IntTy, v: i128) -> i128 {
    let bits = ty.bits();
    let mask = (1i128 << bits) - 1;
    let low = v & mask;
    if ty.signed() && low >= 1i128 << (bits - 1) {
        low - (1i128 << bits)
    } else {
        low
    }
}

impl Interp<'_> {
    /// `int` overflow: the result does not fit the operands' type.
    pub(crate) fn overflow(&mut self, ty: IntTy, what: &str, span: Span) -> super::Flow {
        self.error(
            "OverflowError",
            format!("{what} overflows `{}`", int_name(ty)),
            span,
        )
    }

    /// `v` as a `ty`, or `OverflowError`. `what` describes the operation; it is only
    /// formatted on failure, keeping the common path free of allocation.
    pub(crate) fn checked(
        &mut self,
        ty: IntTy,
        v: i128,
        what: impl FnOnce() -> String,
        span: Span,
    ) -> R<Value> {
        if v < ty.min() || v > ty.max() {
            return Err(self.overflow(ty, &what(), span));
        }
        Ok(Value::int(ty, v))
    }

    /// Arithmetic, bitwise, shift and concatenation operators.
    pub(crate) fn arith(&mut self, op: BinaryOp, a: Value, b: Value, span: Span) -> R<Value> {
        if let (Some(ty), Some(x)) = (a.int_ty(), a.as_i128()) {
            let y = b.as_i128().unwrap_or(0);
            let what = || format!("{x} {} {y}", op_str(op));
            return match op {
                BinaryOp::Add => self.checked(ty, x + y, what, span),
                BinaryOp::Sub => self.checked(ty, x - y, what, span),
                // u64 × u64 can exceed i128: a product i128 cannot hold overflows any
                // SCB type.
                BinaryOp::Mul => match x.checked_mul(y) {
                    Some(v) => self.checked(ty, v, what, span),
                    None => Err(self.overflow(ty, &what(), span)),
                },
                BinaryOp::IntDiv | BinaryOp::Rem if y == 0 => Err(self.error(
                    "DivisionByZeroError",
                    format!("{}: division by zero", what()),
                    span,
                )),
                BinaryOp::IntDiv => self.checked(ty, x / y, what, span),
                BinaryOp::Rem => Ok(Value::int(ty, x % y)),
                BinaryOp::Pow => {
                    // |x| ≤ 1 never overflows (and would loop up to 2^32 times);
                    // any other base overflows within 64 multiplications.
                    match x {
                        0 => return Ok(Value::int(ty, i128::from(y == 0))),
                        1 => return Ok(Value::int(ty, 1)),
                        -1 => return Ok(Value::int(ty, if y % 2 == 0 { 1 } else { -1 })),
                        _ => {}
                    }
                    let mut r: i128 = 1;
                    for _ in 0..y {
                        r = r.checked_mul(x).unwrap_or(i128::MAX);
                        if r < ty.min() || r > ty.max() {
                            return Err(self.overflow(ty, &format!("{x} ** {y}"), span));
                        }
                    }
                    Ok(Value::int(ty, r))
                }
                BinaryOp::Shl | BinaryOp::Shr => {
                    if y >= i128::from(ty.bits()) {
                        return Err(self.error(
                            "OverflowError",
                            format!(
                                "shift by {y} is not less than the width of `{}`",
                                int_name(ty)
                            ),
                            span,
                        ));
                    }
                    let r = if op == BinaryOp::Shl {
                        wrap(ty, x << y)
                    } else {
                        x >> y
                    };
                    Ok(Value::int(ty, r))
                }
                BinaryOp::BitAnd => Ok(Value::int(ty, x & y)),
                BinaryOp::BitOr => Ok(Value::int(ty, x | y)),
                BinaryOp::BitXor => Ok(Value::int(ty, x ^ y)),
                _ => Err(super::Flow::Fatal(format!("integer operator {op:?}"))),
            };
        }
        match (a, b) {
            (Value::F32(x), Value::F32(y)) => Ok(Value::F32(match op {
                BinaryOp::Add => x + y,
                BinaryOp::Sub => x - y,
                BinaryOp::Mul => x * y,
                BinaryOp::Div => x / y,
                BinaryOp::Rem => x % y,
                _ => x.powf(y),
            })),
            (Value::F64(x), Value::F64(y)) => Ok(Value::F64(match op {
                BinaryOp::Add => x + y,
                BinaryOp::Sub => x - y,
                BinaryOp::Mul => x * y,
                BinaryOp::Div => x / y,
                BinaryOp::Rem => x % y,
                _ => x.powf(y),
            })),
            (Value::Str(x), Value::Str(y)) => {
                let mut s = x;
                Rc::make_mut(&mut s).push_str(&y);
                Ok(Value::Str(s))
            }
            _ => Err(super::Flow::Fatal(format!(
                "operator {op:?} on mismatched values"
            ))),
        }
    }

    /// `(T) value` for primitive `T` (SPEC "Casts and conversions").
    pub(crate) fn cast(&mut self, v: Value, to: Ty, span: Span) -> R<Value> {
        let kind = self.program.types.kind(to).clone();
        match kind {
            TyKind::Int(t) => {
                let n = if let Some(n) = v.as_i128() {
                    n
                } else {
                    let f = v.as_f64().unwrap_or(f64::NAN);
                    if !f.is_finite() {
                        return Err(self.error(
                            "CastError",
                            format!(
                                "cannot convert {} to `{}`",
                                format_float(f, false),
                                int_name(t)
                            ),
                            span,
                        ));
                    }
                    let tr = f.trunc();
                    if tr < t.min() as f64 || tr > t.max() as f64 {
                        return Err(self.error(
                            "CastError",
                            format!(
                                "{} does not fit in `{}`",
                                format_float(f, false),
                                int_name(t)
                            ),
                            span,
                        ));
                    }
                    tr as i128
                };
                if n < t.min() || n > t.max() {
                    return Err(self.error(
                        "CastError",
                        format!("{n} does not fit in `{}`", int_name(t)),
                        span,
                    ));
                }
                Ok(Value::int(t, n))
            }
            TyKind::Float(f) => {
                let x = match v.as_i128() {
                    Some(n) => n as f64,
                    None => v.as_f64().unwrap_or(f64::NAN),
                };
                Ok(Value::float(f, x))
            }
            _ => Ok(v),
        }
    }

    /// `==` (SPEC "Equality and comparison"): structural, IEEE for floats, user
    /// `equals()` for classes implementing `Equatable`.
    pub fn eq(&mut self, a: &Value, b: &Value, span: Span) -> R<bool> {
        Ok(match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                if Rc::ptr_eq(x, y) && !self.implements(a, "Equatable") {
                    return Ok(true);
                }
                if self.implements(a, "Equatable") {
                    return Ok(self
                        .call_method(a.clone(), "equals", vec![b.clone()], span)?
                        .as_bool());
                }
                Rc::ptr_eq(x, y)
            }
            (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
                if x.len() != y.len() {
                    return Ok(false);
                }
                for (p, q) in x.iter().zip(y.iter()) {
                    if !self.eq(p, q, span)? {
                        return Ok(false);
                    }
                }
                true
            }
            (Value::Struct(x), Value::Struct(y)) => {
                if x.decl != y.decl {
                    return Ok(false);
                }
                if self.implements(a, "Equatable") {
                    return Ok(self
                        .call_method(a.clone(), "equals", vec![b.clone()], span)?
                        .as_bool());
                }
                for (p, q) in x.fields.iter().zip(&y.fields) {
                    if !self.eq(p, q, span)? {
                        return Ok(false);
                    }
                }
                true
            }
            // Keys match structurally; values compare with `==` (user `equals()`).
            (Value::Map(x), Value::Map(y)) => {
                if x.len() != y.len() {
                    return Ok(false);
                }
                for (k, v) in x.iter() {
                    let Some(pos) = y.find(hash_value(k), k, &mut values_equal) else {
                        return Ok(false);
                    };
                    let w = y.get(pos).expect("found").1.clone();
                    if !self.eq(v, &w, span)? {
                        return Ok(false);
                    }
                }
                true
            }
            (Value::Enum(x), Value::Enum(y)) => {
                if x.decl != y.decl || x.case != y.case {
                    return Ok(false);
                }
                for (p, q) in x.payload.iter().zip(&y.payload) {
                    if !self.eq(p, q, span)? {
                        return Ok(false);
                    }
                }
                true
            }
            _ => values_equal(a, b),
        })
    }

    fn implements(&self, v: &Value, iface: &str) -> bool {
        let Some(d) = v.decl() else { return false };
        let decls = &self.program.decls;
        decls
            .type_named(iface)
            .is_some_and(|i| decls.all_interfaces(d).contains(&i))
    }

    /// `<=>` (SPEC: `totalOrder` for floats, byte order for strings, lexicographic for
    /// lists and tuples, case order then payload for enums, `compareTo()` for classes
    /// and structs implementing `Comparable`).
    pub fn compare(&mut self, a: &Value, b: &Value, span: Span) -> R<Ordering> {
        if let (Some(x), Some(y)) = (a.as_i128(), b.as_i128()) {
            return Ok(x.cmp(&y));
        }
        Ok(match (a, b) {
            (Value::F32(x), Value::F32(y)) => x.total_cmp(y),
            (Value::F64(x), Value::F64(y)) => x.total_cmp(y),
            (Value::Str(x), Value::Str(y)) => x.as_bytes().cmp(y.as_bytes()),
            (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
            (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
            (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
                for (p, q) in x.iter().zip(y.iter()) {
                    let o = self.compare(p, q, span)?;
                    if o != Ordering::Equal {
                        return Ok(o);
                    }
                }
                x.len().cmp(&y.len())
            }
            (Value::Enum(x), Value::Enum(y)) if !self.implements_compare_method(a) => {
                let o = x.case.cmp(&y.case);
                if o != Ordering::Equal {
                    return Ok(o);
                }
                for (p, q) in x.payload.iter().zip(&y.payload) {
                    let o = self.compare(p, q, span)?;
                    if o != Ordering::Equal {
                        return Ok(o);
                    }
                }
                Ordering::Equal
            }
            _ if a.decl().is_some() => {
                let r = self.call_method(a.clone(), "compareTo", vec![b.clone()], span)?;
                r.as_i64().cmp(&0)
            }
            _ => Ordering::Equal,
        })
    }

    fn implements_compare_method(&self, v: &Value) -> bool {
        v.decl().is_some_and(|d| {
            self.program
                .decls
                .find_method(d, "compareTo")
                .is_some_and(|m| self.program.decls.method(m).has_body)
        })
    }

    /// A value as text (`echo`, interpolation, `(string)`): SPEC "Casts and
    /// conversions"; `Stringable` objects call `toString()`.
    pub fn display(&mut self, v: &Value) -> R<String> {
        Ok(match v {
            Value::Str(s) => s.to_string(),
            Value::F32(f) => format_float(f64::from(*f), true),
            Value::F64(f) => format_float(*f, false),
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
            Value::Struct(_) | Value::Enum(_) | Value::Object(_) => {
                let span = self
                    .iso
                    .calls
                    .last()
                    .map_or_else(|| self.program.funcs[0].span, |c| c.1);
                let s = self.call_method(v.clone(), "toString", Vec::new(), span)?;
                s.as_str().to_string()
            }
            other => match other.as_i128() {
                Some(n) => n.to_string(),
                None => String::new(),
            },
        })
    }
}

fn int_name(t: IntTy) -> &'static str {
    if t == IntTy::I64 { "int" } else { t.name() }
}

fn op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::IntDiv => "~/",
        BinaryOp::Rem => "%",
        BinaryOp::Pow => "**",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
        _ => "op",
    }
}
