//! Static methods of the numeric types (SPEC "Casts and conversions"): wrapping,
//! saturating and checked arithmetic, conversions and parsing.
//!
//! Each native serves every type: the target type is the call's result type
//! (`NativeCall::ret`), e.g. `i8` for `i8::wrap`.

use scb_runtime::value::Value;
use scb_runtime::{Interp, NativeCall, Natives, R};
use scb_types::ty::IntTy;

use crate::util::{int_result, result_ty};

pub fn register(n: &mut Natives) {
    let ints: &[(&str, scb_runtime::NativeFn)] = &[
        ("wrap", wrap),
        ("saturate", saturate),
        ("tryFrom", try_from),
        ("parse", parse),
        ("tryParse", try_parse),
        ("round", round),
        ("wrappingAdd", wrapping_add),
        ("wrappingSub", wrapping_sub),
        ("wrappingMul", wrapping_mul),
        ("saturatingAdd", saturating_add),
        ("saturatingSub", saturating_sub),
        ("saturatingMul", saturating_mul),
        ("checkedAdd", checked_add),
        ("checkedSub", checked_sub),
        ("checkedMul", checked_mul),
        ("wrappingPow", wrapping_pow),
        ("checkedPow", checked_pow),
        ("wrappingNeg", wrapping_neg),
        ("wrappingShl", wrapping_shl),
        ("wrappingShr", wrapping_shr),
    ];
    for t in IntTy::ALL {
        for (name, f) in ints {
            n.insert(format!("{}::{name}", t.name()), *f);
        }
    }
    let floats: &[(&str, scb_runtime::NativeFn)] = &[
        ("exactFrom", float_exact_from),
        ("parse", float_parse),
        ("tryParse", float_try_parse),
        ("isNan", float_is_nan),
    ];
    for t in ["f32", "f64"] {
        for (name, f) in floats {
            n.insert(format!("{t}::{name}"), *f);
        }
    }
}

/// Two's-complement wrap to `t`'s width.
fn wrapped(t: IntTy, v: i128) -> i128 {
    let bits = t.bits();
    let low = v & ((1i128 << bits) - 1);
    if t.signed() && low >= 1i128 << (bits - 1) {
        low - (1i128 << bits)
    } else {
        low
    }
}

fn arg(c: &NativeCall, i: usize) -> i128 {
    c.args[i].as_i128().unwrap_or(0)
}

fn fits(t: IntTy, v: i128) -> bool {
    (t.min()..=t.max()).contains(&v)
}

fn checked_or_null(t: IntTy, v: Option<i128>) -> Value {
    match v {
        Some(v) if fits(t, v) => Value::int(t, v),
        _ => Value::Null,
    }
}

fn wrap(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    Ok(Value::int(t, wrapped(t, arg(c, 0))))
}

fn saturate(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    Ok(Value::int(t, arg(c, 0).clamp(t.min(), t.max())))
}

fn try_from(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    Ok(checked_or_null(t, Some(arg(c, 0))))
}

/// A decimal integer: an optional sign and digits, nothing else.
fn parse_int(text: &str, t: IntTy) -> Option<i128> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i128 = text.parse().ok()?;
    fits(t, v).then_some(v)
}

fn parse(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    let text = c.args[0].as_str().to_string();
    match parse_int(&text, t) {
        Some(v) => Ok(Value::int(t, v)),
        None => Err(it.error(
            "ParseException",
            format!("'{text}' is not a valid `{}`", t.name()),
            c.span,
        )),
    }
}

fn try_parse(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    Ok(checked_or_null(t, parse_int(c.args[0].as_str(), t)))
}

/// `T::round($f, Math\Rounding)`: the modes are the enum's cases in declaration
/// order (HALF_UP, HALF_EVEN, FLOOR, CEIL, TRUNCATE).
fn round(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    let f = c.args[0].as_f64().unwrap_or(f64::NAN);
    let mode = match &c.args[1] {
        Value::Enum(e) => e.case,
        _ => 1,
    };
    let r = match mode {
        // HALF_UP: halves round away from zero (`f64::round`).
        0 => f.round(),
        1 => f.round_ties_even(),
        2 => f.floor(),
        3 => f.ceil(),
        _ => f.trunc(),
    };
    // Checked on the integer: `t.max() as f64` rounds up (2^63 for `i64`), so a
    // float comparison would let 2^63 through.
    let n = r as i128;
    if !r.is_finite() || !fits(t, n) {
        return Err(it.error(
            "CastError",
            format!("{f} does not fit in `{}`", t.name()),
            c.span,
        ));
    }
    Ok(Value::int(t, n))
}

fn op3(it: &Interp<'_>, c: &NativeCall, f: fn(i128, i128) -> i128) -> (IntTy, i128) {
    (int_result(it, c), f(arg(c, 0), arg(c, 1)))
}

