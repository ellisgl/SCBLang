//! Helpers shared by the native functions.

use std::cmp::Ordering;
use std::rc::Rc;

use scb_runtime::value::{RangeValue, Value};
use scb_runtime::{Interp, NativeCall, R};
use scb_types::ty::{Ty, TyKind};

/// An argument as `i64` (counts, indexes, sizes).
pub fn int(call: &NativeCall, i: usize) -> i64 {
    call.args[i].as_i64()
}

pub fn string(call: &NativeCall, i: usize) -> &str {
    call.args[i].as_str()
}

/// The receiver as a list (an empty one if it is not a list, which the checker rules
/// out).
pub fn list(call: &NativeCall) -> Rc<Vec<Value>> {
    match &call.receiver {
        Value::List(l) => l.clone(),
        _ => Rc::new(Vec::new()),
    }
}

/// The receiver list, unique, for in-place changes (`mutating` methods: the
/// interpreter moved it out of its variable, so this does not copy).
pub fn list_mut(call: &mut NativeCall) -> &mut Vec<Value> {
    if !matches!(call.receiver, Value::List(_)) {
        call.receiver = Value::list(Vec::new());
    }
    match &mut call.receiver {
        Value::List(l) => Rc::make_mut(l),
        _ => unreachable!("set above"),
    }
}

pub fn receiver_str(call: &NativeCall) -> &str {
    call.receiver.as_str()
}

/// The non-null part of a call's result type (`?i8` → `i8`).
pub fn result_ty(it: &Interp<'_>, call: &NativeCall) -> Ty {
    let types = &it.program.types;
    match types.kind(call.ret) {
        TyKind::Union(ms) => ms
            .iter()
            .copied()
            .find(|&m| m != Ty::NULL)
            .unwrap_or(call.ret),
        _ => call.ret,
    }
}

pub fn int_result(it: &Interp<'_>, call: &NativeCall) -> scb_types::ty::IntTy {
    let t = result_ty(it, call);
    it.program
        .types
        .as_int(t)
        .unwrap_or(scb_types::ty::IntTy::I64)
}

/// Positions `[start, end)` of `range` in a sequence of `len` elements (SPEC
/// "Ranges": an empty range slices to empty; bounds outside the sequence trap).
pub fn slice_bounds(
    it: &mut Interp<'_>,
    r: &RangeValue,
    len: usize,
    call: &NativeCall,
) -> R<(usize, usize)> {
    if r.count == 0 {
        return Ok((0, 0));
    }
    if r.step != 1 {
        return Err(it.error(
            "ValueError",
            "`slice()` takes a contiguous, ascending range",
            call.span,
        ));
    }
    let start = r.first;
    let end = r.first + r.count;
    if start < 0 || end > len as i128 {
        return Err(it.error(
            "IndexError",
            format!("range {start}..<{end} is outside 0..<{len}"),
            call.span,
        ));
    }
    Ok((start as usize, end as usize))
}

pub fn range_arg(call: &NativeCall, i: usize) -> RangeValue {
    match &call.args[i] {
        Value::Range(r) => (**r).clone(),
        _ => RangeValue::new(0, 0, false, scb_types::ty::IntTy::I64),
    }
}

/// Stable merge sort with a comparison that may fail (it can run SCB code). Unlike
/// `slice::sort_by`, an inconsistent comparator cannot make it panic.
pub fn sort_values(
    it: &mut Interp<'_>,
    items: Vec<Value>,
    cmp: &mut dyn FnMut(&mut Interp<'_>, &Value, &Value) -> R<Ordering>,
) -> R<Vec<Value>> {
    if items.len() <= 1 {
        return Ok(items);
    }
    let mut items = items;
    let right = items.split_off(items.len() / 2);
    let left = sort_values(it, items, cmp)?;
    let right = sort_values(it, right, cmp)?;
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
    while let (Some(a), Some(b)) = (l.peek(), r.peek()) {
        if cmp(it, b, a)? == Ordering::Less {
            out.push(r.next().expect("peeked"));
        } else {
            out.push(l.next().expect("peeked"));
        }
    }
    out.extend(l);
    out.extend(r);
    Ok(out)
}

/// A comparator callback's `int` result as an ordering (negative, zero, positive).
pub fn ordering_of(v: &Value) -> Ordering {
    v.as_i64().cmp(&0)
}
