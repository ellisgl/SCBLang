//! The [`Isolate`]: one execution of a program, with all of its state.
//!
//! Embedders (the CLI now, `scb-fpm` later) build an [`IsolateIo`] from whatever I/O
//! they have and hand it over; interpreter and stdlib code reach the outside world
//! only through the isolate's methods. This is what lets a server run many isolates
//! side by side with different stdin/stdout/env and no shared state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, BufRead, BufWriter, Read, Write};
use std::rc::Weak;
use std::sync::Arc;

use scb_syntax::Span;
use scb_types::Program;
use scb_types::decls::{DeclId, StaticId};
use scb_types::tir::FuncId;

use crate::interp::{Exclusive, Interp, Natives};
use crate::value::{DestructQueue, ObjectValue, Value};

/// Stdout buffer size (SPEC: "size implementation-defined").
const STDOUT_BUFFER: usize = 8 * 1024;

/// Environment lookup injected into an isolate. Values are raw bytes: SPEC
/// requires `Env\get()` to reject invalid UTF-8, so the runtime must see it.
pub trait Env {
    fn get(&self, name: &str) -> Option<Vec<u8>>;
}

/// An environment backed by a map (tests, and later one FastCGI request's params).
#[derive(Debug, Default, Clone)]
pub struct MapEnv(pub HashMap<String, Vec<u8>>);

impl Env for MapEnv {
    fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.0.get(name).cloned()
    }
}

/// The outside world as seen by one isolate. Only the embedder (CLI, later
/// `scb-fpm`) builds this from real OS handles.
pub struct IsolateIo {
    pub stdout: Box<dyn Write>,
    pub stderr: Box<dyn Write>,
    pub stdin: Box<dyn BufRead>,
    pub env: Box<dyn Env>,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Process exit code (0–255 on POSIX).
    Exit(i32),
}

/// One function activation: its local slots.
#[derive(Debug, Default)]
pub(crate) struct Frame {
    pub locals: Vec<Value>,
}

/// A static property's lifecycle (SPEC "Static initialization").
#[derive(Debug)]
pub(crate) enum StaticSlot {
    Uninit,
    /// Its initializer is running: an access now is a cycle.
    Running,
    Ready(Value),
    /// Its initializer threw this; every later access throws `StaticInitError`.
    Poisoned(Value),
    /// Released at program end (statics are destructed in reverse initialization
    /// order); a destructor that runs later and reads it gets `StaticInitError`.
    Destroyed,
}

/// All interpreter state for one execution of a [`Program`]. No interpreter
/// state lives anywhere else (CLAUDE.md architecture rule 1).
pub struct Isolate {
    stdout: BufWriter<Box<dyn Write>>,
    stderr: Box<dyn Write>,
    stdin: Box<dyn BufRead>,
    env: Box<dyn Env>,
    /// The core library's native functions (provided by the embedder).
    pub(crate) natives: Natives,
    pub(crate) frames: Vec<Frame>,
    /// The SCB call stack: callee and call site, outermost first.
    pub(crate) calls: Vec<(FuncId, Span)>,
    pub(crate) statics: Vec<StaticSlot>,
    pub(crate) consts: Vec<Option<Value>>,
    /// Resolved dynamic dispatch: (runtime type, statically resolved method) →
    /// implementation.
    pub(crate) vtables: HashMap<(DeclId, FuncId), FuncId>,
    /// The result type of the builtin call about to run (see `NativeCall::ret`).
    pub(crate) ret_hint: Option<scb_types::ty::Ty>,
    /// Objects whose destructor is due (see [`DestructQueue`]).
    pub(crate) destruct_queue: DestructQueue,
    /// Queue entries below this index belong to an enclosing destructor run:
    /// while a destructor runs, only objects it released itself are destructed
    /// inside it (so one destructor never runs in the middle of another).
    pub(crate) destruct_floor: usize,
    /// Each class's `__destruct`, if it has one (resolved on first use).
    pub(crate) destructors: HashMap<DeclId, Option<FuncId>>,
    /// Statics in the order their initializers finished (destructed in reverse).
    pub(crate) static_order: Vec<StaticId>,
    /// Modifying accesses in progress on class properties and statics (SPEC
    /// "Exclusivity"), with the id that ends each.
    pub(crate) exclusive: Vec<(u64, Exclusive)>,
    pub(crate) next_access: u64,
    /// Every class instance created and not yet found dead (the cycle collector's
    /// starting points), and the length at which dead entries are pruned.
    pub(crate) objects: Vec<Weak<RefCell<ObjectValue>>>,
    pub(crate) gc_prune_at: usize,
    /// Objects created since the last collection, and the count that triggers
    /// the next one.
    pub(crate) gc_created: usize,
    pub(crate) gc_threshold: usize,
    /// A collection is running (its destructors do not start another).
    pub(crate) gc_running: bool,
    /// `Process\name()`.
    program_name: String,
}

