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

use std::rc::Rc;

use scb_syntax::Span;
use scb_types::decls::StaticId;
use scb_types::tir::{Expr, IndexKind, LocalId, Place, PlaceRoot, Proj};

use super::{Flow, Interp, R};
use crate::isolate::StaticSlot;
use crate::value::{ObjectValue, Value};

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
    Map(Value, u64),
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
}

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
                let h = super::hash_value(&v);
                EProj::Map(v, h)
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
                (EProj::Map(k, h), Value::Map(m)) => {
                    match m.find(*h, k, &mut super::ops::values_equal) {
                        Some(pos) => m.get(pos).expect("found").1.clone(),
                        None => return Err(self.fault(Fault::Key(k.clone()), ep.span)),
                    }
                }
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
        let Some(EProj::Map(k, h)) = ep.path.last() else {
            return self.read(ep);
        };
        let prefix = self.prefix(ep);
        let Value::Map(m) = self.read(&prefix)? else {
            return Err(self.fault(Fault::Shape, ep.span));
        };
        Ok(m.find(*h, k, &mut super::ops::values_equal)
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
        let fits = match ep.path.last() {
            Some(EProj::Push) => {
                let prefix = self.prefix(ep);
                self.modify(&prefix, &mut |list| match list {
                    Value::List(l) => {
                        Rc::make_mut(l).push(v.take().expect("assigned once"));
                        true
                    }
                    _ => false,
                })?
            }
            Some(EProj::Map(k, h)) => {
                let (k, h) = (k.clone(), *h);
                let prefix = self.prefix(ep);
                self.modify(&prefix, &mut |map| match map {
                    Value::Map(m) => {
                        Rc::make_mut(m).insert(
                            h,
                            k.clone(),
                            v.take().expect("assigned once"),
                            &mut super::ops::values_equal,
                        );
                        true
                    }
                    _ => false,
                })?
            }
            _ => self.modify(ep, &mut |slot| {
                *slot = v.take().expect("assigned once");
                true
            })?,
        };
        if fits {
            Ok(())
        } else {
            Err(self.fault(Fault::Shape, ep.span))
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
        let mut f = Some(f);
        let mut leaf = |v: &mut Value| (f.take().expect("one leaf"))(v);
        let step = match &ep.root {
            ERoot::Local(id) => {
                let slot = &mut self.iso.frames.last_mut().expect("a frame").locals[id.0 as usize];
                walk(slot, &ep.path, &mut leaf)
            }
            ERoot::Static(sid) => {
                self.check_static(*sid, ep.span)?;
                let StaticSlot::Ready(slot) = &mut self.iso.statics[sid.0 as usize] else {
                    return Err(Flow::Fatal("static property not initialized".into()));
                };
                walk(slot, &ep.path, &mut leaf)
            }
            ERoot::Value(v) => {
                let mut v = v.clone();
                walk(&mut v, &ep.path, &mut leaf)
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
                    let r = walk_from(&mut o.fields[i as usize], &ep.path, at + 1, &mut leaf);
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
                let Flow::Throw(e) = self.error("StaticInitError", msg, span) else {
                    unreachable!("error() throws")
                };
                super::stmt::set_previous(&e, original);
                Err(Flow::Throw(e))
            }
            // SPEC-OPEN: Q41 (a static read after release at program end)
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
                    Err(Flow::Throw(e)) => {
                        self.iso.statics[sid.0 as usize] = StaticSlot::Poisoned(e.clone());
                        Err(Flow::Throw(e))
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

fn walk<T>(
    v: &mut Value,
    path: &[EProj],
    leaf: &mut dyn FnMut(&mut Value) -> T,
) -> Result<Step<T>, Fault> {
    walk_from(v, path, 0, leaf)
}

/// Walks `path[at..]` from `v`. Stops at a class instance (returning it) unless the
/// path ends there.
fn walk_from<T>(
    v: &mut Value,
    path: &[EProj],
    at: usize,
    leaf: &mut dyn FnMut(&mut Value) -> T,
) -> Result<Step<T>, Fault> {
    let Some(proj) = path.get(at) else {
        return Ok(Step::Done(leaf(v)));
    };
    match (proj, v) {
        (EProj::Field(_), Value::Object(o)) => Ok(Step::Object(o.clone(), at)),
        (EProj::Field(i), Value::Struct(s)) => {
            walk_from(&mut Rc::make_mut(s).fields[*i as usize], path, at + 1, leaf)
        }
        (EProj::Tuple(i), Value::Tuple(t)) => {
            walk_from(&mut Rc::make_mut(t)[*i as usize], path, at + 1, leaf)
        }
        (EProj::List(i), Value::List(l)) => {
            let len = l.len();
            let i = index_of(*i, len).ok_or(Fault::Index(*i, len))?;
            walk_from(&mut Rc::make_mut(l)[i], path, at + 1, leaf)
        }
        (EProj::Map(k, h), Value::Map(m)) => {
            let pos = m
                .find(*h, k, &mut super::ops::values_equal)
                .ok_or_else(|| Fault::Key(k.clone()))?;
            walk_from(Rc::make_mut(m).value_mut(pos), path, at + 1, leaf)
        }
        (EProj::Bytes(i), Value::Bytes(b)) => {
            let len = b.len();
            let i = index_of(*i, len).ok_or(Fault::Index(*i, len))?;
            let bytes = Rc::make_mut(b);
            let mut tmp = Value::U8(bytes[i]);
            let r = walk_from(&mut tmp, path, at + 1, leaf)?;
            if let Value::U8(x) = tmp {
                bytes[i] = x;
            }
            Ok(r)
        }
        (EProj::Push, _) => Err(Fault::Shape),
        _ => Err(Fault::Shape),
    }
}
