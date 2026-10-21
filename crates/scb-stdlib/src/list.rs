//! `list<T>` (SPEC "Core library": size, read, mutate, new value, query, conditional).

use std::cmp::Ordering;

use scb_runtime::value::Value;
use scb_runtime::{Interp, NativeCall, Natives, R};

use crate::util::{
    int, int_result, list, list_mut, ordering_of, range_arg, slice_bounds, sort_values,
};

pub fn register(n: &mut Natives) {
    let fns: &[(&str, scb_runtime::NativeFn)] = &[
        ("count", count),
        ("isEmpty", is_empty),
        ("get", get),
        ("first", first),
        ("last", last),
        ("contains", contains),
        ("indexOf", index_of),
        ("slice", slice),
        ("push", push),
        ("pop", pop),
        ("insert", insert),
        ("removeAt", remove_at),
        ("swap", swap),
        ("clear", clear),
        ("sort", sort),
        ("sortBy", sort_by),
        ("reverse", reverse),
        ("concat", concat),
        ("sorted", sorted),
        ("sortedBy", sorted_by),
        ("reversed", reversed),
        ("map", map),
        ("filter", filter),
        ("reduce", reduce),
        ("chunk", chunk),
        ("any", any),
        ("all", all),
        ("find", find),
        ("findIndex", find_index),
        ("each", each),
        ("join", join),
        ("sum", sum),
        ("min", min),
        ("max", max),
    ];
    for (name, f) in fns {
        n.insert(format!("list.{name}"), *f);
    }
}

fn count(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::I64(list(c).len() as i64))
}

fn is_empty(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(list(c).is_empty()))
}

fn get(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let l = list(c);
    Ok(usize::try_from(int(c, 0))
        .ok()
        .and_then(|i| l.get(i).cloned())
        .unwrap_or(Value::Null))
}

fn first(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(list(c).first().cloned().unwrap_or(Value::Null))
}

fn last(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(list(c).last().cloned().unwrap_or(Value::Null))
}

fn position(it: &mut Interp<'_>, c: &NativeCall) -> R<Option<usize>> {
    let l = list(c);
    for (i, x) in l.iter().enumerate() {
        if it.eq(x, &c.args[0], c.span)? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

fn contains(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(position(it, c)?.is_some()))
}

fn index_of(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(position(it, c)?.map_or(Value::Null, |i| Value::I64(i as i64)))
}

fn slice(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let l = list(c);
    let r = range_arg(c, 0);
    let (s, e) = slice_bounds(it, &r, l.len(), c)?;
    Ok(Value::list(l[s..e].to_vec()))
}

fn push(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let v = std::mem::take(&mut c.args[0]);
    list_mut(c).push(v);
    Ok(Value::Null)
}

fn pop(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(list_mut(c).pop().unwrap_or(Value::Null))
}

/// `i` as a position below `limit` in a list of `len` elements.
fn index(it: &mut Interp<'_>, c: &NativeCall, i: i64, limit: usize, len: usize) -> R<usize> {
    usize::try_from(i)
        .ok()
        .filter(|&i| i < limit)
        .ok_or_else(|| {
            it.error(
                "IndexError",
                format!("index {i} is out of range (size {len})"),
                c.span,
            )
        })
}

fn insert(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let len = list(c).len();
    let i = int(c, 0);
    // Inserting at the end is allowed.
    let pos = index(it, c, i, len + 1, len)?;
    let v = std::mem::take(&mut c.args[1]);
    list_mut(c).insert(pos, v);
    Ok(Value::Null)
}

fn remove_at(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let len = list(c).len();
    let pos = index(it, c, int(c, 0), len, len)?;
    Ok(list_mut(c).remove(pos))
}

fn swap(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let len = list(c).len();
    let a = index(it, c, int(c, 0), len, len)?;
    let b = index(it, c, int(c, 1), len, len)?;
    list_mut(c).swap(a, b);
    Ok(Value::Null)
}

fn clear(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    list_mut(c).clear();
    Ok(Value::Null)
}

fn natural(it: &mut Interp<'_>, c: &NativeCall, items: Vec<Value>) -> R<Vec<Value>> {
    let span = c.span;
    sort_values(it, items, &mut |it, a, b| it.compare(a, b, span))
}

fn by(it: &mut Interp<'_>, c: &NativeCall, items: Vec<Value>) -> R<Vec<Value>> {
    let (f, span) = (c.args[0].clone(), c.span);
    sort_values(it, items, &mut |it, a, b| {
        Ok(ordering_of(&it.call_value(
            &f,
            vec![a.clone(), b.clone()],
            span,
        )?))
    })
}

/// The sort works on a copy of the element handles and stores the result only on
/// success: a comparison that throws leaves the list unchanged.
fn sort(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let items = list(c).to_vec();
    let sorted = natural(it, c, items)?;
    *list_mut(c) = sorted;
    Ok(Value::Null)
}

fn sort_by(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let items = list(c).to_vec();
    let sorted = by(it, c, items)?;
    *list_mut(c) = sorted;
    Ok(Value::Null)
}

fn reverse(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    list_mut(c).reverse();
    Ok(Value::Null)
}

fn concat(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let mut out = list(c).to_vec();
    if let Value::List(other) = &c.args[0] {
        out.extend(other.iter().cloned());
    }
    Ok(Value::list(out))
}

fn sorted(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let items = list(c).to_vec();
    Ok(Value::list(natural(it, c, items)?))
}

fn sorted_by(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let items = list(c).to_vec();
    Ok(Value::list(by(it, c, items)?))
}

fn reversed(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let mut out = list(c).to_vec();
    out.reverse();
    Ok(Value::list(out))
}

fn map(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[0].clone(), c.span);
    let mut out = Vec::new();
    for x in list(c).iter() {
        out.push(it.call_value(&f, vec![x.clone()], span)?);
    }
    Ok(Value::list(out))
}

