//! The interpreter: walks the typed IR (`scb_types::tir`) of a checked program.
//!
//! # Control flow
//!
//! Every evaluation returns `R<T> = Result<T, Flow>`. `Flow` carries everything that
//! leaves an expression early: a thrown SCB value, `exit`, `return`, `break`,
//! `continue`, and fatal errors. Rust panics never carry SCB semantics (a panic is an
//! interpreter bug; CLAUDE.md).
//!
//! # State
//!
//! All state lives in the [`Isolate`] (frames, the call stack, statics, constant
//! values, I/O); an [`Interp`] is a short-lived view pairing an isolate with the
//! immutable [`Program`] it runs.
//!
//! # Calls
//!
//! A frame is a vector of local slots, sized by the function's TIR. Arguments are
//! evaluated left to right as written and placed into parameter slots; omitted
//! parameters take their default; `inout` arguments move the value out of the
//! caller's place and write the final parameter value back after the call (also
//! after a throw), so the callee always sees an unshared value and mutates it in
//! place. `mutating` receivers work the same way.
//!
//! The submodules: `expr` (expressions), `stmt` (statements), `place` (reading and
//! writing places, copy-on-write), `ops` (arithmetic, conversions, equality,
//! ordering, hashing), `pattern` (patterns and runtime type tests).

mod cycles;
mod destruct;
mod exclusive;
mod expr;
mod ops;
mod pattern;
mod place;
mod stmt;

use std::collections::HashMap;
use std::rc::Rc;

use scb_syntax::Span;
use scb_types::Program;
use scb_types::decls::{DeclId, MethodId};
use scb_types::tir::{self, Arg, ArgValue, Body, FuncId, LocalId};
use scb_types::ty::Ty;

pub(crate) use exclusive::Exclusive;
pub use ops::{hash_value, values_equal};

use crate::isolate::{Frame, Isolate, StaticSlot};
use crate::value::{NativeState, ObjectValue, ThrowableState, Value};
use place::EPlace;

/// Why evaluation stopped early.
#[derive(Debug)]
pub enum Flow {
    /// A thrown throwable (an `Object`) and where it was thrown (the `throw`, or
    /// the operation that trapped).
    Throw(Value, Span),
    /// `exit(code)` / `Process\exit` and where it was called: unwinds, runs
    /// `finally` blocks, not catchable.
    Exit(i32, Span),
    Return(Value),
    /// `break n` / `continue n` (`n ≥ 1` loops to leave).
    Break(u32),
    Continue(u32),
    /// Not catchable, ends the program (stack overflow, …).
    Fatal(String),
    /// `Process\abort()`: ends immediately: no `finally`, no stdout flush.
    Abort,
}

pub type R<T> = Result<T, Flow>;

/// A natively implemented core-library function or method.
///
/// `receiver` is the method's receiver (`Null` for functions); a `mutating` method
/// changes it in place and the interpreter writes it back. `args` holds one value
/// per parameter, defaults filled in.
pub type NativeFn = fn(&mut Interp<'_>, &mut NativeCall) -> R<Value>;

/// The core library's native functions by name (`list.push`, `Io\print`, …).
pub type Natives = HashMap<String, NativeFn>;

pub struct NativeCall {
    pub receiver: Value,
    pub args: Vec<Value>,
    pub span: Span,
    /// The call's result type, with type parameters resolved (`f64` for `sum()` on a
    /// `list<f64>`, `i8` for `i8::wrap`).
    pub ret: Ty,
}

/// Maximum depth of SCB calls. Each SCB call uses several Rust frames, so this also
/// bounds the Rust stack (the CLI runs programs on a large-stack thread).
const MAX_CALL_DEPTH: usize = 10_000;

/// An interpreter run: an isolate plus the program it executes.
pub struct Interp<'a> {
    pub iso: &'a mut Isolate,
    pub program: &'a Program,
}

/// An `inout` argument: the parameter, the caller's place its value was moved out
/// of, and the exclusivity claim on that place (released at write-back).
pub(crate) struct Inout {
    param: usize,
    place: EPlace,
    claim: Option<u64>,
}

