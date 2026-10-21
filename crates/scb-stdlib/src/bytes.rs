//! `bytes` (SPEC "Core library": raw data; not `Stringable`).

use std::rc::Rc;

use scb_runtime::value::Value;
use scb_runtime::{Interp, NativeCall, Natives, R};

use crate::util::{range_arg, slice_bounds};

pub fn register(n: &mut Natives) {
    let fns: &[(&str, scb_runtime::NativeFn)] = &[
        ("count", count),
        ("isEmpty", is_empty),
        ("slice", slice),
        ("concat", concat),
        ("decodeUtf8", decode_utf8),
        ("tryDecodeUtf8", try_decode_utf8),
    ];
    for (name, f) in fns {
        n.insert(format!("bytes.{name}"), *f);
    }
}

fn data(c: &NativeCall) -> Rc<Vec<u8>> {
    match &c.receiver {
        Value::Bytes(b) => b.clone(),
        _ => Rc::new(Vec::new()),
    }
}

fn count(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::I64(data(c).len() as i64))
}

fn is_empty(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(data(c).is_empty()))
}

fn slice(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let b = data(c);
    let r = range_arg(c, 0);
    let (s, e) = slice_bounds(it, &r, b.len(), c)?;
    Ok(Value::Bytes(Rc::new(b[s..e].to_vec())))
}

fn concat(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let mut out = data(c).to_vec();
    if let Value::Bytes(other) = &c.args[0] {
        out.extend_from_slice(other);
    }
    Ok(Value::Bytes(Rc::new(out)))
}

fn decode_utf8(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    match String::from_utf8(data(c).to_vec()) {
        Ok(s) => Ok(Value::str(s)),
        Err(e) => Err(it.error(
            "EncodingException",
            format!("invalid UTF-8 at byte {}", e.utf8_error().valid_up_to()),
            c.span,
        )),
    }
}

fn try_decode_utf8(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(String::from_utf8(data(c).to_vec()).map_or(Value::Null, Value::str))
}
