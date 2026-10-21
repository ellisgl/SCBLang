//! `string` (SPEC "Strings", "Core library": views, search, transform, edge cases).
//!
//! Offsets are byte offsets; slicing or searching from a position inside a UTF-8
//! sequence traps with `BoundaryError`. Padding lengths count graphemes.

use std::rc::Rc;

use scb_runtime::value::Value;
use scb_runtime::{Interp, NativeCall, Natives, R};
use unicode_segmentation::UnicodeSegmentation;

use crate::util::{int, range_arg, receiver_str, slice_bounds, string};

pub fn register(n: &mut Natives) {
    let fns: &[(&str, scb_runtime::NativeFn)] = &[
        ("bytes", bytes),
        ("codepoints", codepoints),
        ("graphemes", graphemes),
        ("byteLength", byte_length),
        ("codepointLength", codepoint_length),
        ("graphemeLength", grapheme_length),
        ("contains", contains),
        ("startsWith", starts_with),
        ("endsWith", ends_with),
        ("find", find),
        ("replace", replace),
        ("slice", slice),
        ("split", split),
        ("trim", trim),
        ("trimStart", trim_start),
        ("trimEnd", trim_end),
        ("toUpper", to_upper),
        ("toLower", to_lower),
        ("repeat", repeat),
        ("padStart", pad_start),
        ("padEnd", pad_end),
        ("isEmpty", is_empty),
    ];
    for (name, f) in fns {
        n.insert(format!("string.{name}"), *f);
    }
}

fn s(c: &NativeCall) -> String {
    receiver_str(c).to_string()
}

fn bytes(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bytes(Rc::new(receiver_str(c).as_bytes().to_vec())))
}

fn codepoints(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::list(
        receiver_str(c)
            .chars()
            .map(|ch| Value::U32(ch as u32))
            .collect(),
    ))
}

fn graphemes(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::list(
        receiver_str(c).graphemes(true).map(Value::str).collect(),
    ))
}

fn byte_length(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::I64(receiver_str(c).len() as i64))
}

fn codepoint_length(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::I64(receiver_str(c).chars().count() as i64))
}

fn grapheme_length(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::I64(receiver_str(c).graphemes(true).count() as i64))
}

fn contains(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(receiver_str(c).contains(string(c, 0))))
}

fn starts_with(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(receiver_str(c).starts_with(string(c, 0))))
}

fn ends_with(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(receiver_str(c).ends_with(string(c, 0))))
}

/// A byte offset into `text` that must be a character boundary.
fn boundary(it: &mut Interp<'_>, text: &str, at: i64, c: &NativeCall) -> R<usize> {
    let Some(pos) = usize::try_from(at).ok().filter(|&p| p <= text.len()) else {
        return Err(it.error(
            "IndexError",
            format!("offset {at} is outside 0..={}", text.len()),
            c.span,
        ));
    };
    if !text.is_char_boundary(pos) {
        return Err(it.error(
            "BoundaryError",
            format!("offset {pos} is inside a character"),
            c.span,
        ));
    }
    Ok(pos)
}

/// SPEC: `$offset` past the end traps (`IndexError`), off a boundary traps
/// (`BoundaryError`); an empty needle matches at `$offset`.
fn find(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let text = s(c);
    let from = boundary(it, &text, int(c, 1), c)?;
    Ok(text[from..]
        .find(string(c, 0))
        .map_or(Value::Null, |i| Value::I64((from + i) as i64)))
}

fn replace(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let needle = string(c, 0);
    let text = s(c);
    // SPEC: an empty needle traps, like an empty `split()` separator.
    if needle.is_empty() {
        return Err(it.error("ValueError", "`replace()` needs a non-empty needle", c.span));
    }
    Ok(Value::str(text.replace(needle, string(c, 1))))
}

fn slice(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let text = s(c);
    let r = range_arg(c, 0);
    let (a, b) = slice_bounds(it, &r, text.len(), c)?;
    for p in [a, b] {
        if !text.is_char_boundary(p) {
            return Err(it.error(
                "BoundaryError",
                format!("offset {p} is inside a character"),
                c.span,
            ));
        }
    }
    Ok(Value::str(&text[a..b]))
}

/// SPEC: `$limit` is the maximum number of parts (`-1` = no limit); `0` or below
/// `-1` traps, as does an empty separator.
fn split(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let sep = string(c, 0).to_string();
    let limit = int(c, 1);
    if sep.is_empty() {
        return Err(it.error(
            "ValueError",
            "`split()` needs a non-empty separator (use `graphemes()`)",
            c.span,
        ));
    }
    if limit == 0 || limit < -1 {
        return Err(it.error(
            "ValueError",
            format!("`split()` limit must be positive or -1, got {limit}"),
            c.span,
        ));
    }
    let text = s(c);
    let parts: Vec<Value> = if limit == -1 {
        text.split(sep.as_str()).map(Value::str).collect()
    } else {
        text.splitn(limit as usize, sep.as_str())
            .map(Value::str)
            .collect()
    };
    Ok(Value::list(parts))
}

fn trim(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::str(receiver_str(c).trim()))
}

fn trim_start(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::str(receiver_str(c).trim_start()))
}

fn trim_end(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::str(receiver_str(c).trim_end()))
}

fn to_upper(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::str(receiver_str(c).to_uppercase()))
}

fn to_lower(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::str(receiver_str(c).to_lowercase()))
}

fn repeat(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let n = int(c, 0);
    if n < 0 {
        return Err(it.error(
            "ValueError",
            format!("`repeat()` count must not be negative, got {n}"),
            c.span,
        ));
    }
    let text = receiver_str(c);
    // `str::repeat` panics when the size overflows; no allocation could succeed.
    if text
        .len()
        .checked_mul(n as usize)
        .is_none_or(|b| b > isize::MAX as usize)
    {
        return Err(scb_runtime::Flow::Fatal(format!(
            "out of memory: `repeat({n})` of a {}-byte string",
            text.len()
        )));
    }
    Ok(Value::str(text.repeat(n as usize)))
}

/// The padding to reach `len` graphemes: `$pad` repeated and cut at a grapheme.
fn padding(it: &mut Interp<'_>, c: &NativeCall) -> R<String> {
    let pad = string(c, 1);
    if pad.is_empty() {
        return Err(it.error("ValueError", "padding must not be empty", c.span));
    }
    let have = receiver_str(c).graphemes(true).count() as i64;
    let missing = int(c, 0).saturating_sub(have).max(0) as usize;
    Ok(pad.graphemes(true).cycle().take(missing).collect())
}

fn pad_start(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let p = padding(it, c)?;
    Ok(Value::str(format!("{p}{}", receiver_str(c))))
}

fn pad_end(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let p = padding(it, c)?;
    Ok(Value::str(format!("{}{p}", receiver_str(c))))
}

fn is_empty(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(receiver_str(c).is_empty()))
}