/// Arguments by parameter index (`None` = omitted), and the `inout` arguments.
pub(crate) type EvaluatedArgs = (Vec<Option<Value>>, Vec<Inout>);

/// The final state of a call: `$this` after it (structs) and the parameters (for
/// `inout` write-back).
pub(crate) struct CallEnd {
    pub this: Option<Value>,
    pub params: Vec<Value>,
}

impl<'a> Interp<'a> {
    /// Starts a run: per-program state (statics, constants, the dispatch cache, the
    /// stack) starts fresh, so an isolate reused for another run or another program
    /// never sees stale values.
    pub fn new(iso: &'a mut Isolate, program: &'a Program) -> Self {
        // What an earlier run left is freed without its destructors.
        iso.free_leftovers();
        iso.statics = program.statics.iter().map(|_| StaticSlot::Uninit).collect();
        iso.consts = program.consts.iter().map(|_| None).collect();
        iso.vtables.clear();
        iso.destructors.clear();
        iso.static_order.clear();
        iso.calls.clear();
        iso.ret_hint = None;
        iso.destruct_floor = 0;
        iso.gc_prune_at = cycles::MIN_THRESHOLD;
        iso.gc_created = 0;
        iso.gc_threshold = cycles::MIN_THRESHOLD;
        iso.gc_running = false;
        Interp { iso, program }
    }

    // ---------- frames ----------

    pub(crate) fn locals(&mut self) -> &mut Vec<Value> {
        &mut self
            .iso
            .frames
            .last_mut()
            .expect("a frame is active")
            .locals
    }

    pub(crate) fn local(&mut self, id: LocalId) -> &mut Value {
        &mut self.locals()[id.0 as usize]
    }

    /// Evaluates an initializer (constant, static, default, case value) in a frame of
    /// its own.
    pub(crate) fn eval_init(&mut self, init: &tir::Init) -> R<Value> {
        self.iso.frames.push(Frame {
            locals: vec![Value::Null; init.locals.len()],
        });
        let r = self.eval(&init.expr);
        self.iso.frames.pop();
        r
    }

    pub(crate) fn constant(&mut self, id: scb_types::decls::ConstId) -> R<Value> {
        if let Some(v) = &self.iso.consts[id.0 as usize] {
            return Ok(v.clone());
        }
        let v = self.eval_init(&self.program.consts[id.0 as usize])?;
        self.iso.consts[id.0 as usize] = Some(v.clone());
        Ok(v)
    }

    // ---------- calls ----------

    /// Calls `fid` with arguments by parameter index (`None` = omitted). Returns the
    /// result and the final `$this` / parameter values, which are available even
    /// when the call threw (`inout` write-back).
    pub(crate) fn call_func(
        &mut self,
        fid: FuncId,
        this: Option<Value>,
        mut args: Vec<Option<Value>>,
        captures: &[(LocalId, Value)],
        span: Span,
    ) -> (R<Value>, CallEnd) {
        let func = &self.program.funcs[fid.0 as usize];
        args.resize(func.params.len(), None);
        if self.iso.calls.len() >= MAX_CALL_DEPTH {
            let end = CallEnd {
                this,
                params: args.into_iter().map(Option::unwrap_or_default).collect(),
            };
            return (
                Err(Flow::Fatal(format!(
                    "stack overflow: more than {MAX_CALL_DEPTH} nested calls (in {})",
                    func.name
                ))),
                end,
            );
        }
        self.iso.calls.push((fid, span));
        self.iso.frames.push(Frame {
            locals: vec![Value::Null; func.locals.len()],
        });
        // Values move into the frame and back out: a receiver or `inout` argument
        // that arrives unshared stays unshared, so the callee mutates it in place.
        let mut this = this;
        if let Some(slot) = func.this {
            *self.local(slot) = this.take().unwrap_or_default();
        }
        for (slot, v) in captures {
            *self.local(*slot) = v.clone();
        }
        let result = self.bind_and_run(func, args);
        let mut frame = self.iso.frames.pop().expect("pushed above");
        self.iso.calls.pop();
        let end = CallEnd {
            this: match func.this {
                Some(t) => Some(std::mem::take(&mut frame.locals[t.0 as usize])),
                None => this,
            },
            params: func
                .params
                .iter()
                .map(|p| std::mem::take(&mut frame.locals[p.local.0 as usize]))
                .collect(),
        };
        // The remaining locals go in reverse declaration order (SPEC "Destructors").
        while let Some(v) = frame.locals.pop() {
            drop(v);
        }
        (result, end)
    }

