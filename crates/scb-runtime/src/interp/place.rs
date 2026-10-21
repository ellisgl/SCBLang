//! Places at runtime: reading and writing through paths (SPEC "Nested mutation").
//!
//! A TIR place is first *evaluated* into an [`EPlace`]: the root is resolved and
//! every index expression on the path is computed, left to right (SPEC "Evaluation
//! order": path first, then the right-hand side, then the write). The write itself
//! walks the path from the root:
//!
//! - a value-typed level (string, list, map, tuple, struct, bytes) is made unique
//!   with `Rc::make_mut`, which copies only that level and only if it is shared, so
//!   an unshared path costs no copies;
//! - a class instance is a reference: the walk continues *inside* the shared object.
//!   It restarts from a fresh handle at every object, so no `RefCell` borrow is held
//!   while descending (a value that refers back to its own object would otherwise
//!   be borrowed twice).
//!
//! A map key compared with user `equals()` (see `keys.rs`) cannot be looked up during
//! the walk, which holds the map mutably: a write first finds those positions with a
//! read-only pass ([`Interp::resolve_keys`]), and the walk checks that each position
//! still holds the key found there.

use std::rc::Rc;

use scb_syntax::Span;
use scb_types::decls::StaticId;
use scb_types::tir::{Expr, IndexKind, LocalId, Place, PlaceRoot, Proj};

use super::{Flow, Interp, R};
use crate::isolate::StaticSlot;
use crate::value::{ObjectValue, OrderedMap, Value};

/// An evaluated place: everything computed, ready to read or write.
#[derive(Debug, Clone)]
pub(crate) struct EPlace {
    pub(super) root: ERoot,
    pub(super) path: Vec<EProj>,
    pub(super) span: Span,
}

#[derive(Debug, Clone)]
pub(super) enum ERoot {
    Local(LocalId),
    Static(StaticId),
    /// The value of an expression (a class instance for writes).
    Value(Value),
}

#[derive(Debug, Clone)]
pub(super) enum EProj {
    Field(u32),
    List(i64),
    /// The key, its hash, and whether comparing it runs user code.
    Map(Value, u64, bool),
    Bytes(i64),
    Tuple(u32),
    Push,
}

/// Why a walk stopped.
enum Fault {
    Index(i64, usize),
    Key(Value),
    /// A projection that does not fit the value (an interpreter bug).
    Shape,
    /// A map changed between finding a user key's position and using it (the user
    /// `equals()` that found it changed the map).
    Changed,
}

/// Per projection: for a map key compared with user code, where it was found (the
/// position and the key there), or `None` if missing; `None` for other projections.
/// The whole table is `None` when no key on the path needs user code (the usual
/// case, which then costs nothing).
type Resolved = Option<Vec<Option<Option<(usize, Value)>>>>;

/// The result of walking one stretch of a path.
enum Step<T> {
    Done(T),
    /// Reached a class instance: continue inside it with the remaining path.
    Object(Rc<std::cell::RefCell<ObjectValue>>, usize),
}

