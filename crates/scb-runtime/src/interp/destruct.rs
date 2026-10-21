//! Destructors (SPEC "Destructors").
//!
//! An object of a class with `__destruct` carries a handle to the isolate's
//! [`DestructQueue`]; when its last reference goes, `Drop` moves its contents there
//! (see `value.rs`). [`Interp::run_destructors`] then runs the queue, oldest first:
//! after every statement, when a block releases its locals (also while a throwable
//! or `exit` unwinds), and at program end after the statics are released. A local's
//! destructor therefore runs before the next statement, and the locals of a block
//! are destructed in reverse declaration order (that is the order the block
//! releases them in).
//!
//! A destructor that throws or calls `exit` is fatal: no further unwinding.
//! Objects in reference cycles are destructed by the cycle collector (`cycles.rs`).

use std::cell::RefCell;
use std::rc::Rc;

use scb_types::decls::DeclId;
use scb_types::tir::FuncId;

use super::{Flow, Interp, R};
use crate::isolate::StaticSlot;
use crate::value::{DestructQueue, Value};

impl Interp<'_> {
    /// The queue handle for a new instance of `class`, if the class has a
    /// destructor.
    pub(crate) fn destructor_queue(&mut self, class: DeclId) -> Option<DestructQueue> {
        self.destructor_of(class)
            .map(|_| self.iso.destruct_queue.clone())
    }

    /// `class`'s `__destruct` (declared or inherited).
    fn destructor_of(&mut self, class: DeclId) -> Option<FuncId> {
        if let Some(&f) = self.iso.destructors.get(&class) {
            return f;
        }
        let decls = &self.program.decls;
        let f = decls
            .find_method(class, "__destruct")
            .map(|m| decls.method(m))
            .filter(|m| m.has_body)
            .map(|m| m.func);
        self.iso.destructors.insert(class, f);
        f
    }

    /// Runs every due destructor above the current floor, oldest first, including
    /// those that become due meanwhile.
    pub(crate) fn run_destructors(&mut self) -> R<()> {
        let floor = self.iso.destruct_floor;
        let r = self.drain_from(floor);
        self.iso.destruct_floor = floor;
        r
    }

    fn drain_from(&mut self, floor: usize) -> R<()> {
        loop {
            let obj = {
                let mut q = self.iso.destruct_queue.borrow_mut();
                if q.len() <= floor {
                    return Ok(());
                }
                q.remove(floor).expect("checked the length")
            };
            let decl = obj.decl;
            let this = Rc::new(RefCell::new(obj));
            let handle = Rc::downgrade(&this);
            self.call_destructor(decl, Value::Object(this))?;
            // Resurrected: the object is back in the collector's care.
            if handle.strong_count() > 0 {
                self.register(handle);
            }
        }
    }

    /// Runs `decl`'s destructor on `this` (an object whose last outside reference
    /// is gone). Objects it releases are destructed inside it; its fields' objects
    /// follow it directly. A throw or `exit` escaping it is fatal.
    pub(crate) fn call_destructor(&mut self, decl: DeclId, this: Value) -> R<()> {
        let Some(func) = self.destructor_of(decl) else {
            return Ok(());
        };
        let floor = self.iso.destruct_floor;
        let span = self.program.funcs[func.0 as usize].span;
        // Objects this destructor releases queue above everything pending now.
        let inner = self.iso.destruct_queue.borrow().len();
        self.iso.destruct_floor = inner;
        let (r, end) = self.call_func(func, Some(this), Vec::new(), &[], span);
        // `$this` goes now: its fields are released, which may queue more. Those
        // are part of this object's destruction, so they go to the front, ahead
        // of what was pending before (a loop, not recursion: a long chain of
        // objects cannot exhaust the stack).
        drop(end);
        {
            let mut q = self.iso.destruct_queue.borrow_mut();
            let from = inner.min(q.len());
            let released: Vec<_> = q.drain(from..).collect();
            for (k, o) in released.into_iter().enumerate() {
                q.insert(floor + k, o);
            }
        }
        self.iso.destruct_floor = floor;
        match r {
            Ok(_) | Err(Flow::Return(_) | Flow::Break(_) | Flow::Continue(_)) => Ok(()),
            Err(Flow::Throw(e, _)) => {
                let name = &self.program.funcs[func.0 as usize].name;
                let trace = self.describe_throwable(&e, "Uncaught");
                Err(Flow::Fatal(format!(
                    "a throwable escaped `{name}`\n{}",
                    trace.trim_end()
                )))
            }
            Err(Flow::Exit(code, at)) => {
                let name = &self.program.funcs[func.0 as usize].name;
                let (file, line) = self.position(at);
                Err(Flow::Fatal(format!(
                    "exit({code}) called in `{name}` at {file}:{line}"
                )))
            }
            Err(e @ (Flow::Fatal(_) | Flow::Abort)) => Err(e),
        }
    }

    /// Program end: statics are released in reverse initialization order, each
    /// one's destructors running before the next is released.
    pub(crate) fn release_statics(&mut self) -> R<()> {
        while let Some(sid) = self.iso.static_order.pop() {
            let slot =
                std::mem::replace(&mut self.iso.statics[sid.0 as usize], StaticSlot::Destroyed);
            drop(slot);
            self.run_destructors()?;
        }
        Ok(())
    }
}