    fn bind_and_run(&mut self, func: &'a tir::Func, args: Vec<Option<Value>>) -> R<Value> {
        // Taken first: a default below may itself call a builtin and set its own.
        let ret_hint = self.iso.ret_hint.take();
        // Parameters, with defaults evaluated in the callee's frame.
        for (p, a) in func.params.iter().zip(args) {
            let v = match a {
                Some(v) => v,
                None if p.variadic => Value::list(Vec::new()),
                None => match &p.default {
                    Some(d) => self.eval(d)?,
                    None => Value::Null,
                },
            };
            *self.local(p.local) = v;
        }
        // Constructor promotion: parameters become properties before the body.
        if let Some(this) = func.this {
            for p in &func.params {
                if let Some(field) = p.promote {
                    let v = self.local(p.local).clone();
                    let mut target = std::mem::take(self.local(this));
                    set_field(&mut target, field, v);
                    *self.local(this) = target;
                }
            }
        }
        match &func.body {
            Body::Block(b) => match self.exec_block(b) {
                Ok(()) => Ok(Value::Null),
                Err(Flow::Return(v)) => Ok(v),
                Err(e) => Err(e),
            },
            Body::Expr(e) => self.eval(e),
            Body::Builtin(b) => {
                let Some(native) = self.iso.natives.get(b.0.as_str()).copied() else {
                    return Err(Flow::Fatal(format!(
                        "native function `{}` is not implemented",
                        b.0
                    )));
                };
                let receiver = func
                    .this
                    .map_or(Value::Null, |t| std::mem::take(self.local(t)));
                let args = func
                    .params
                    .iter()
                    .map(|p| std::mem::take(self.local(p.local)))
                    .collect();
                let span = self.iso.calls.last().map_or(func.span, |c| c.1);
                // The call site's resolved type, else the stub's declared one.
                let ret = ret_hint.unwrap_or(func.ret);
                let mut call = NativeCall {
                    receiver,
                    args,
                    span,
                    ret,
                };
                let r = native(self, &mut call);
                if let Some(t) = func.this {
                    *self.local(t) = call.receiver;
                }
                for (p, v) in func.params.iter().zip(call.args) {
                    *self.local(p.local) = v;
                }
                r
            }
            Body::Abstract => Err(Flow::Fatal(format!(
                "abstract method `{}` called",
                func.name
            ))),
        }
    }

