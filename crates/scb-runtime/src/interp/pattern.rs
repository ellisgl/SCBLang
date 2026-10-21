//! Patterns and runtime type tests (SPEC "Enums and match").
//!
//! A pattern binds as it matches; when a later part fails, the bindings already made
//! are harmless (the arm is not taken and its locals are reset).

use scb_types::tir::{Pattern, PatternKind as P};
use scb_types::ty::{Ty, TyKind};

use super::{Interp, R};
use crate::value::Value;

impl Interp<'_> {
    pub(crate) fn match_pattern(&mut self, p: &Pattern, v: &Value) -> R<bool> {
        Ok(match &p.kind {
            P::Wildcard => true,
            P::Bind(l) => {
                *self.local(*l) = v.clone();
                true
            }
            P::TypeTest { ty, bind } => {
                let ok = self.value_is(v, *ty);
                if ok && let Some(l) = bind {
                    *self.local(*l) = v.clone();
                }
                ok
            }
            P::Equals(e) => {
                let w = self.eval(e)?;
                self.eq(v, &w, p.span)?
            }
            P::Range {
                start,
                end,
                inclusive,
            } => match v.as_i128() {
                Some(x) => x >= *start && if *inclusive { x <= *end } else { x < *end },
                None => false,
            },
            P::Case {
                decl,
                case,
                payload,
            } => match v {
                Value::Enum(e) if e.decl == *decl && e.case == *case => {
                    let values = e.payload.clone();
                    for (sub, pv) in payload.iter().zip(values.iter()) {
                        if !self.match_pattern(sub, pv)? {
                            return Ok(false);
                        }
                    }
                    true
                }
                _ => false,
            },
            P::Class(decl) => v
                .decl()
                .is_some_and(|d| self.program.decls.is_subtype(d, *decl)),
            P::Struct { decl, fields } => {
                let values: Vec<Value> = match v {
                    Value::Struct(s) if self.program.decls.is_subtype(s.decl, *decl) => {
                        s.fields.clone()
                    }
                    Value::Object(o) if self.program.decls.is_subtype(o.borrow().decl, *decl) => {
                        for (idx, _) in fields {
                            self.check_field(o, *idx, p.span)?;
                        }
                        o.borrow().fields.clone()
                    }
                    _ => return Ok(false),
                };
                for (idx, sub) in fields {
                    if !self.match_pattern(sub, &values[*idx as usize])? {
                        return Ok(false);
                    }
                }
                true
            }
            P::Tuple(items) => {
                let values = match v {
                    Value::Tuple(t) | Value::List(t) if t.len() == items.len() => t.clone(),
                    _ => return Ok(false),
                };
                for (sub, x) in items.iter().zip(values.iter()) {
                    if !self.match_pattern(sub, x)? {
                        return Ok(false);
                    }
                }
                true
            }
        })
    }

    /// Whether a value has type `ty`: the runtime side of `$v: T` and `instanceof`.
    ///
    /// Collections are recognized by kind only: values do not record their element
    /// types, so `list<int>` and `list<string>` cannot be told apart at runtime (the
    /// checker only allows such a test where the kind decides).
    pub(crate) fn value_is(&self, v: &Value, ty: Ty) -> bool {
        let types = &self.program.types;
        match types.kind(ty) {
            TyKind::Error => true,
            TyKind::Null => matches!(v, Value::Null),
            TyKind::Bool => matches!(v, Value::Bool(_)),
            TyKind::Int(i) => v.int_ty() == Some(*i),
            TyKind::Float(scb_types::ty::FloatTy::F32) => matches!(v, Value::F32(_)),
            TyKind::Float(scb_types::ty::FloatTy::F64) => matches!(v, Value::F64(_)),
            TyKind::String => matches!(v, Value::Str(_)),
            TyKind::Bytes => matches!(v, Value::Bytes(_)),
            TyKind::List(_) => matches!(v, Value::List(_)),
            TyKind::Map(..) => matches!(v, Value::Map(_)),
            TyKind::Set(_) => matches!(v, Value::Set(_)),
            TyKind::Tuple(es) => matches!(v, Value::Tuple(t) if t.len() == es.len()),
            TyKind::Range(_) => matches!(v, Value::Range(_)),
            TyKind::Fn(_) => matches!(v, Value::Func(_)),
            TyKind::Union(ms) => ms.iter().any(|&m| self.value_is(v, m)),
            TyKind::Named(d, _) => v
                .decl()
                .is_some_and(|vd| self.program.decls.is_subtype(vd, *d)),
            TyKind::Never | TyKind::Void | TyKind::Param(_) | TyKind::SelfType => false,
        }
    }
}
