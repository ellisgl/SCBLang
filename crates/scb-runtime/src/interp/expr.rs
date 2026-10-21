//! Expressions.

use std::cell::RefCell;
use std::rc::Rc;

use scb_syntax::Span;
use scb_types::decls::{DeclId, TypeKind};
use scb_types::tir::{
    Arg, BinaryOp, CallTarget, Expr, ExprKind as K, FuncId, Lit, Receiver, UnaryOp,
};
use scb_types::ty::TyKind;

use super::ops::wrap;
use super::place::EPlace;
use super::{Flow, Interp, R};
use crate::value::{
    EnumValue, FuncValue, NativeState, ObjectValue, OrderedMap, RangeValue, StructValue,
    ThrowableState, Value,
};

impl Interp<'_> {
    pub(crate) fn eval(&mut self, e: &Expr) -> R<Value> {
        let span = e.span;
        Ok(match &e.kind {
            K::Lit(l) => self.literal(l, e),
            K::Local(id) => self.local(*id).clone(),
            K::Const(c) => self.constant(*c)?,
            K::ClassName(d) => Value::str(self.program.decls.ty(*d).name.clone()),
            K::List(xs) => {
                let mut out = Vec::with_capacity(xs.len());
                for x in xs {
                    out.push(self.eval(x)?);
                }
                Value::list(out)
            }
            K::Tuple(xs) => {
                let mut out = Vec::with_capacity(xs.len());
                for x in xs {
                    out.push(self.eval(x)?);
                }
                Value::Tuple(Rc::new(out))
            }
            K::Set(xs) => {
                let mut m = OrderedMap::new();
                for x in xs {
                    let v = self.eval(x)?;
                    m.insert(
                        super::hash_value(&v),
                        v,
                        Value::Null,
                        &mut super::ops::values_equal,
                    );
                }
                Value::Set(Rc::new(m))
            }
            K::Map(kvs) => {
                let mut m = OrderedMap::new();
                for (k, v) in kvs {
                    let k = self.eval(k)?;
                    let v = self.eval(v)?;
                    m.insert(super::hash_value(&k), k, v, &mut super::ops::values_equal);
                }
                Value::Map(Rc::new(m))
            }
            K::Unary { op, expr } => {
                let v = self.eval(expr)?;
                self.unary(*op, v, span)?
            }
            K::Binary { op, lhs, rhs } => {
                let a = self.eval(lhs)?;
                let b = self.eval(rhs)?;
                self.binary(*op, a, b, span)?
            }
            K::Logic { and, lhs, rhs } => {
                let l = self.eval(lhs)?.as_bool();
                if l != *and {
                    Value::Bool(l)
                } else {
                    Value::Bool(self.eval(rhs)?.as_bool())
                }
            }
            K::Coalesce { lhs, rhs } => match self.eval(lhs)? {
                Value::Null => self.eval(rhs)?,
                v => v,
            },
            K::Ternary {
                cond,
                then,
                otherwise,
            } => {
                if self.eval(cond)?.as_bool() {
                    self.eval(then)?
                } else {
                    self.eval(otherwise)?
                }
            }
            K::Assign { place, value } => {
                let ep = self.eval_place(place)?;
                let v = self.eval(value)?;
                self.assign(&ep, v.clone())?;
                v
            }
            K::CompoundAssign { place, op, value } => {
                let ep = self.eval_place(place)?;
                self.compound(&ep, *op, value, span)?
            }
            K::IncDec {
                place,
                increment,
                prefix,
            } => {
                let ep = self.eval_place(place)?;
                let old = self.read(&ep)?;
                let ty = old.int_ty().expect("++ on integers");
                let x = old.as_i128().unwrap_or(0);
                let sign = if *increment { '+' } else { '-' };
                let v = if *increment { x + 1 } else { x - 1 };
                let new = self.checked(ty, v, || format!("{x} {sign} 1"), span)?;
                self.assign(&ep, new.clone())?;
                if *prefix { new } else { old }
            }
            K::Read(place) => {
                let ep = self.eval_place(place)?;
                self.read(&ep)?
            }
            K::Nullsafe {
                object,
                local,
                then,
            } => match self.eval(object)? {
                Value::Null => Value::Null,
                v => {
                    *self.local(*local) = v;
                    self.eval(then)?
                }
            },
            K::EnumValue(x) => {
                let v = self.eval(x)?;
                self.case_value(&v)?
            }
            K::EnumFrom { decl, value, try_ } => {
                let v = self.eval(value)?;
                self.enum_from(*decl, &v, *try_, span)?
            }
            K::EnumCases(decl) => {
                let n = self.program.decls.ty(*decl).cases.len();
                Value::list(
                    (0..n)
                        .map(|c| enum_value(*decl, c as u32, Vec::new()))
                        .collect(),
                )
            }
            K::Call { target, args } => {
                if matches!(target, CallTarget::Builtin { .. }) {
                    // Natives need the resolved result type; arguments evaluated in
                    // between must not consume it.
                    let r = self.call_builtin_hinted(target, args, e.ty, span)?;
                    return Ok(r);
                }
                self.call_target(target, args, span)?
            }
            K::New { class, ctor, args } => self.instantiate(*class, *ctor, args, span)?,
            K::EnumCase { decl, case, args } => {
                let n = self.program.decls.ty(*decl).cases[*case as usize]
                    .payload
                    .as_ref()
                    .map_or(0, Vec::len);
                let mut payload = vec![Value::Null; n];
                for a in args {
                    if let scb_types::tir::ArgValue::Value(x) = &a.value {
                        payload[a.param as usize] = self.eval(x)?;
                    }
                }
                enum_value(*decl, *case, payload)
            }
            K::Closure { func, captures } => {
                let captures = captures
                    .iter()
                    .map(|(inner, outer)| (*inner, self.local(*outer).clone()))
                    .collect();
                Value::Func(Rc::new(FuncValue::Closure {
                    func: *func,
                    captures,
                }))
            }
            K::CallableRef(target) => self.callable_ref(target)?,
            K::Range {
                start,
                end,
                inclusive,
            } => {
                let s = self.eval(start)?.as_i128().unwrap_or(0);
                let t = self.eval(end)?.as_i128().unwrap_or(0);
                let elem = match self.program.types.kind(e.ty) {
                    TyKind::Range(el) => self
                        .program
                        .types
                        .as_int(*el)
                        .unwrap_or(scb_types::ty::IntTy::I64),
                    _ => scb_types::ty::IntTy::I64,
                };
                Value::Range(Rc::new(RangeValue::new(s, t, *inclusive, elem)))
            }
            K::InstanceOf { expr, pattern } => {
                let v = self.eval(expr)?;
                Value::Bool(self.match_pattern(pattern, &v)?)
            }
            K::Match(m) => self.eval_match(m)?,
            K::Cast(x) => {
                let v = self.eval(x)?;
                self.cast(v, e.ty, span)?
            }
            K::ToString(x) => {
                let v = self.eval(x)?;
                Value::str(self.display(&v)?)
            }
            K::Interpolate(parts) => {
                let mut s = String::new();
                for p in parts {
                    let v = self.eval(p)?;
                    s.push_str(v.as_str());
                }
                Value::str(s)
            }
            K::Throw(x) => return Err(Flow::Throw(self.eval(x)?)),
            K::Exit(code) => {
                let code = match code {
                    Some(c) => self.eval(c)?.as_i64(),
                    None => 0,
                };
                return Err(self.exit_with(code, e.span));
            }
            K::Clone { object, with } => {
                let v = self.eval(object)?;
                let Value::Object(o) = v else {
                    return Ok(v);
                };
                self.check_object(&o, span)?;
                let copy = {
                    let o = o.borrow();
                    ObjectValue {
                        decl: o.decl,
                        fields: o.fields.clone(),
                        native: o.native.clone(),
                        destructor: self.destructor_queue(o.decl),
                    }
                };
                let mut out = Value::Object(Rc::new(RefCell::new(copy)));
                for (idx, x) in with {
                    let v = self.eval(x)?;
                    super::set_field(&mut out, *idx, v);
                }
                out
            }
            K::Upcast(x) => self.eval(x)?,
        })
    }

    fn literal(&self, l: &Lit, e: &Expr) -> Value {
        match l {
            Lit::Int(v) => match self.program.types.kind(e.ty) {
                TyKind::Int(t) => Value::int(*t, *v),
                _ => Value::I64(*v as i64),
            },
            Lit::Float(f) => match self.program.types.kind(e.ty) {
                TyKind::Float(t) => Value::float(*t, *f),
                _ => Value::F64(*f),
            },
            Lit::Bool(b) => Value::Bool(*b),
            Lit::Null => Value::Null,
            Lit::Str(s) => Value::str(s.clone()),
            Lit::Bytes(b) => Value::Bytes(Rc::new(b.clone())),
        }
    }

    fn unary(&mut self, op: UnaryOp, v: Value, span: Span) -> R<Value> {
        Ok(match op {
            UnaryOp::Not => Value::Bool(!v.as_bool()),
            UnaryOp::Plus => v,
            UnaryOp::Neg => match (v.int_ty(), v.as_i128()) {
                (Some(t), Some(x)) => self.checked(t, -x, || format!("-({x})"), span)?,
                _ => match v {
                    Value::F32(f) => Value::F32(-f),
                    Value::F64(f) => Value::F64(-f),
                    other => other,
                },
            },
            UnaryOp::BitNot => match (v.int_ty(), v.as_i128()) {
                (Some(t), Some(x)) => Value::int(t, wrap(t, !x)),
                _ => v,
            },
        })
    }

    pub fn binary(&mut self, op: BinaryOp, a: Value, b: Value, span: Span) -> R<Value> {
        use std::cmp::Ordering::*;
        Ok(match op {
            BinaryOp::Eq => Value::Bool(self.eq(&a, &b, span)?),
            BinaryOp::NotEq => Value::Bool(!self.eq(&a, &b, span)?),
            BinaryOp::Is => Value::Bool(match (&a, &b) {
                (Value::Object(x), Value::Object(y)) => Rc::ptr_eq(x, y),
                _ => false,
            }),
            BinaryOp::Cmp => Value::I64(match self.compare(&a, &b, span)? {
                Less => -1,
                Equal => 0,
                Greater => 1,
            }),
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                // Floats follow IEEE here (NaN compares false); `<=>` is total.
                let ord = match (a.as_f64(), b.as_f64()) {
                    (Some(x), Some(y)) => x.partial_cmp(&y),
                    _ => Some(self.compare(&a, &b, span)?),
                };
                Value::Bool(match (op, ord) {
                    (_, None) => false,
                    (BinaryOp::Lt, Some(o)) => o == Less,
                    (BinaryOp::Le, Some(o)) => o != Greater,
                    (BinaryOp::Gt, Some(o)) => o == Greater,
                    (_, Some(o)) => o != Less,
                })
            }
            _ => self.arith(op, a, b, span)?,
        })
    }

    /// `place op= value`. `.=` appends in place (a loop of appends stays linear);
    /// `??=` assigns only when the place is `null` or a map key is missing.
    fn compound(&mut self, ep: &EPlace, op: BinaryOp, value: &Expr, span: Span) -> R<Value> {
        match op {
            // A compound assignment's access spans the right side (SPEC
            // "Exclusivity"): a class property or static is claimed meanwhile.
            BinaryOp::Coalesce => {
                let old = self.read_or_null(ep)?;
                if !matches!(old, Value::Null) {
                    return Ok(old);
                }
                let claim = self.begin_exclusive(ep);
                let v = self.eval(value);
                self.end_exclusive(claim);
                let v = v?;
                self.assign(ep, v.clone())?;
                Ok(v)
            }
            BinaryOp::Concat => {
                self.read(ep)?;
                let claim = self.begin_exclusive(ep);
                let tail = self.eval(value);
                self.end_exclusive(claim);
                let tail = tail?.as_str().to_string();
                self.modify(ep, &mut |slot| {
                    if let Value::Str(s) = slot {
                        Rc::make_mut(s).push_str(&tail);
                    }
                    slot.clone()
                })
            }
            _ => {
                let old = self.read(ep)?;
                let claim = self.begin_exclusive(ep);
                let rhs = self.eval(value);
                self.end_exclusive(claim);
                let rhs = rhs?;
                let new = self.arith(op, old, rhs, span)?;
                self.assign(ep, new.clone())?;
                Ok(new)
            }
        }
    }

    // ---------- calls ----------

    pub(crate) fn call_target(
        &mut self,
        target: &CallTarget,
        args: &[Arg],
        span: Span,
    ) -> R<Value> {
        match target {
            CallTarget::Func(fid) => self.invoke(*fid, None, args, span),
            CallTarget::Parent { func, receiver } => {
                let this = match receiver {
                    Some(r) => Some(self.receiver_value(r)?),
                    None => None,
                };
                self.invoke(*func, this, args, span)
            }
            CallTarget::Method {
                receiver,
                name,
                func,
            } => match &**receiver {
                Receiver::Value(e) => {
                    let recv = self.eval(e)?;
                    let f = self.dispatch(&recv, name, *func);
                    self.invoke(f, Some(recv), args, span)
                }
                Receiver::Place(p) => {
                    let ep = self.eval_place(p)?;
                    let probe = self.read(&ep)?;
                    let f = self.dispatch(&probe, name, *func);
                    drop(probe);
                    self.invoke_on_place(f, &ep, args, span)
                }
            },
            CallTarget::Builtin {
                func: fid,
                receiver,
                ..
            } => {
                let fid = *fid;
                match receiver.as_deref() {
                    None => self.invoke(fid, None, args, span),
                    Some(Receiver::Value(e)) => {
                        let recv = self.eval(e)?;
                        self.invoke(fid, Some(recv), args, span)
                    }
                    Some(Receiver::Place(p)) => {
                        let ep = self.eval_place(p)?;
                        self.invoke_on_place(fid, &ep, args, span)
                    }
                }
            }
            CallTarget::Value(callee) => {
                let f = self.eval(callee)?;
                let Value::Func(fv) = &f else {
                    return Err(Flow::Fatal("called a value that is not a function".into()));
                };
                let (fid, this, captures): (FuncId, Option<Value>, Vec<_>) = match &**fv {
                    FuncValue::Closure { func, captures } => (*func, None, captures.clone()),
                    FuncValue::Func(func) => (*func, None, Vec::new()),
                    FuncValue::Method { receiver, func } => {
                        (*func, Some(receiver.clone()), Vec::new())
                    }
                    FuncValue::Builtin { func, receiver } => (*func, receiver.clone(), Vec::new()),
                };
                let (vals, inouts) = self.eval_args(fid, args)?;
                let (r, mut end) = self.call_func(fid, this, vals, &captures, span);
                self.write_back(inouts, &mut end)?;
                r
            }
        }
    }

    /// A builtin call: the call's type is handed to the native as `NativeCall::ret`,
    /// set just before the native runs (after the arguments are evaluated).
    fn call_builtin_hinted(
        &mut self,
        target: &CallTarget,
        args: &[Arg],
        ret: scb_types::ty::Ty,
        span: Span,
    ) -> R<Value> {
        let CallTarget::Builtin {
            func: fid,
            receiver,
            ..
        } = target
        else {
            unreachable!("checked by the caller")
        };
        let fid = *fid;
        let this = match receiver.as_deref() {
            None => None,
            Some(Receiver::Value(e)) => Some(self.eval(e)?),
            Some(Receiver::Place(p)) => {
                let ep = self.eval_place(p)?;
                let (vals, inouts) = self.eval_args(fid, args)?;
                let recv = self.take(&ep)?;
                let claim = self.begin_exclusive(&ep);
                self.iso.ret_hint = Some(ret);
                let (r, mut end) = self.call_func(fid, Some(recv), vals, &[], span);
                self.iso.ret_hint = None;
                self.end_exclusive(claim);
                let this = end.this.take().unwrap_or_default();
                self.assign(&ep, this)?;
                self.write_back(inouts, &mut end)?;
                return r;
            }
        };
        let (vals, inouts) = self.eval_args(fid, args)?;
        self.iso.ret_hint = Some(ret);
        let (r, mut end) = self.call_func(fid, this, vals, &[], span);
        self.iso.ret_hint = None;
        self.write_back(inouts, &mut end)?;
        r
    }

    fn receiver_value(&mut self, r: &Receiver) -> R<Value> {
        match r {
            Receiver::Value(e) => self.eval(e),
            Receiver::Place(p) => {
                let ep = self.eval_place(p)?;
                self.read(&ep)
            }
        }
    }

    /// Evaluates arguments and calls; writes `inout` arguments back.
    fn invoke(&mut self, fid: FuncId, this: Option<Value>, args: &[Arg], span: Span) -> R<Value> {
        let (vals, inouts) = self.eval_args(fid, args)?;
        let (r, mut end) = self.call_func(fid, this, vals, &[], span);
        self.write_back(inouts, &mut end)?;
        r
    }

    /// A `mutating` call: the arguments are evaluated first, then the receiver is
    /// moved out of its place, and the final `$this` written back (also after a throw).
    fn invoke_on_place(&mut self, fid: FuncId, ep: &EPlace, args: &[Arg], span: Span) -> R<Value> {
        let (vals, inouts) = self.eval_args(fid, args)?;
        let recv = self.take(ep)?;
        let claim = self.begin_exclusive(ep);
        let (r, mut end) = self.call_func(fid, Some(recv), vals, &[], span);
        self.end_exclusive(claim);
        let this = end.this.take().unwrap_or_default();
        self.assign(ep, this)?;
        self.write_back(inouts, &mut end)?;
        r
    }

    fn callable_ref(&mut self, target: &CallTarget) -> R<Value> {
        let fv = match target {
            CallTarget::Func(f) => FuncValue::Func(*f),
            CallTarget::Method {
                receiver,
                name,
                func,
            } => {
                let recv = self.receiver_value(receiver)?;
                let func = self.dispatch(&recv, name, *func);
                FuncValue::Method {
                    receiver: recv,
                    func,
                }
            }
            CallTarget::Builtin { func, receiver, .. } => FuncValue::Builtin {
                func: *func,
                receiver: match receiver.as_deref() {
                    Some(r) => Some(self.receiver_value(r)?),
                    None => None,
                },
            },
            CallTarget::Parent { func, .. } => FuncValue::Func(*func),
            CallTarget::Value(e) => return self.eval(e),
        };
        Ok(Value::Func(Rc::new(fv)))
    }

    /// `new C(…)`: arguments first, then the instance (defaults of every class in
    /// the hierarchy, base first), then the constructor.
    fn instantiate(
        &mut self,
        class: DeclId,
        ctor: Option<FuncId>,
        args: &[Arg],
        span: Span,
    ) -> R<Value> {
        let (vals, inouts) = match ctor {
            Some(f) => self.eval_args(f, args)?,
            None => (Vec::new(), Vec::new()),
        };
        let program = self.program;
        let decl = program.decls.ty(class);
        let n = decl.fields.len();
        let mut obj = if decl.kind == TypeKind::Struct {
            Value::Struct(Rc::new(StructValue {
                decl: class,
                fields: vec![Value::Null; n],
            }))
        } else {
            let native = self.is_throwable(class).then(|| {
                let (file, line) = self.position(span);
                NativeState::Throwable(ThrowableState {
                    message: String::new(),
                    code: 0,
                    previous: None,
                    file,
                    line,
                    trace: self.trace(span),
                })
            });
            Value::Object(Rc::new(RefCell::new(ObjectValue {
                decl: class,
                fields: vec![Value::Null; n],
                native,
                destructor: self.destructor_queue(class),
            })))
        };
        for a in program.decls.ancestors(class).into_iter().rev() {
            for (idx, init) in &program.field_defaults[a.0 as usize] {
                match self.eval_init(init) {
                    Ok(v) => super::set_field(&mut obj, *idx, v),
                    Err(e) => {
                        // Put `inout` arguments back before giving up.
                        let mut end = super::CallEnd {
                            this: None,
                            params: vals.into_iter().map(Option::unwrap_or_default).collect(),
                        };
                        self.write_back(inouts, &mut end)?;
                        return Err(e);
                    }
                }
            }
        }
        let Some(ctor) = ctor else {
            return Ok(obj);
        };
        let (r, mut end) = self.call_func(ctor, Some(obj), vals, &[], span);
        self.write_back(inouts, &mut end)?;
        r?;
        Ok(end.this.unwrap_or_default())
    }

    pub(crate) fn is_throwable(&self, class: DeclId) -> bool {
        let decls = &self.program.decls;
        decls
            .type_named("Throwable")
            .is_some_and(|t| decls.is_subtype(class, t))
    }

    // ---------- enums ----------

    fn case_value(&mut self, v: &Value) -> R<Value> {
        let Value::Enum(e) = v else {
            return Ok(Value::Null);
        };
        let program = self.program;
        match &program.case_values[e.decl.0 as usize][e.case as usize] {
            Some(init) => self.eval_init(init),
            None => Ok(Value::Null),
        }
    }

    fn enum_from(&mut self, decl: DeclId, v: &Value, try_: bool, span: Span) -> R<Value> {
        let program = self.program;
        for (i, init) in program.case_values[decl.0 as usize].iter().enumerate() {
            if let Some(init) = init {
                let cv = self.eval_init(init)?;
                if super::ops::values_equal(&cv, v) {
                    return Ok(enum_value(decl, i as u32, Vec::new()));
                }
            }
        }
        if try_ {
            return Ok(Value::Null);
        }
        let shown = self.display(v)?;
        let name = program.decls.ty(decl).name.clone();
        Err(self.error(
            "ValueError",
            format!("{shown} is not a value of `{name}`"),
            span,
        ))
    }
}

fn enum_value(decl: DeclId, case: u32, payload: Vec<Value>) -> Value {
    Value::Enum(Rc::new(EnumValue {
        decl,
        case,
        payload,
    }))
}
