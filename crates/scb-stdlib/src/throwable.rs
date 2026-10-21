//! `Throwable` (SPEC "Errors"): the constructor and the final accessors.

use scb_runtime::value::{NativeState, ThrowableState, Value};
use scb_runtime::{Interp, NativeCall, Natives, R};

pub fn register(n: &mut Natives) {
    let fns: &[(&str, scb_runtime::NativeFn)] = &[
        ("__construct", construct),
        ("getMessage", get_message),
        ("getCode", get_code),
        ("getPrevious", get_previous),
        ("getFile", get_file),
        ("getLine", get_line),
    ];
    for (name, f) in fns {
        n.insert(format!("Throwable.{name}"), *f);
    }
}

fn with_state<T>(c: &NativeCall, f: impl FnOnce(&mut ThrowableState) -> T) -> Option<T> {
    let Value::Object(o) = &c.receiver else {
        return None;
    };
    match &mut o.borrow_mut().native {
        Some(NativeState::Throwable(t)) => Some(f(t)),
        _ => None,
    }
}

fn construct(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let message = c.args[0].as_str().to_string();
    let code = c.args[1].as_i64();
    let previous = match &c.args[2] {
        Value::Null => None,
        v => Some(v.clone()),
    };
    with_state(c, |t| {
        t.message = message;
        t.code = code;
        t.previous = previous;
    });
    Ok(Value::Null)
}

fn get_message(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(with_state(c, |t| Value::str(t.message.clone())).unwrap_or_default())
}

fn get_code(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(with_state(c, |t| Value::I64(t.code)).unwrap_or_default())
}

fn get_previous(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(with_state(c, |t| t.previous.clone().unwrap_or_default()).unwrap_or_default())
}

fn get_file(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(with_state(c, |t| Value::str(t.file.clone())).unwrap_or_default())
}

fn get_line(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(with_state(c, |t| Value::I64(t.line)).unwrap_or_default())
}
