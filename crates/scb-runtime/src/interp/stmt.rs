//! Statements.

use std::rc::Rc;

use scb_types::tir::{ArmBody, Block, Catch, IterKind, Match, Stmt, StmtKind};

use super::{Flow, Interp, R};
use crate::value::{NativeState, Value};

impl Interp<'_> {
    /// Runs a block, then releases the locals it declared in reverse declaration
    /// order and runs the destructors that became due (SPEC "Destructors": also
    /// while a throwable or `exit` unwinds; not after `abort` or a fatal error).
    pub(crate) fn exec_block(&mut self, b: &Block) -> R<()> {
        let r = self.exec_stmts(&b.stmts);
        for l in b.locals.iter().rev() {
            *self.local(*l) = Value::Null;
        }
        if matches!(r, Err(Flow::Abort | Flow::Fatal(_))) || !self.iso.has_pending_destructors() {
            return r;
        }
        self.run_destructors()?;
        r
    }

    /// Statements in order; values released by a statement (an overwritten
    /// variable, a temporary) are destructed before the next one.
    // SPEC-OPEN: Q41 (temporaries: end of the statement, not mid-expression)
    fn exec_stmts(&mut self, stmts: &[Stmt]) -> R<()> {
        for s in stmts {
            self.exec(s)?;
            if self.iso.has_pending_destructors() {
                self.run_destructors()?;
            }
        }
        Ok(())
    }

    fn exec(&mut self, s: &Stmt) -> R<()> {
        match &s.kind {
            StmtKind::Expr(e) => {
                self.eval(e)?;
            }
            StmtKind::Let { local, value } => {
                let v = self.eval(value)?;
                *self.local(*local) = v;
            }
            StmtKind::Destructure { pattern, value } => {
                let v = self.eval(value)?;
                self.match_pattern(pattern, &v)?;
            }
            StmtKind::Block(b) => self.exec_block(b)?,
            StmtKind::If {
                cond,
                then,
                otherwise,
            } => {
                if self.eval(cond)?.as_bool() {
                    self.exec_block(then)?;
                } else if let Some(b) = otherwise {
                    self.exec_block(b)?;
                }
            }
            StmtKind::While { cond, body } => {
                while self.eval(cond)?.as_bool() {
                    if self.loop_body(body)? {
                        break;
                    }
                }
            }
            StmtKind::DoWhile { body, cond } => loop {
                if self.loop_body(body)? || !self.eval(cond)?.as_bool() {
                    break;
                }
            },
            StmtKind::For {
                init,
                cond,
                step,
                body,
                scope,
            } => {
                let r = self.for_loop(init, cond.as_ref(), step, body);
                for l in scope {
                    *self.local(*l) = Value::Null;
                }
                r?;
            }
            StmtKind::Foreach {
                subject,
                iter,
                key,
                value,
                body,
            } => {
                // The loop iterates a snapshot (SPEC "Exclusivity"): changing the
                // collection in the body copies it instead.
                let coll = self.eval(subject)?;
                let emit = |it: &mut Self, k: Value, v: Value| -> R<bool> {
                    if let Some(kl) = key {
                        *it.local(*kl) = k;
                    }
                    it.match_pattern(value, &v)?;
                    it.loop_body(body)
                };
                match (iter, &coll) {
                    (IterKind::List, Value::List(items)) => {
                        for (i, v) in items.iter().enumerate() {
                            if emit(self, Value::I64(i as i64), v.clone())? {
                                break;
                            }
                        }
                    }
                    (IterKind::Map, Value::Map(m)) => {
                        for (k, v) in m.iter() {
                            if emit(self, k.clone(), v.clone())? {
                                break;
                            }
                        }
                    }
                    (IterKind::Set, Value::Set(m)) => {
                        for (k, _) in m.iter() {
                            if emit(self, Value::Null, k.clone())? {
                                break;
                            }
                        }
                    }
                    (IterKind::Range, Value::Range(r)) => {
                        for i in 0..r.count {
                            if emit(self, Value::Null, Value::int(r.elem, r.get(i)))? {
                                break;
                            }
                        }
                    }
                    (IterKind::Bytes, Value::Bytes(b)) => {
                        for (i, x) in b.iter().enumerate() {
                            if emit(self, Value::I64(i as i64), Value::U8(*x))? {
                                break;
                            }
                        }
                    }
                    _ => {
                        return Err(Flow::Fatal(
                            "internal error: `foreach` over a value of the wrong kind".into(),
                        ));
                    }
                }
                for l in &body.locals {
                    *self.local(*l) = Value::Null;
                }
            }
            StmtKind::Try {
                body,
                catches,
                finally,
            } => self.try_stmt(body, catches, finally.as_ref())?,
            StmtKind::Match(m) => {
                self.eval_match(m)?;
            }
            StmtKind::Print(e) => {
                let v = self.eval(e)?;
                // SPEC: a failed write throws `IoException`, as `Io\print` does.
                if let Err(err) = self.iso.write_stdout(v.as_str().as_bytes()) {
                    return Err(self.error("IoException", err.to_string(), s.span));
                }
            }
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e)?,
                    None => Value::Null,
                };
                return Err(Flow::Return(v));
            }
            StmtKind::Break(n) => return Err(Flow::Break(*n)),
            StmtKind::Continue(n) => return Err(Flow::Continue(*n)),
        }
        Ok(())
    }

    /// One iteration of a loop body. `Ok(true)` means "leave this loop"; `break n` /
    /// `continue n` for outer loops propagate with `n - 1`.
    fn loop_body(&mut self, body: &Block) -> R<bool> {
        match self.exec_block(body) {
            Ok(()) | Err(Flow::Continue(1)) => Ok(false),
            Err(Flow::Break(1)) => Ok(true),
            Err(Flow::Break(n)) => Err(Flow::Break(n - 1)),
            Err(Flow::Continue(n)) => Err(Flow::Continue(n - 1)),
            Err(e) => Err(e),
        }
    }

    fn for_loop(
        &mut self,
        init: &[Stmt],
        cond: Option<&scb_types::tir::Expr>,
        step: &[scb_types::tir::Expr],
        body: &Block,
    ) -> R<()> {
        self.exec_stmts(init)?;
        loop {
            if let Some(c) = cond
                && !self.eval(c)?.as_bool()
            {
                return Ok(());
            }
            if self.loop_body(body)? {
                return Ok(());
            }
            for s in step {
                self.eval(s)?;
            }
        }
    }

    /// `try` / `catch` / `finally` (SPEC "Errors"). `exit` is never caught but runs
    /// `finally`. Whatever leaves a `finally` replaces the pending outcome; a
    /// replaced throwable is reported on stderr, never dropped silently, and becomes
    /// the `previous` of a new throwable that has none.
    fn try_stmt(&mut self, body: &Block, catches: &[Catch], finally: Option<&Block>) -> R<()> {
        let mut result = self.exec_block(body);
        // `Process\abort()` and fatal errors end the program at once: no `catch`, no
        // `finally` (SPEC "Source files and program entry", "Errors").
        if matches!(result, Err(Flow::Abort | Flow::Fatal(_))) {
            return result;
        }
        if let Err(Flow::Throw(e)) = &result {
            let e = e.clone();
            let decl = e.decl();
            let program = self.program;
            if let Some(c) = catches.iter().find(|c| {
                decl.is_some_and(|d| c.classes.iter().any(|&k| program.decls.is_subtype(d, k)))
            }) {
                if let Some(l) = c.local {
                    *self.local(l) = e;
                }
                result = self.exec_block(&c.body);
                if let Some(l) = c.local {
                    *self.local(l) = Value::Null;
                }
                if matches!(result, Err(Flow::Abort | Flow::Fatal(_))) {
                    return result;
                }
            }
        }
        let Some(f) = finally else {
            return result;
        };
        match self.exec_block(f) {
            Ok(()) => result,
            Err(new) => {
                match (&result, &new) {
                    (Err(Flow::Throw(old)), _) => {
                        // SPEC "Errors": marked "discarded by throw / exit(n)".
                        // M6: "at file:line" for a replacing `throw` (an `exit`
                        // carries its position already).
                        let by = self.replaced_by(&new);
                        let text = format!(
                            "{}  discarded by {by}\n",
                            self.describe_throwable(old, "Discarded")
                        );
                        let _ = self.iso.flush_stdout();
                        let _ = self.iso.write_stderr(text.as_bytes());
                        if let Flow::Throw(n) = &new {
                            set_previous_if_absent(n, old.clone());
                        }
                    }
                    (Err(Flow::Exit(old, at)), Flow::Exit(..) | Flow::Throw(_)) => {
                        // SPEC: the newest wins; the replaced exit is reported.
                        let by = self.replaced_by(&new);
                        let (file, line) = self.position(*at);
                        let _ = self.iso.flush_stdout();
                        let msg = format!("exit({old}) at {file}:{line} superseded by {by}\n");
                        let _ = self.iso.write_stderr(msg.as_bytes());
                    }
                    _ => {}
                }
                Err(new)
            }
        }
    }

    fn replaced_by(&self, new: &Flow) -> String {
        match new {
            Flow::Exit(c, at) => {
                let (file, line) = self.position(*at);
                format!("exit({c}) at {file}:{line}")
            }
            Flow::Throw(_) => "throw".into(),
            _ => "control flow".into(),
        }
    }

    pub(crate) fn eval_match(&mut self, m: &Match) -> R<Value> {
        let subject = match &m.subject {
            Some(s) => Some(self.eval(s)?),
            None => None,
        };
        for arm in &m.arms {
            let matched = if let Some(c) = &arm.condition {
                self.eval(c)?.as_bool()
            } else if arm.patterns.is_empty() {
                true
            } else {
                let s = subject.clone().unwrap_or_default();
                let mut any = false;
                for p in &arm.patterns {
                    if self.match_pattern(p, &s)? {
                        any = true;
                        break;
                    }
                }
                any
            };
            // A pattern that failed part-way, or a failed guard, may have bound
            // locals: release them so they do not keep values shared.
            if !matched
                || arm
                    .guard
                    .as_ref()
                    .map(|g| self.eval(g).map(|v| !v.as_bool()))
                    .transpose()?
                    .unwrap_or(false)
            {
                for l in &arm.locals {
                    *self.local(*l) = Value::Null;
                }
                continue;
            }
            let r = match &arm.body {
                ArmBody::Expr(e) => self.eval(e),
                ArmBody::Block(b) => self.exec_block(b).map(|()| Value::Null),
            };
            for l in &arm.locals {
                *self.local(*l) = Value::Null;
            }
            return r;
        }
        Err(Flow::Fatal(
            "no `match` arm matched (exhaustiveness is checked at compile time)".into(),
        ))
    }
}

/// Sets a throwable's `previous` (SPEC: the throwable that replaced a pending one).
pub(crate) fn set_previous(e: &Value, previous: Value) {
    if let Value::Object(o) = e
        && let Some(NativeState::Throwable(t)) = &mut o.borrow_mut().native
    {
        t.previous = Some(previous);
    }
}

fn set_previous_if_absent(e: &Value, previous: Value) {
    if let Value::Object(o) = e {
        if let Value::Object(p) = &previous
            && Rc::ptr_eq(o, p)
        {
            return;
        }
        if let Some(NativeState::Throwable(t)) = &mut o.borrow_mut().native
            && t.previous.is_none()
        {
            t.previous = Some(previous);
        }
    }
}