impl Interp<'_> {
    pub(crate) fn eval_place(&mut self, p: &Place) -> R<EPlace> {
        let root = match &p.root {
            PlaceRoot::Local(id) => ERoot::Local(*id),
            PlaceRoot::Static(sid) => {
                self.ensure_static(*sid, p.span)?;
                ERoot::Static(*sid)
            }
            PlaceRoot::Object(e) => ERoot::Value(self.eval(e)?),
        };
        let mut path = Vec::with_capacity(p.path.len());
        for proj in &p.path {
            path.push(match proj {
                Proj::Field(i) => EProj::Field(*i),
                Proj::TupleField(i) => EProj::Tuple(*i),
                Proj::Push => EProj::Push,
                Proj::Index(e, kind) => self.index(e, *kind)?,
            });
        }
        Ok(EPlace {
            root,
            path,
            span: p.span,
        })
    }

    fn index(&mut self, e: &Expr, kind: IndexKind) -> R<EProj> {
        let v = self.eval(e)?;
        Ok(match kind {
            IndexKind::List => EProj::List(v.as_i64()),
            IndexKind::Bytes => EProj::Bytes(v.as_i64()),
            IndexKind::Map => {
                let user = self.needs_user_key(&v);
                let h = if user {
                    self.key_hash(&v)?
                } else {
                    super::hash_value(&v)
                };
                EProj::Map(v, h, user)
            }
        })
    }

    /// Reads the value at a place.
    pub(crate) fn read(&mut self, ep: &EPlace) -> R<Value> {
        let mut cur = match &ep.root {
            ERoot::Local(id) => self.local(*id).clone(),
            ERoot::Static(sid) => {
                self.check_static(*sid, ep.span)?;
                self.static_value(*sid)
            }
            ERoot::Value(v) => v.clone(),
        };
        for proj in &ep.path {
            let next = match (proj, &cur) {
                (EProj::Field(i), Value::Struct(s)) => s.fields[*i as usize].clone(),
                (EProj::Field(i), Value::Object(o)) => {
                    self.check_field(o, *i, ep.span)?;
                    o.borrow().fields[*i as usize].clone()
                }
                (EProj::Tuple(i), Value::Tuple(t)) => t[*i as usize].clone(),
                (EProj::List(i), Value::List(l)) => match index_of(*i, l.len()) {
                    Some(i) => l[i].clone(),
                    None => return Err(self.fault(Fault::Index(*i, l.len()), ep.span)),
                },
                (EProj::Bytes(i), Value::Bytes(b)) => match index_of(*i, b.len()) {
                    Some(i) => Value::U8(b[i]),
                    None => return Err(self.fault(Fault::Index(*i, b.len()), ep.span)),
                },
                (EProj::Map(k, h, user), Value::Map(m)) => match self.lookup(m, *h, k, *user)? {
                    Some(pos) => m.get(pos).expect("found").1.clone(),
                    None => return Err(self.fault(Fault::Key(k.clone()), ep.span)),
                },
                _ => return Err(self.fault(Fault::Shape, ep.span)),
            };
            cur = next;
        }
        Ok(cur)
    }

    /// Reads with a missing final map key giving `null` (`??=`). Everything before
    /// the last step must exist (SPEC "No autovivification"), and a list index must
    /// be in range: those trap as in any read.
    pub(crate) fn read_or_null(&mut self, ep: &EPlace) -> R<Value> {
        let Some(EProj::Map(k, h, user)) = ep.path.last() else {
            return self.read(ep);
        };
        let prefix = self.prefix(ep);
        let Value::Map(m) = self.read(&prefix)? else {
            return Err(self.fault(Fault::Shape, ep.span));
        };
        Ok(self
            .lookup(&m, *h, k, *user)?
            .map_or(Value::Null, |pos| m.get(pos).expect("found").1.clone()))
    }

    /// Moves the value out of a place (leaving `null`), for `inout` and `mutating`
    /// receivers; [`Self::assign`] puts it back.
    pub(crate) fn take(&mut self, ep: &EPlace) -> R<Value> {
        self.modify(ep, &mut |v| std::mem::take(v))
    }

    /// Stores `v` at a place: `[]` appends, a missing map key is inserted.
    pub(crate) fn assign(&mut self, ep: &EPlace, v: Value) -> R<()> {
        let mut v = Some(v);
        let resolved = self.resolve_keys(ep)?;
        let fits = match ep.path.last() {
            Some(EProj::Push) => {
                let prefix = self.prefix(ep);
                self.modify_resolved(&prefix, &resolved, &mut |list| match list {
                    Value::List(l) => {
                        Rc::make_mut(l).push(v.take().expect("assigned once"));
                        Ok(())
                    }
                    _ => Err(Fault::Shape),
                })?
            }
            Some(EProj::Map(k, h, _)) => {
                let (k, h) = (k.clone(), *h);
                let found = resolved.as_ref().and_then(|r| r.last().cloned().flatten());
                let prefix = self.prefix(ep);
                self.modify_resolved(&prefix, &resolved, &mut |map| match map {
                    Value::Map(m) => {
                        let pos = match &found {
                            None => m.find(h, &k, &mut super::ops::values_equal),
                            Some(found) => checked_position(m, h, found.as_ref())?,
                        };
                        let value = v.take().expect("assigned once");
                        Rc::make_mut(m).insert_at(pos, h, k.clone(), value);
                        Ok(())
                    }
                    _ => Err(Fault::Shape),
                })?
            }
            _ => self.modify_resolved(ep, &resolved, &mut |slot| {
                *slot = v.take().expect("assigned once");
                Ok(())
            })?,
        };
        fits.map_err(|f| self.fault(f, ep.span))
    }

    /// Positions of the map keys on `ep`'s path that are compared with user code
    /// (see the module docs). Stops at the first step that cannot be taken; the walk
    /// that follows reports it.
    fn resolve_keys(&mut self, ep: &EPlace) -> R<Resolved> {
        if !ep.path.iter().any(|p| matches!(p, EProj::Map(_, _, true))) {
            return Ok(None);
        }
        let mut out = vec![None; ep.path.len()];
        let mut cur = match &ep.root {
            ERoot::Local(id) => self.local(*id).clone(),
            ERoot::Static(sid) => self.static_value(*sid),
            ERoot::Value(v) => v.clone(),
        };
        for (i, proj) in ep.path.iter().enumerate() {
            let next = match (proj, &cur) {
                (EProj::Field(f), Value::Struct(s)) => s.fields[*f as usize].clone(),
                (EProj::Field(f), Value::Object(o)) => o.borrow().fields[*f as usize].clone(),
                (EProj::Tuple(t), Value::Tuple(xs)) => xs[*t as usize].clone(),
                (EProj::List(n), Value::List(l)) => match index_of(*n, l.len()) {
                    Some(n) => l[n].clone(),
                    None => break,
                },
                (EProj::Map(k, h, true), Value::Map(m)) => {
                    let pos = self.key_find_hashed(m, *h, k)?;
                    let entry = pos.map(|p| m.get(p).expect("found").clone());
                    out[i] = Some(pos.zip(entry.as_ref().map(|e| e.0.clone())));
                    match entry {
                        Some((_, value)) => value,
                        None => break,
                    }
                }
                (EProj::Map(k, h, false), Value::Map(m)) => {
                    match m.find(*h, k, &mut super::ops::values_equal) {
                        Some(p) => m.get(p).expect("found").1.clone(),
                        None => break,
                    }
                }
                _ => break,
            };
            cur = next;
        }
        Ok(Some(out))
    }

    /// The position of a map key: structural, or with user code when the projection
    /// says the key needs it (decided once, when the place was evaluated).
    fn lookup(&mut self, m: &OrderedMap, h: u64, k: &Value, user: bool) -> R<Option<usize>> {
        if user {
            self.key_find_hashed(m, h, k)
        } else {
            Ok(m.find(h, k, &mut super::ops::values_equal))
        }
    }

    fn prefix(&self, ep: &EPlace) -> EPlace {
        EPlace {
            root: ep.root.clone(),
            path: ep.path[..ep.path.len() - 1].to_vec(),
            span: ep.span,
        }
    }

    /// Applies `f` to the value at a place, making every value-typed level on the
    /// way unique first (copy-on-write).
    pub(crate) fn modify<T>(&mut self, ep: &EPlace, f: &mut dyn FnMut(&mut Value) -> T) -> R<T> {
        let resolved = self.resolve_keys(ep)?;
        let r = self.modify_resolved(ep, &resolved, &mut |v| Ok(f(v)))?;
        r.map_err(|e| self.fault(e, ep.span))
    }

    /// [`Self::modify`] with the user-compared map keys already found. The leaf may
    /// fail (a write that does not fit); that is returned, not raised.
    fn modify_resolved<T>(
        &mut self,
        ep: &EPlace,
        resolved: &Resolved,
        f: &mut dyn FnMut(&mut Value) -> Result<T, Fault>,
    ) -> R<Result<T, Fault>> {
        let mut f = Some(f);
        let mut leaf = |v: &mut Value| (f.take().expect("one leaf"))(v);
        let walk = Walk {
            path: &ep.path,
            resolved,
        };
        let step = match &ep.root {
            ERoot::Local(id) => {
                let slot = &mut self.iso.frames.last_mut().expect("a frame").locals[id.0 as usize];
                walk.from(slot, 0, &mut leaf)
            }
            ERoot::Static(sid) => {
                self.check_static(*sid, ep.span)?;
                let StaticSlot::Ready(slot) = &mut self.iso.statics[sid.0 as usize] else {
                    return Err(Flow::Fatal("static property not initialized".into()));
                };
                walk.from(slot, 0, &mut leaf)
            }
            ERoot::Value(v) => {
                let mut v = v.clone();
                walk.from(&mut v, 0, &mut leaf)
            }
        };
        let mut step = step.map_err(|e| self.fault(e, ep.span))?;
        loop {
            match step {
                Step::Done(t) => return Ok(t),
                Step::Object(obj, at) => {
                    let Some(&EProj::Field(i)) = ep.path.get(at) else {
                        return Err(self.fault(Fault::Shape, ep.span));
                    };
                    self.check_field(&obj, i, ep.span)?;
                    let mut o = obj.borrow_mut();
                    let r = walk.from(&mut o.fields[i as usize], at + 1, &mut leaf);
                    drop(o);
                    step = r.map_err(|e| self.fault(e, ep.span))?;
                }
            }
        }
    }

    fn fault(&mut self, f: Fault, span: Span) -> Flow {
        match f {
            Fault::Index(i, len) => self.error(
                "IndexError",
                format!("index {i} is out of range (size {len})"),
                span,
            ),
            Fault::Key(k) => {
                let shown = self.debug_key(&k);
                self.error("KeyError", format!("key {shown} not found"), span)
            }
            Fault::Shape => Flow::Fatal("internal error: a place does not match its value".into()),
            Fault::Changed => self.error(
                "ExclusivityError",
                "a map was changed while one of its keys was being compared",
                span,
            ),
        }
    }

    fn debug_key(&mut self, k: &Value) -> String {
        match k {
            Value::Str(s) => format!("'{s}'"),
            other => self.display(other).unwrap_or_else(|_| "?".into()),
        }
    }

    // ---------- statics (SPEC "Static initialization") ----------

    /// Initializes a static on first access; a failed initializer poisons it.
    pub(crate) fn ensure_static(&mut self, sid: StaticId, span: Span) -> R<()> {
        match &self.iso.statics[sid.0 as usize] {
            StaticSlot::Ready(_) => Ok(()),
            StaticSlot::Running => {
                let name = &self.program.decls.static_(sid).name;
                let msg = format!("static `${name}` is used while it is being initialized");
                Err(self.error("StaticInitError", msg, span))
            }
            StaticSlot::Poisoned(original) => {
                let original = original.clone();
                let name = &self.program.decls.static_(sid).name;
                let msg = format!("static `${name}` failed to initialize earlier");
                let Flow::Throw(e, at) = self.error("StaticInitError", msg, span) else {
                    unreachable!("error() throws")
                };
                super::stmt::set_previous(&e, original);
                Err(Flow::Throw(e, at))
            }
            StaticSlot::Destroyed => {
                let name = &self.program.decls.static_(sid).name;
                let msg = format!("static `${name}` is used after it was released at program end");
                Err(self.error("StaticInitError", msg, span))
            }
            StaticSlot::Uninit => {
                self.iso.statics[sid.0 as usize] = StaticSlot::Running;
                let program = self.program;
                match self.eval_init(&program.statics[sid.0 as usize]) {
                    Ok(v) => {
                        self.iso.statics[sid.0 as usize] = StaticSlot::Ready(v);
                        self.iso.static_order.push(sid);
                        Ok(())
                    }
                    Err(Flow::Throw(e, at)) => {
                        self.iso.statics[sid.0 as usize] = StaticSlot::Poisoned(e.clone());
                        Err(Flow::Throw(e, at))
                    }
                    Err(other) => {
                        self.iso.statics[sid.0 as usize] = StaticSlot::Uninit;
                        Err(other)
                    }
                }
            }
        }
    }

    fn static_value(&self, sid: StaticId) -> Value {
        match &self.iso.statics[sid.0 as usize] {
            StaticSlot::Ready(v) => v.clone(),
            _ => Value::Null,
        }
    }
}