fn filter(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[0].clone(), c.span);
    let mut out = Vec::new();
    for x in list(c).iter() {
        if it.call_value(&f, vec![x.clone()], span)?.as_bool() {
            out.push(x.clone());
        }
    }
    Ok(Value::list(out))
}

fn reduce(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[1].clone(), c.span);
    let mut acc = c.args[0].clone();
    for x in list(c).iter() {
        acc = it.call_value(&f, vec![acc, x.clone()], span)?;
    }
    Ok(acc)
}

fn chunk(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let size = int(c, 0);
    if size <= 0 {
        return Err(it.error(
            "ValueError",
            format!("chunk size must be positive, got {size}"),
            c.span,
        ));
    }
    let chunks = list(c)
        .chunks(size as usize)
        .map(|ch| Value::list(ch.to_vec()))
        .collect();
    Ok(Value::list(chunks))
}

/// Runs the predicate over the elements until `stop` says so; returns the position.
fn scan(it: &mut Interp<'_>, c: &NativeCall, stop: bool) -> R<Option<usize>> {
    let (f, span) = (c.args[0].clone(), c.span);
    for (i, x) in list(c).iter().enumerate() {
        if it.call_value(&f, vec![x.clone()], span)?.as_bool() == stop {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

fn any(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(scan(it, c, true)?.is_some()))
}

fn all(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(scan(it, c, false)?.is_none()))
}

fn find(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(scan(it, c, true)?.map_or(Value::Null, |i| list(c)[i].clone()))
}

fn find_index(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(scan(it, c, true)?.map_or(Value::Null, |i| Value::I64(i as i64)))
}

fn each(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[0].clone(), c.span);
    for x in list(c).iter() {
        it.call_value(&f, vec![x.clone()], span)?;
    }
    Ok(Value::Null)
}

fn join(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let sep = c.args[0].as_str().to_string();
    let mut out = String::new();
    for (i, x) in list(c).iter().enumerate() {
        if i > 0 {
            out.push_str(&sep);
        }
        out.push_str(&it.display(x)?);
    }
    Ok(Value::str(out))
}

/// SPEC: `0` / `0.0` for an empty list; integer overflow traps.
fn sum(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let l = list(c);
    let mut acc = match l.first() {
        Some(_) => None,
        None => {
            let ty = crate::util::result_ty(it, c);
            return Ok(match it.program.types.as_float(ty) {
                Some(f) => Value::float(f, 0.0),
                None => Value::int(int_result(it, c), 0),
            });
        }
    };
    for x in l.iter() {
        acc = Some(match acc {
            None => x.clone(),
            Some(a) => it.binary(scb_types::tir::BinaryOp::Add, a, x.clone(), c.span)?,
        });
    }
    Ok(acc.unwrap_or_default())
}

fn extreme(it: &mut Interp<'_>, c: &NativeCall, want: Ordering) -> R<Value> {
    let l = list(c);
    let mut best: Option<Value> = None;
    for x in l.iter() {
        best = Some(match best {
            None => x.clone(),
            Some(b) => {
                if it.compare(x, &b, c.span)? == want {
                    x.clone()
                } else {
                    b
                }
            }
        });
    }
    Ok(best.unwrap_or(Value::Null))
}

fn min(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    extreme(it, c, Ordering::Less)
}

fn max(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    extreme(it, c, Ordering::Greater)
}
