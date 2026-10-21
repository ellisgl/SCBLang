//! Runtime exclusivity for class properties and statics (SPEC "Exclusivity").
//!
//! Locals cannot alias, so conflicts on them are found at compile time. A class
//! instance can be reached through several variables, and a static from anywhere, so
//! those are checked here: a modifying access that spans user code (an `inout`
//! argument, a `mutating` call, a compound assignment) *claims* the storage it
//! modifies for its duration, and any access that reaches claimed storage meanwhile
//! throws `ExclusivityError`.
//!
//! The claimed storage is the last class property on the place's path (`$a->b->c[0]`
//! claims `c` of the object in `$a->b`), or the static at its root when the path
//! crosses no object. A path entirely inside locals claims nothing.

use std::cell::RefCell;
use std::rc::Rc;

use scb_syntax::Span;
use scb_types::decls::StaticId;

use super::place::{EPlace, EProj, ERoot};
use super::{Interp, R};
use crate::isolate::StaticSlot;
use crate::value::{ObjectValue, Value};

/// Storage claimed by a modifying access.
#[derive(Debug)]
pub(crate) enum Exclusive {
    Static(StaticId),
    /// A property of a class instance (held, so its identity stays unique).
    Field(Rc<RefCell<ObjectValue>>, u32),
}

impl Interp<'_> {
    /// Claims the storage `ep` modifies, if it is a class property or a static.
    /// Returns the claim to pass to [`Self::end_exclusive`].
    pub(crate) fn begin_exclusive(&mut self, ep: &EPlace) -> Option<u64> {
        let storage = self.storage_of(ep)?;
        let id = self.iso.next_access;
        self.iso.next_access += 1;
        self.iso.exclusive.push((id, storage));
        Some(id)
    }

    pub(crate) fn end_exclusive(&mut self, claim: Option<u64>) {
        if let Some(id) = claim {
            self.iso.exclusive.retain(|(i, _)| *i != id);
        }
    }

    fn storage_of(&mut self, ep: &EPlace) -> Option<Exclusive> {
        let (mut cur, mut found) = match &ep.root {
            ERoot::Local(_) if ep.path.is_empty() => return None,
            ERoot::Local(id) => (self.local(*id).clone(), None),
            ERoot::Static(sid) => {
                let v = match &self.iso.statics[sid.0 as usize] {
                    StaticSlot::Ready(v) => v.clone(),
                    _ => Value::Null,
                };
                (v, Some(Exclusive::Static(*sid)))
            }
            ERoot::Value(v) => (v.clone(), None),
        };
        for proj in &ep.path {
            let next = match (proj, &cur) {
                (EProj::Field(i), Value::Object(o)) => {
                    found = Some(Exclusive::Field(o.clone(), *i));
                    o.borrow().fields[*i as usize].clone()
                }
                (EProj::Field(i), Value::Struct(s)) => s.fields[*i as usize].clone(),
                (EProj::Tuple(i), Value::Tuple(t)) => t[*i as usize].clone(),
                (EProj::List(i), Value::List(l)) => match super::place::index_of(*i, l.len()) {
                    Some(i) => l[i].clone(),
                    None => break,
                },
                (EProj::Map(k, h), Value::Map(m)) => {
                    match m.find(*h, k, &mut super::ops::values_equal) {
                        Some(pos) => m.get(pos).expect("found").1.clone(),
                        None => break,
                    }
                }
                _ => break,
            };
            cur = next;
        }
        found
    }

    /// An access reaching property `i` of `o`: fails if it is claimed.
    pub(crate) fn check_field(
        &mut self,
        o: &Rc<RefCell<ObjectValue>>,
        i: u32,
        span: Span,
    ) -> R<()> {
        if self.iso.exclusive.is_empty() {
            return Ok(());
        }
        let claimed = self
            .iso
            .exclusive
            .iter()
            .any(|(_, s)| matches!(s, Exclusive::Field(p, j) if *j == i && Rc::ptr_eq(p, o)));
        if !claimed {
            return Ok(());
        }
        let decl = self.program.decls.ty(o.borrow().decl);
        let what = format!("{}::${}", decl.name, decl.fields[i as usize].name);
        Err(self.conflict(&what, span))
    }

    /// An access reaching static `sid`: fails if it is claimed.
    pub(crate) fn check_static(&mut self, sid: StaticId, span: Span) -> R<()> {
        if self.iso.exclusive.is_empty() {
            return Ok(());
        }
        let claimed = self
            .iso
            .exclusive
            .iter()
            .any(|(_, s)| matches!(s, Exclusive::Static(t) if *t == sid));
        if !claimed {
            return Ok(());
        }
        let decls = &self.program.decls;
        let st = decls.static_(sid);
        let what = format!("{}::${}", decls.ty(st.owner).name, st.name);
        Err(self.conflict(&what, span))
    }

    /// Every property of `o` (a whole-object access such as `clone`).
    pub(crate) fn check_object(&mut self, o: &Rc<RefCell<ObjectValue>>, span: Span) -> R<()> {
        if self.iso.exclusive.is_empty() {
            return Ok(());
        }
        let n = o.borrow().fields.len() as u32;
        for i in 0..n {
            self.check_field(o, i, span)?;
        }
        Ok(())
    }

    fn conflict(&mut self, what: &str, span: Span) -> super::Flow {
        self.error(
            "ExclusivityError",
            format!("`{what}` is accessed while it is being modified"),
            span,
        )
    }
}