/// `i` as a position in a collection of `len` elements.
pub(crate) fn index_of(i: i64, len: usize) -> Option<usize> {
    usize::try_from(i).ok().filter(|&i| i < len)
}

/// The position for a user-compared key found by [`Interp::resolve_keys`], if the map
/// still holds that key there (user code ran in between); `None` if it was missing.
fn checked_position(
    m: &crate::value::OrderedMap,
    h: u64,
    found: Option<&(usize, Value)>,
) -> Result<Option<usize>, Fault> {
    let Some((pos, key)) = found else {
        return Ok(None);
    };
    let still =
        m.hash_at(*pos) == Some(h) && super::ops::values_equal(&m.get(*pos).expect("live").0, key);
    if still {
        Ok(Some(*pos))
    } else {
        Err(Fault::Changed)
    }
}

/// A write walk along a path (see the module docs).
struct Walk<'p> {
    path: &'p [EProj],
    resolved: &'p Resolved,
}

impl Walk<'_> {
    /// Walks `path[at..]` from `v`. Stops at a class instance (returning it) unless the
    /// path ends there.
    fn from<T>(
        &self,
        v: &mut Value,
        at: usize,
        leaf: &mut dyn FnMut(&mut Value) -> Result<T, Fault>,
    ) -> Result<Step<Result<T, Fault>>, Fault> {
        let Some(proj) = self.path.get(at) else {
            return Ok(Step::Done(leaf(v)));
        };
        match (proj, v) {
            (EProj::Field(_), Value::Object(o)) => Ok(Step::Object(o.clone(), at)),
            (EProj::Field(i), Value::Struct(s)) => {
                self.from(&mut Rc::make_mut(s).fields[*i as usize], at + 1, leaf)
            }
            (EProj::Tuple(i), Value::Tuple(t)) => {
                self.from(&mut Rc::make_mut(t)[*i as usize], at + 1, leaf)
            }
            (EProj::List(i), Value::List(l)) => {
                let len = l.len();
                let i = index_of(*i, len).ok_or(Fault::Index(*i, len))?;
                self.from(&mut Rc::make_mut(l)[i], at + 1, leaf)
            }
            (EProj::Map(k, h, _), Value::Map(m)) => {
                let found = self
                    .resolved
                    .as_ref()
                    .and_then(|r| r.get(at).cloned().flatten());
                let pos = match found {
                    None => m.find(*h, k, &mut super::ops::values_equal),
                    Some(found) => checked_position(m, *h, found.as_ref())?,
                }
                .ok_or_else(|| Fault::Key(k.clone()))?;
                self.from(Rc::make_mut(m).value_mut(pos), at + 1, leaf)
            }
            (EProj::Bytes(i), Value::Bytes(b)) => {
                let len = b.len();
                let i = index_of(*i, len).ok_or(Fault::Index(*i, len))?;
                let bytes = Rc::make_mut(b);
                let mut tmp = Value::U8(bytes[i]);
                let r = self.from(&mut tmp, at + 1, leaf)?;
                if let Value::U8(x) = tmp {
                    bytes[i] = x;
                }
                Ok(r)
            }
            (EProj::Push, _) => Err(Fault::Shape),
            _ => Err(Fault::Shape),
        }
    }
}