    /// Evaluates call arguments in written order. Returns the values by parameter
    /// index and the `inout` places (with the values moved out of them).
    pub(crate) fn eval_args(&mut self, func: FuncId, args: &[Arg]) -> R<EvaluatedArgs> {
        let params = &self.program.funcs[func.0 as usize].params;
        let mut out: Vec<Option<Value>> = vec![None; params.len()];
        let mut variadic: Option<(usize, Vec<Value>)> = None;
        let mut inouts = Vec::new();
        for a in args {
            let i = a.param as usize;
            let is_variadic = params.get(i).is_some_and(|p| p.variadic);
            match &a.value {
                ArgValue::Value(e) => {
                    let v = self.eval(e)?;
                    if is_variadic {
                        variadic.get_or_insert((i, Vec::new())).1.push(v);
                    } else {
                        out[i] = Some(v);
                    }
                }
                ArgValue::Spread(e) => {
                    let v = self.eval(e)?;
                    if let Value::List(items) = v {
                        variadic
                            .get_or_insert((i, Vec::new()))
                            .1
                            .extend(items.iter().cloned());
                    }
                }
                ArgValue::Inout(p) => inouts.push(Inout {
                    param: i,
                    place: self.eval_place(p)?,
                    claim: None,
                }),
            }
        }
        if let Some((i, items)) = variadic {
            out[i] = Some(Value::list(items));
        }
        // Move `inout` values out only now: a by-value argument naming the same
        // variable has been read first (SPEC "Exclusivity": `f(inout $a, $a[0])`).
        // Each claims its storage until written back, so two `inout` arguments
        // reaching the same class property conflict (SPEC "Exclusivity"). If one
        // cannot be taken (an index out of range, a conflict), the ones already taken
        // are put back before the error propagates.
        for n in 0..inouts.len() {
            match self.take(&inouts[n].place) {
                Ok(v) => {
                    out[inouts[n].param] = Some(v);
                    inouts[n].claim = self.begin_exclusive(&inouts[n].place);
                }
                Err(e) => {
                    for a in inouts.iter().take(n) {
                        self.end_exclusive(a.claim);
                        let v = out[a.param].take().unwrap_or_default();
                        let _ = self.assign(&a.place, v);
                    }
                    return Err(e);
                }
            }
        }
        Ok((out, inouts))
    }

    /// Gives evaluated arguments back when the call does not happen after all (its
    /// receiver or instance could not be prepared): `inout` values return to their
    /// places and their claims end.
    pub(crate) fn abandon_args(&mut self, vals: Vec<Option<Value>>, inouts: Vec<Inout>) -> R<()> {
        let mut end = CallEnd {
            this: None,
            params: vals.into_iter().map(Option::unwrap_or_default).collect(),
        };
        self.write_back(inouts, &mut end)
    }

    /// Writes final `inout` parameter values back into the caller's places.
    pub(crate) fn write_back(&mut self, inouts: Vec<Inout>, end: &mut CallEnd) -> R<()> {
        for a in &inouts {
            self.end_exclusive(a.claim);
        }
        for a in inouts {
            let v = std::mem::take(&mut end.params[a.param]);
            self.assign(&a.place, v)?;
        }
        Ok(())
    }

    /// The method `name` as implemented by the runtime type of `receiver` (dynamic
    /// dispatch), or `fallback` (the statically resolved method) for values without a
    /// declaration. Cached per (runtime type, static method).
    pub(crate) fn dispatch(&mut self, receiver: &Value, name: &str, fallback: FuncId) -> FuncId {
        let Some(decl) = receiver.decl() else {
            return fallback;
        };
        if let Some(&f) = self.iso.vtables.get(&(decl, fallback)) {
            return f;
        }
        let f = self
            .program
            .decls
            .find_method(decl, name)
            .map_or(fallback, |m: MethodId| self.program.decls.method(m).func);
        self.iso.vtables.insert((decl, fallback), f);
        f
    }

    /// Calls a function value with positional arguments (callbacks from natives).
    pub fn call_value(&mut self, f: &Value, args: Vec<Value>, span: Span) -> R<Value> {
        let Value::Func(fv) = f else {
            return Err(Flow::Fatal("called a value that is not a function".into()));
        };
        let args: Vec<Option<Value>> = args.into_iter().map(Some).collect();
        match &**fv {
            crate::value::FuncValue::Closure { func, captures } => {
                self.call_func(*func, None, args, captures, span).0
            }
            crate::value::FuncValue::Func(func) => self.call_func(*func, None, args, &[], span).0,
            crate::value::FuncValue::Method { receiver, func } => {
                self.call_func(*func, Some(receiver.clone()), args, &[], span)
                    .0
            }
            crate::value::FuncValue::Builtin {
                func,
                receiver,
                ret,
            } => {
                self.iso.ret_hint = Some(*ret);
                self.call_func(*func, receiver.clone(), args, &[], span).0
            }
        }
    }

