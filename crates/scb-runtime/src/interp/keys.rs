//! Map keys and set elements (SPEC "Equality and comparison": `Hashable` governs map
//! keys and set members).
//!
//! Most keys (integers, strings, tuples of them, structs and enums that derive
//! `Hashable`) are hashed and compared structurally, without running any SCB code:
//! [`super::hash_value`] and [`super::values_equal`]. A key that contains a value whose
//! type implements `Hashable` itself (a class, or a struct or enum with its own
//! `hash()` / `equals()`), anywhere inside it, is hashed with that value's `hash()`
//! and compared with its `equals()`. Both may throw, so every operation here returns
//! `R`; and both run user code, so they never run while a map is borrowed for writing
//! (writes find the position first, then change the map at that position).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use scb_syntax::Span;
use scb_types::decls::DeclId;

use super::{Interp, R};
use crate::value::{OrderedMap, Value};

impl Interp<'_> {
    /// Whether `d` implements `Hashable` itself (its own `hash()` / `equals()`), rather
    /// than deriving it.
    fn user_hashed(&mut self, d: DeclId) -> bool {
        if let Some(&b) = self.iso.user_hashed.get(&d) {
            return b;
        }
        let decls = &self.program.decls;
        let b = decls
            .type_named("Hashable")
            .is_some_and(|h| decls.all_interfaces(d).contains(&h));
        self.iso.user_hashed.insert(d, b);
        b
    }

    /// Whether hashing or comparing `v` as a key runs user code.
    pub fn needs_user_key(&mut self, v: &Value) -> bool {
        match v {
            Value::Struct(s) => {
                self.user_hashed(s.decl) || s.fields.iter().any(|f| self.needs_user_key(f))
            }
            Value::Enum(e) => {
                self.user_hashed(e.decl) || e.payload.iter().any(|p| self.needs_user_key(p))
            }
            // A class instance is a key only when its class implements `Hashable`.
            Value::Object(o) => {
                let d = o.borrow().decl;
                self.user_hashed(d)
            }
            Value::List(xs) | Value::Tuple(xs) => xs.iter().any(|x| self.needs_user_key(x)),
            Value::Map(m) | Value::Set(m) => m
                .iter()
                .any(|(k, v)| self.needs_user_key(k) || self.needs_user_key(v)),
            _ => false,
        }
    }

    /// The hash of a key: structural, or through user `hash()` methods.
    pub fn key_hash(&mut self, v: &Value) -> R<u64> {
        if !self.needs_user_key(v) {
            return Ok(super::hash_value(v));
        }
        let mut h = DefaultHasher::new();
        self.key_hash_into(v, &mut h)?;
        Ok(h.finish())
    }

    fn key_hash_into(&mut self, v: &Value, h: &mut DefaultHasher) -> R<()> {
        std::mem::discriminant(v).hash(h);
        match v {
            Value::Struct(_) | Value::Enum(_) | Value::Object(_)
                if v.decl().is_some_and(|d| self.user_hashed(d)) =>
            {
                let span = self.here();
                let r = self.call_method(v.clone(), "hash", Vec::new(), span)?;
                r.as_i64().hash(h);
            }
            Value::Struct(s) => {
                s.decl.hash(h);
                for f in &s.fields {
                    self.key_hash_into(f, h)?;
                }
            }
            Value::Enum(e) => {
                (e.decl, e.case).hash(h);
                for p in &e.payload {
                    self.key_hash_into(p, h)?;
                }
            }
            Value::List(xs) | Value::Tuple(xs) => {
                xs.len().hash(h);
                for x in xs.iter() {
                    self.key_hash_into(x, h)?;
                }
            }
            // Order-independent, as in `hash_value`.
            Value::Map(m) | Value::Set(m) => {
                let mut sum = 0u64;
                for (k, v) in m.iter() {
                    sum = sum.wrapping_add(self.key_hash(k)? ^ self.key_hash(v)?.rotate_left(17));
                }
                sum.hash(h);
            }
            other => super::hash_value(other).hash(h),
        }
        Ok(())
    }

    /// Key equality: structural, or through user `equals()` methods.
    pub fn key_eq(&mut self, a: &Value, b: &Value) -> R<bool> {
        if !self.needs_user_key(a) && !self.needs_user_key(b) {
            return Ok(super::values_equal(a, b));
        }
        Ok(match (a, b) {
            (Value::Struct(_) | Value::Enum(_) | Value::Object(_), _)
                if a.decl().is_some_and(|d| self.user_hashed(d)) =>
            {
                if b.decl().is_none() {
                    return Ok(false);
                }
                let span = self.here();
                self.call_method(a.clone(), "equals", vec![b.clone()], span)?
                    .as_bool()
            }
            (Value::Struct(x), Value::Struct(y)) => {
                if x.decl != y.decl {
                    return Ok(false);
                }
                for (p, q) in x.fields.iter().zip(&y.fields) {
                    if !self.key_eq(p, q)? {
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
                    if !self.key_eq(p, q)? {
                        return Ok(false);
                    }
                }
                true
            }
            (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
                if x.len() != y.len() {
                    return Ok(false);
                }
                for (p, q) in x.iter().zip(y.iter()) {
                    if !self.key_eq(p, q)? {
                        return Ok(false);
                    }
                }
                true
            }
            (Value::Map(x), Value::Map(y)) | (Value::Set(x), Value::Set(y)) => {
                if x.len() != y.len() {
                    return Ok(false);
                }
                for (k, v) in x.iter() {
                    let Some(pos) = self.key_find(y, k)? else {
                        return Ok(false);
                    };
                    let w = y.get(pos).expect("found").1.clone();
                    if !self.key_eq(v, &w)? {
                        return Ok(false);
                    }
                }
                true
            }
            _ => super::values_equal(a, b),
        })
    }

    /// The position of `key` (whose hash is `hash`) in `m`.
    pub fn key_find_hashed(&mut self, m: &OrderedMap, hash: u64, key: &Value) -> R<Option<usize>> {
        if !self.needs_user_key(key) {
            return Ok(m.find(hash, key, &mut super::values_equal));
        }
        for pos in m.candidates(hash) {
            let k = m.get(pos).expect("a live candidate").0.clone();
            if self.key_eq(&k, key)? {
                return Ok(Some(pos));
            }
        }
        Ok(None)
    }

    /// The position of `key` in `m`.
    pub fn key_find(&mut self, m: &OrderedMap, key: &Value) -> R<Option<usize>> {
        let hash = self.key_hash(key)?;
        self.key_find_hashed(m, hash, key)
    }

    /// Inserts or replaces; returns the previous value. `m` must not be reachable
    /// from SCB code while this runs (a map being built, or one moved out of its
    /// place): comparing keys may run user code.
    pub fn map_insert(&mut self, m: &mut OrderedMap, key: Value, value: Value) -> R<Option<Value>> {
        let hash = self.key_hash(&key)?;
        let pos = self.key_find_hashed(m, hash, &key)?;
        Ok(m.insert_at(pos, hash, key, value))
    }

    /// Removes `key`; returns its value.
    pub fn map_remove(&mut self, m: &mut OrderedMap, key: &Value) -> R<Option<Value>> {
        Ok(self.key_find(m, key)?.map(|pos| m.remove_at(pos)))
    }

    /// A span for calls the runtime makes on its own (`hash()`, `equals()`): the
    /// current call site.
    pub(crate) fn here(&self) -> Span {
        self.iso
            .calls
            .last()
            .map_or_else(|| self.program.funcs[0].span, |c| c.1)
    }
}