impl Isolate {
    /// An isolate without core-library natives (enough for programs that do not call
    /// the stdlib; embedders use [`Isolate::with_natives`]).
    pub fn new(io: IsolateIo) -> Self {
        Self::with_natives(io, Natives::new())
    }

    pub fn with_natives(io: IsolateIo, natives: Natives) -> Self {
        Isolate {
            stdout: BufWriter::with_capacity(STDOUT_BUFFER, io.stdout),
            stderr: io.stderr,
            stdin: io.stdin,
            env: io.env,
            natives,
            frames: Vec::new(),
            calls: Vec::new(),
            statics: Vec::new(),
            consts: Vec::new(),
            vtables: HashMap::new(),
            ret_hint: None,
            destruct_queue: DestructQueue::default(),
            destruct_floor: 0,
            destructors: HashMap::new(),
            static_order: Vec::new(),
            exclusive: Vec::new(),
            next_access: 0,
            objects: Vec::new(),
            gc_prune_at: 0,
            gc_created: 0,
            gc_threshold: usize::MAX,
            gc_running: false,
            program_name: "scb".into(),
        }
    }

    /// The name `Process\name()` reports (the CLI passes the program's path).
    pub fn set_program_name(&mut self, name: impl Into<String>) {
        self.program_name = name.into();
    }

    pub fn program_name(&self) -> &str {
        &self.program_name
    }

    /// Drops buffered stdout without writing it (`Process\abort()`).
    pub(crate) fn discard_stdout(&mut self) {
        let fresh = BufWriter::with_capacity(STDOUT_BUFFER, Box::new(io::sink()) as Box<dyn Write>);
        let old = std::mem::replace(&mut self.stdout, fresh);
        // `into_parts` returns the inner writer without flushing the buffer.
        let (inner, _unflushed) = old.into_parts();
        self.stdout = BufWriter::with_capacity(STDOUT_BUFFER, inner);
    }

    /// Runs `main` with the user arguments (raw bytes; UTF-8 is checked before
    /// `main`) and returns the process exit code. Stdout is flushed at the end.
    ///
    /// Run it on a thread with a large stack (the CLI uses 1 GiB): the interpreter
    /// recurses on the Rust stack per SCB call (up to 10 000 deep) and per level
    /// when deeply nested values are freed, and running out of native stack aborts
    /// the process instead of raising SCB's "stack overflow" fatal error.
    pub fn run(&mut self, program: &Arc<Program>, args: &[Vec<u8>]) -> RunOutcome {
        let code = Interp::new(self, program).run_main(args);
        let _ = self.flush_stdout();
        RunOutcome::Exit(code)
    }

    /// Buffered write to stdout. After a failure the buffer is dropped: its bytes
    /// cannot be delivered, and keeping them would make every later flush (the one
    /// at program end included) fail and report the same error again.
    pub fn write_stdout(&mut self, bytes: &[u8]) -> io::Result<()> {
        let r = self.stdout.write_all(bytes);
        if r.is_err() {
            self.discard_stdout();
        }
        r
    }

    /// Unbuffered write to stderr.
    pub fn write_stderr(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.write_all(bytes)?;
        self.stderr.flush()
    }

    /// Flushes stdout; after a failure the buffer is dropped, as in
    /// [`Self::write_stdout`].
    pub fn flush_stdout(&mut self) -> io::Result<()> {
        let r = self.stdout.flush();
        if r.is_err() {
            self.discard_stdout();
        }
        r
    }

    /// Reads one line including its terminator; `None` at end of input.
    /// Stdout is flushed first so a prompt is visible before the program blocks.
    pub fn read_line_raw(&mut self) -> io::Result<Option<Vec<u8>>> {
        self.flush_stdout()?;
        let mut line = Vec::new();
        let n = self.stdin.read_until(b'\n', &mut line)?;
        Ok(if n == 0 { None } else { Some(line) })
    }