    /// Calls a method of an object by name (`toString`, `equals`, …).
    pub fn call_method(
        &mut self,
        receiver: Value,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> R<Value> {
        let Some(decl) = receiver.decl() else {
            return Err(Flow::Fatal(format!(
                "`{name}` called on a value without methods"
            )));
        };
        let Some(m) = self.program.decls.find_method(decl, name) else {
            return Err(Flow::Fatal(format!("no method `{name}`")));
        };
        let func = self.program.decls.method(m).func;
        let args = args.into_iter().map(Some).collect();
        self.call_func(func, Some(receiver), args, &[], span).0
    }

    /// `exit($code)`: a code outside 0–255 raises `ValueError`, as a `main` return
    /// does; it is never truncated (SPEC "Source files and program entry").
    pub fn exit_with(&mut self, code: i64, span: Span) -> Flow {
        match u8::try_from(code) {
            Ok(c) => Flow::Exit(i32::from(c), span),
            Err(_) => self.error(
                "ValueError",
                format!("exit({code}): exit codes are 0 to 255"),
                span,
            ),
        }
    }

    // ---------- errors ----------

    /// A new throwable of class `class` (a prelude class such as `IndexError`).
    pub fn error(&mut self, class: &str, message: impl Into<String>, span: Span) -> Flow {
        let decl = self
            .program
            .decls
            .type_named(class)
            .expect("throwable classes are in the prelude");
        Flow::Throw(self.new_throwable(decl, message.into(), span), span)
    }

    pub(crate) fn new_throwable(&mut self, decl: DeclId, message: String, span: Span) -> Value {
        let n = self.program.decls.ty(decl).fields.len();
        let (file, line) = self.position(span);
        let trace = self.trace(span);
        let v = Value::Object(Rc::new(std::cell::RefCell::new(ObjectValue::new(
            decl,
            vec![Value::Null; n],
            Some(NativeState::Throwable(ThrowableState {
                message,
                code: 0,
                previous: None,
                file,
                line,
                trace,
            })),
        ))));
        self.track(&v);
        v
    }

    pub fn position(&self, span: Span) -> (String, i64) {
        let f = self.program.sources.file(span.file);
        (
            f.path.display().to_string(),
            i64::from(f.line_col(span.start).0),
        )
    }

    /// The call stack, innermost first: `function (file:line)` per frame.
    pub(crate) fn trace(&self, span: Span) -> Vec<String> {
        let mut out = Vec::new();
        let mut at = span;
        for &(fid, call) in self.iso.calls.iter().rev() {
            let (file, line) = self.position(at);
            out.push(format!(
                "{} ({file}:{line})",
                self.program.funcs[fid.0 as usize].name
            ));
            at = call;
        }
        out
    }

    /// Runs `main`. Returns the process exit code (SPEC "Source files and program
    /// entry").
    pub fn run_main(&mut self, args: &[Vec<u8>]) -> i32 {
        let main = &self.program.funcs[self.program.main.0 as usize];
        let span = main.span;
        let mut values = Vec::new();
        for (i, a) in args.iter().enumerate() {
            match String::from_utf8(a.clone()) {
                Ok(s) => values.push(Value::str(s)),
                Err(_) => {
                    // Reported before `main` runs, like an uncaught EncodingException.
                    let e = self.error(
                        "EncodingException",
                        format!("command-line argument {} is not valid UTF-8", i + 1),
                        span,
                    );
                    return self.finish(Err(e));
                }
            }
        }
        let call_args = if main.params.is_empty() {
            Vec::new()
        } else {
            vec![Some(Value::list(values))]
        };
        let (result, _) = self.call_func(self.program.main, None, call_args, &[], span);
        let result = match result {
            Ok(Value::I64(code)) if !(0..=255).contains(&code) => Err(self.error(
                "ValueError",
                format!("main returned {code}; exit codes are 0 to 255"),
                span,
            )),
            other => other,
        };
        // Program end (SPEC "Destructors"): `main`'s locals are gone; the statics
        // follow, on every end but `abort` and fatal errors. After an uncaught
        // throwable they follow its trace (in `finish`).
        let result = match result {
            Err(Flow::Abort | Flow::Fatal(_) | Flow::Throw(..)) => result,
            _ => match self.end_of_program() {
                Ok(()) => result,
                Err(fatal) => Err(fatal),
            },
        };
        self.finish(result)
    }

    /// Program end: pending destructors, unreachable cycles, then the statics and
    /// the cycles only they kept alive.
    fn end_of_program(&mut self) -> R<()> {
        self.run_destructors()?;
        self.collect_cycles()?;
        self.release_statics()?;
        self.collect_cycles()
    }

    /// Flushes stdout and turns the outcome of `main` into an exit code, printing an
    /// uncaught throwable's trace (SPEC: stdout is flushed before the trace).
    fn finish(&mut self, result: R<Value>) -> i32 {
        if let Err(Flow::Abort) = result {
            // SPEC: `Process\abort()` does not flush stdout.
            self.iso.discard_stdout();
            return 134;
        }
        // SPEC: a failed final flush is reported and the exit code becomes 1.
        let flush_failed = match self.iso.flush_stdout() {
            Ok(()) => false,
            Err(e) => {
                let msg = format!("Fatal error: cannot write to stdout: {e}\n");
                let _ = self.iso.write_stderr(msg.as_bytes());
                // Nothing more can be written; do not retry at the end of `run`.
                self.iso.discard_stdout();
                true
            }
        };
        let code = match result {
            Ok(Value::I64(code)) => code as i32,
            Ok(_) => 0,
            Err(Flow::Exit(code, _)) => code,
            Err(Flow::Throw(e, _)) => {
                let text = self.describe_throwable(&e, "Uncaught");
                let _ = self.iso.write_stderr(text.as_bytes());
                drop(e);
                // The statics go after the trace (SPEC "Destructors").
                let teardown = self.end_of_program();
                let _ = self.iso.flush_stdout();
                if let Err(Flow::Fatal(msg)) = teardown {
                    let _ = self
                        .iso
                        .write_stderr(format!("Fatal error: {msg}\n").as_bytes());
                }
                1
            }
            Err(Flow::Fatal(msg)) => {
                let _ = self
                    .iso
                    .write_stderr(format!("Fatal error: {msg}\n").as_bytes());
                1
            }
            Err(Flow::Return(_) | Flow::Break(_) | Flow::Continue(_) | Flow::Abort) => 0,
        };
        if flush_failed { 1 } else { code }
    }

    /// `Uncaught Class: message`, the position, the trace and the `previous` chain.
    pub(crate) fn describe_throwable(&self, e: &Value, heading: &str) -> String {
        let mut out = String::new();
        let mut cur = Some(e.clone());
        let mut first = true;
        let mut seen: Vec<*const std::cell::RefCell<ObjectValue>> = Vec::new();
        while let Some(Value::Object(o)) = cur {
            // `previous` chains can loop (a `finally` can link two pending throwables).
            if seen.contains(&Rc::as_ptr(&o)) {
                break;
            }
            seen.push(Rc::as_ptr(&o));
            let o = o.borrow();
            let class = &self.program.decls.ty(o.decl).name;
            let Some(NativeState::Throwable(t)) = o.native.as_deref() else {
                break;
            };
            let head = if first { heading } else { "Caused by" };
            out.push_str(&format!(
                "{head} {class}: {}\n  at {}:{}\n",
                t.message, t.file, t.line
            ));
            for (i, frame) in t.trace.iter().enumerate() {
                out.push_str(&format!("  #{i} {frame}\n"));
            }
            first = false;
            cur = t.previous.clone();
        }
        out
    }
}

/// Stores `v` into field `idx` of a struct or object value.
pub(crate) fn set_field(target: &mut Value, idx: u32, v: Value) {
    match target {
        Value::Struct(s) => Rc::make_mut(s).fields[idx as usize] = v,
        Value::Object(o) => o.borrow_mut().fields[idx as usize] = v,
        _ => {}
    }
}
