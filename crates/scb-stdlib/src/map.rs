//! `map<K, V>` and `set<T>` (SPEC "Core library"). Both keep insertion order; map
//! callbacks receive `($value, $key)`.
//!
//! Keys are found with the interpreter's key operations (`Interp::key_find` …), which
//! run user `hash()` / `equals()` for keys of a type implementing `Hashable` itself.
//! A new map built from an existing one's entries reuses their stored hashes: its keys
//! are already distinct.

use std::rc::Rc;

use scb_runtime::value::{OrderedMap, Value};
use scb_runtime::{Interp, NativeCall, Natives, R};

pub fn register(n: &mut Natives) {
    let maps: &[(&str, scb_runtime::NativeFn)] = &[
        ("count", count),
        ("isEmpty", is_empty),
        ("has", has),
        ("get", get),
        ("keys", keys),
        ("values", values),
        ("entries", entries),
        ("remove", remove),
        ("clear", clear),
        ("merge", merge),
        ("filter", filter),
        ("mapValues", map_values),
        ("each", each),
        ("any", any),
        ("all", all),
    ];
    for (name, f) in maps {
        n.insert(format!("map.{name}"), *f);
    }
    let sets: &[(&str, scb_runtime::NativeFn)] = &[
        ("add", set_add),
        ("remove", set_remove),
        ("has", has),
        ("count", count),
        ("isEmpty", is_empty),
        ("values", keys),
        ("union", set_union),
        ("intersect", set_intersect),
        ("diff", set_diff),
    ];
    for (name, f) in sets {
        n.insert(format!("set.{name}"), *f);
    }
}

fn storage(c: &NativeCall) -> Rc<OrderedMap> {
    match &c.receiver {
        Value::Map(m) | Value::Set(m) => m.clone(),
        _ => Rc::new(OrderedMap::new()),
    }
}

/// The receiver's storage, unique, for in-place changes.
fn storage_mut(c: &mut NativeCall) -> &mut OrderedMap {
    match &mut c.receiver {
        Value::Map(m) | Value::Set(m) => Rc::make_mut(m),
        other => {
            *other = Value::Map(Rc::new(OrderedMap::new()));
            let Value::Map(m) = other else {
                unreachable!("set above")
            };
            Rc::make_mut(m)
        }
    }
}

fn count(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::I64(storage(c).len() as i64))
}

fn is_empty(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(storage(c).is_empty()))
}

fn has(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(it.key_find(&storage(c), &c.args[0])?.is_some()))
}

fn get(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let m = storage(c);
    Ok(it
        .key_find(&m, &c.args[0])?
        .map_or(Value::Null, |p| m.get(p).expect("found").1.clone()))
}

fn keys(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::list(
        storage(c).iter().map(|(k, _)| k.clone()).collect(),
    ))
}

fn values(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::list(
        storage(c).iter().map(|(_, v)| v.clone()).collect(),
    ))
}

fn entries(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::list(
        storage(c)
            .iter()
            .map(|(k, v)| Value::Tuple(Rc::new(vec![k.clone(), v.clone()])))
            .collect(),
    ))
}

fn remove(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let k = c.args[0].clone();
    Ok(it.map_remove(storage_mut(c), &k)?.unwrap_or(Value::Null))
}

fn clear(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    storage_mut(c).clear();
    Ok(Value::Null)
}

/// SPEC: the right side wins; keys are never renumbered.
fn merge(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let mut out = (*storage(c)).clone();
    if let Value::Map(other) = &c.args[0] {
        for (k, v) in other.iter() {
            it.map_insert(&mut out, k.clone(), v.clone())?;
        }
    }
    Ok(Value::Map(Rc::new(out)))
}

fn filter(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[0].clone(), c.span);
    let mut out = OrderedMap::new();
    for (h, (k, v)) in storage(c).iter_hashed() {
        if it
            .call_value(&f, vec![v.clone(), k.clone()], span)?
            .as_bool()
        {
            out.insert_at(None, h, k.clone(), v.clone());
        }
    }
    Ok(Value::Map(Rc::new(out)))
}

fn map_values(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[0].clone(), c.span);
    let mut out = OrderedMap::new();
    for (h, (k, v)) in storage(c).iter_hashed() {
        let nv = it.call_value(&f, vec![v.clone(), k.clone()], span)?;
        out.insert_at(None, h, k.clone(), nv);
    }
    Ok(Value::Map(Rc::new(out)))
}

fn scan(it: &mut Interp<'_>, c: &NativeCall, stop: bool) -> R<bool> {
    let (f, span) = (c.args[0].clone(), c.span);
    for (k, v) in storage(c).iter() {
        if it
            .call_value(&f, vec![v.clone(), k.clone()], span)?
            .as_bool()
            == stop
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn each(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[0].clone(), c.span);
    for (k, v) in storage(c).iter() {
        it.call_value(&f, vec![v.clone(), k.clone()], span)?;
    }
    Ok(Value::Null)
}

fn any(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(scan(it, c, true)?))
}

fn all(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(!scan(it, c, false)?))
}

// ---------- sets ----------

fn set_add(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let v = c.args[0].clone();
    it.map_insert(storage_mut(c), v, Value::Null)?;
    Ok(Value::Null)
}

fn set_remove(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let v = c.args[0].clone();
    it.map_remove(storage_mut(c), &v)?;
    Ok(Value::Null)
}

fn other_set(c: &NativeCall) -> Rc<OrderedMap> {
    match &c.args[0] {
        Value::Set(m) => m.clone(),
        _ => Rc::new(OrderedMap::new()),
    }
}

fn set_union(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let mut out = (*storage(c)).clone();
    for (k, _) in other_set(c).iter() {
        it.map_insert(&mut out, k.clone(), Value::Null)?;
    }
    Ok(Value::Set(Rc::new(out)))
}

fn set_keep(it: &mut Interp<'_>, c: &NativeCall, keep_common: bool) -> R<Value> {
    let other = other_set(c);
    let mut out = OrderedMap::new();
    for (h, (k, _)) in storage(c).iter_hashed() {
        if it.key_find(&other, k)?.is_some() == keep_common {
            out.insert_at(None, h, k.clone(), Value::Null);
        }
    }
    Ok(Value::Set(Rc::new(out)))
}

fn set_intersect(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    set_keep(it, c, true)
}

fn set_diff(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    set_keep(it, c, false)
}