fn wrapping_add(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a + b);
    Ok(Value::int(t, wrapped(t, v)))
}

fn wrapping_sub(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a - b);
    Ok(Value::int(t, wrapped(t, v)))
}

fn wrapping_mul(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a.wrapping_mul(b));
    Ok(Value::int(t, wrapped(t, v)))
}

fn saturating_add(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a + b);
    Ok(Value::int(t, v.clamp(t.min(), t.max())))
}

fn saturating_sub(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a - b);
    Ok(Value::int(t, v.clamp(t.min(), t.max())))
}

fn saturating_mul(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a.saturating_mul(b));
    Ok(Value::int(t, v.clamp(t.min(), t.max())))
}

fn checked_add(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a + b);
    Ok(checked_or_null(t, Some(v)))
}

fn checked_sub(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a - b);
    Ok(checked_or_null(t, Some(v)))
}

fn checked_mul(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (t, v) = op3(it, c, |a, b| a.saturating_mul(b));
    Ok(checked_or_null(t, Some(v)))
}

/// `a ** n` by squaring; `wrap` reduces after every step. `None` when a checked
/// power leaves the type.
fn power(t: IntTy, base: i128, mut n: i128, wrap_steps: bool) -> Option<i128> {
    let step = |v: i128| -> Option<i128> {
        if wrap_steps {
            Some(wrapped(t, v))
        } else {
            fits(t, v).then_some(v)
        }
    };
    let (mut result, mut b) = (1i128, base);
    while n > 0 {
        if n & 1 == 1 {
            // Wrapping keeps the low bits, which is all a wrapped result needs.
            let product = if wrap_steps {
                result.wrapping_mul(b)
            } else {
                result.checked_mul(b)?
            };
            result = step(product)?;
        }
        n >>= 1;
        if n > 0 {
            b = if wrap_steps {
                wrapped(t, b.wrapping_mul(b))
            } else {
                step(b.checked_mul(b)?)?
            };
        }
    }
    Some(result)
}

fn wrapping_pow(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    Ok(Value::int(
        t,
        power(t, arg(c, 0), arg(c, 1), true).unwrap_or(0),
    ))
}

fn checked_pow(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    Ok(checked_or_null(t, power(t, arg(c, 0), arg(c, 1), false)))
}

fn wrapping_neg(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    Ok(Value::int(t, wrapped(t, -arg(c, 0))))
}

/// The shift amount is masked to the width (SPEC: `wrappingShl` / `wrappingShr`).
fn wrapping_shl(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    let n = arg(c, 1) % i128::from(t.bits());
    Ok(Value::int(t, wrapped(t, arg(c, 0) << n)))
}

fn wrapping_shr(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = int_result(it, c);
    let n = arg(c, 1) % i128::from(t.bits());
    Ok(Value::int(t, arg(c, 0) >> n))
}

// ---------- floats ----------

fn float_target(it: &Interp<'_>, c: &NativeCall) -> scb_types::ty::FloatTy {
    it.program
        .types
        .as_float(result_ty(it, c))
        .unwrap_or(scb_types::ty::FloatTy::F64)
}

/// `f64::exactFrom($i)`: `null` unless the integer converts without rounding.
fn float_exact_from(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = float_target(it, c);
    let i = arg(c, 0);
    let exact = match t {
        scb_types::ty::FloatTy::F32 => (i as f32) as i128 == i,
        scb_types::ty::FloatTy::F64 => (i as f64) as i128 == i,
    };
    Ok(if exact {
        Value::float(t, i as f64)
    } else {
        Value::Null
    })
}

/// A float of type `t`, parsed at its own precision (an `f32` parsed through `f64`
/// could round twice).
fn parse_float(text: &str, t: scb_types::ty::FloatTy) -> Option<Value> {
    // Same spelling as float literals: a leading digit, an optional sign.
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    if !body.starts_with(|ch: char| ch.is_ascii_digit()) {
        return None;
    }
    match t {
        scb_types::ty::FloatTy::F32 => text.parse::<f32>().ok().map(Value::F32),
        scb_types::ty::FloatTy::F64 => text.parse::<f64>().ok().map(Value::F64),
    }
}

fn float_parse(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = float_target(it, c);
    let text = c.args[0].as_str().to_string();
    match parse_float(&text, t) {
        Some(v) => Ok(v),
        None => Err(it.error(
            "ParseException",
            format!("'{text}' is not a valid float"),
            c.span,
        )),
    }
}

fn float_try_parse(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let t = float_target(it, c);
    Ok(parse_float(c.args[0].as_str(), t).unwrap_or(Value::Null))
}

fn float_is_nan(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(c.args[0].as_f64().is_some_and(f64::is_nan)))
}