    /// Reads the rest of stdin. Stdout is flushed first, as for [`Self::read_line_raw`].
    pub fn read_all(&mut self) -> io::Result<Vec<u8>> {
        self.flush_stdout()?;
        let mut all = Vec::new();
        self.stdin.read_to_end(&mut all)?;
        Ok(all)
    }

    pub fn env_var(&self, name: &str) -> Option<Vec<u8>> {
        self.env.get(name)
    }

    pub(crate) fn has_pending_destructors(&self) -> bool {
        self.destruct_queue.borrow().len() > self.destruct_floor
    }
}

impl Isolate {
    /// Frees what an earlier run left without running destructors (after
    /// `Process\abort()` or a fatal error, objects still queued, cycles). Queued
    /// objects hold handles to the queue itself, so it is emptied explicitly;
    /// dropping them can queue more, hence the loop.
    pub(crate) fn free_leftovers(&mut self) {
        self.statics.clear();
        self.consts.clear();
        self.frames.clear();
        self.exclusive.clear();
        // Objects still alive are in cycles: clear them so they are freed.
        let live: Vec<_> = self.objects.drain(..).filter_map(|w| w.upgrade()).collect();
        let mut released = Vec::new();
        for o in &live {
            if let Ok(mut o) = o.try_borrow_mut() {
                o.destructor = None;
                released.push((std::mem::take(&mut o.fields), o.native.take()));
            }
        }
        drop(released);
        drop(live);
        loop {
            let pending = std::mem::take(&mut *self.destruct_queue.borrow_mut());
            if pending.is_empty() {
                break;
            }
            drop(pending);
        }
    }
}

impl Drop for Isolate {
    fn drop(&mut self) {
        self.free_leftovers();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::rc::Rc;

    use super::*;

    /// A writer whose contents the test can inspect after handing it to the isolate.
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Shared {
        fn contents(&self) -> Vec<u8> {
            self.0.borrow().clone()
        }
    }

    fn isolate(stdin: &[u8], env: &[(&str, &[u8])]) -> (Isolate, Shared, Shared) {
        let out = Shared::default();
        let err = Shared::default();
        let env = MapEnv(
            env.iter()
                .map(|(k, v)| (k.to_string(), v.to_vec()))
                .collect(),
        );
        let io = IsolateIo {
            stdout: Box::new(out.clone()),
            stderr: Box::new(err.clone()),
            stdin: Box::new(Cursor::new(stdin.to_vec())),
            env: Box::new(env),
        };
        (Isolate::new(io), out, err)
    }

    #[test]
    fn stdout_is_buffered_until_flush() {
        let (mut iso, out, _) = isolate(b"", &[]);
        iso.write_stdout(b"hi").unwrap();
        assert_eq!(out.contents(), b"");
        iso.flush_stdout().unwrap();
        assert_eq!(out.contents(), b"hi");
    }

    #[test]
    fn stderr_is_unbuffered() {
        let (mut iso, _, err) = isolate(b"", &[]);
        iso.write_stderr(b"oops").unwrap();
        assert_eq!(err.contents(), b"oops");
    }

    #[test]
    fn reading_stdin_flushes_stdout_first() {
        let (mut iso, out, _) = isolate(b"one\r\ntwo", &[]);
        iso.write_stdout(b"prompt> ").unwrap();
        assert_eq!(
            iso.read_line_raw().unwrap().as_deref(),
            Some(&b"one\r\n"[..])
        );
        assert_eq!(out.contents(), b"prompt> ");
        assert_eq!(iso.read_line_raw().unwrap().as_deref(), Some(&b"two"[..]));
        assert_eq!(iso.read_line_raw().unwrap(), None);
    }

    #[test]
    fn read_all_returns_remaining_bytes() {
        let (mut iso, _, _) = isolate(b"a\n\xFFb", &[]);
        iso.read_line_raw().unwrap();
        assert_eq!(iso.read_all().unwrap(), b"\xFFb");
    }

    #[test]
    fn env_comes_only_from_the_injected_map() {
        let (iso, _, _) = isolate(b"", &[("GREETING", b"hi")]);
        assert_eq!(iso.env_var("GREETING"), Some(b"hi".to_vec()));
        assert_eq!(iso.env_var("PATH"), None);
    }
}
