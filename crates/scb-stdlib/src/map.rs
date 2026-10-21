//! `map<K, V>` and `set<T>` (SPEC "Core library"). Both keep insertion order; map
//! callbacks receive `($value, $key)`.

use std::rc::Rc;

use scb_runtime::interp::{hash_value, values_equal};
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

fn lookup(m: &OrderedMap, k: &Value) -> Option<usize> {
    m.find(hash_value(k), k, &mut values_equal)
}

fn count(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::I64(storage(c).len() as i64))
}

fn is_empty(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(storage(c).is_empty()))
}

fn has(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(Value::Bool(lookup(&storage(c), &c.args[0]).is_some()))
}

fn get(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let m = storage(c);
    Ok(lookup(&m, &c.args[0]).map_or(Value::Null, |p| m.get(p).expect("found").1.clone()))
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

fn remove(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let k = c.args[0].clone();
    Ok(storage_mut(c)
        .remove(hash_value(&k), &k, &mut values_equal)
        .unwrap_or(Value::Null))
}

fn clear(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    storage_mut(c).clear();
    Ok(Value::Null)
}

/// SPEC: the right side wins; keys are never renumbered.
fn merge(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let mut out = (*storage(c)).clone();
    if let Value::Map(other) = &c.args[0] {
        for (k, v) in other.iter() {
            out.insert(hash_value(k), k.clone(), v.clone(), &mut values_equal);
        }
    }
    Ok(Value::Map(Rc::new(out)))
}

fn filter(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[0].clone(), c.span);
    let mut out = OrderedMap::new();
    for (k, v) in storage(c).iter() {
        if it
            .call_value(&f, vec![v.clone(), k.clone()], span)?
            .as_bool()
        {
            out.insert(hash_value(k), k.clone(), v.clone(), &mut values_equal);
        }
    }
    Ok(Value::Map(Rc::new(out)))
}

fn map_values(it: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let (f, span) = (c.args[0].clone(), c.span);
    let mut out = OrderedMap::new();
    for (k, v) in storage(c).iter() {
        let nv = it.call_value(&f, vec![v.clone(), k.clone()], span)?;
        out.insert(hash_value(k), k.clone(), nv, &mut values_equal);
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

fn set_add(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let v = c.args[0].clone();
    storage_mut(c).insert(hash_value(&v), v, Value::Null, &mut values_equal);
    Ok(Value::Null)
}

fn set_remove(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let v = c.args[0].clone();
    storage_mut(c).remove(hash_value(&v), &v, &mut values_equal);
    Ok(Value::Null)
}

fn other_set(c: &NativeCall) -> Rc<OrderedMap> {
    match &c.args[0] {
        Value::Set(m) => m.clone(),
        _ => Rc::new(OrderedMap::new()),
    }
}

fn set_union(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    let mut out = (*storage(c)).clone();
    for (k, _) in other_set(c).iter() {
        out.insert(hash_value(k), k.clone(), Value::Null, &mut values_equal);
    }
    Ok(Value::Set(Rc::new(out)))
}

fn set_keep(c: &NativeCall, keep_common: bool) -> Value {
    let other = other_set(c);
    let mut out = OrderedMap::new();
    for (k, _) in storage(c).iter() {
        if lookup(&other, k).is_some() == keep_common {
            out.insert(hash_value(k), k.clone(), Value::Null, &mut values_equal);
        }
    }
    Value::Set(Rc::new(out))
}

fn set_intersect(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(set_keep(c, true))
}

fn set_diff(_: &mut Interp<'_>, c: &mut NativeCall) -> R<Value> {
    Ok(set_keep(c, false))
}
