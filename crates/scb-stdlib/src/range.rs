//! `Range<T>` (SPEC "Ranges"): an integer sequence; `5..=1` is empty, `reversed()`
//! counts down, `step($n)` keeps every `$n`-th value.

use std::rc::Rc;

use scb_runtime::value::{RangeValue, Value};
use scb_runtime::{Interp, NativeCall, Natives, R};

use crate::util::int;

pub fn register(n: &mut Natives) {
    let fns: &[(&str, scb_runtime::NativeFn)] = &[
        ("contains", contains),
        ("count", count),
        ("toList", to_list),
        ("reversed", reversed),
        ("step", step),
    ];
    for (name, f) in fns {
        n.insert(format!("Range.{name}"), *f);
    }
}

fn range(c: &NativeCall) -> RangeValue {
    match &c.receiver {
        Value::Range(r) => (**r).clone(),
        _ => RangeValue::new(0, 0, false, scb_types::ty::IntTy::I64),
    }
}

fn contains(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(
        c.args[0].as_i128().is_some_and(|v| range(c).contains(v)),
    ))
}

/// SPEC: counts are `int`; a `u64` range can hold more values than that.
fn count(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let n = range(c).count;
    match i64::try_from(n) {
        Ok(n) => Ok(Value::I64(n)),
        Err(_) => Err(it.error(
            "OverflowError",
            format!("range has {n} values, more than `int` holds"),
            c.span,
        )),
    }
}

fn to_list(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let r = range(c);
    if r.count > (isize::MAX as usize / std::mem::size_of::<Value>()) as i128 {
        return Err(scb_runtime::Flow::Fatal(format!(
            "out of memory: a list of {} values",
            r.count
        )));
    }
    Ok(Value::list(
        (0..r.count).map(|i| Value::int(r.elem, r.get(i))).collect(),
    ))
}

fn reversed(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let r = range(c);
    let out = if r.count == 0 {
        r
    } else {
        RangeValue {
            first: r.get(r.count - 1),
            step: -r.step,
            ..r
        }
    };
    Ok(Value::Range(Rc::new(out)))
}

/// SPEC: `$n ≤ 0` traps (`ValueError`).
fn step(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let n = i128::from(int(c, 0));
    if n <= 0 {
        return Err(it.error(
            "ValueError",
            format!("`step()` must be positive, got {n}"),
            c.span,
        ));
    }
    let r = range(c);
    let count = (r.count + n - 1) / n;
    // With fewer than two values the step is never used; keeping it bounds it
    // (repeated `step()` calls would otherwise overflow).
    let step = if count < 2 { r.step } else { r.step * n };
    Ok(Value::Range(Rc::new(RangeValue { step, count, ..r })))
}
