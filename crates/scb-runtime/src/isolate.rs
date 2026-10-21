//! The [`Isolate`]: one execution of a program, with all of its state.
//!
//! Embedders (the CLI now, `scb-fpm` later) build an [`IsolateIo`] from whatever I/O
//! they have and hand it over; interpreter and stdlib code reach the outside world
//! only through the isolate's methods. This is what lets a server run many isolates
//! side by side with different stdin/stdout/env and no shared state.

use std::collections::HashMap;
use std::io::{self, BufRead, BufWriter, Read, Write};
use std::sync::Arc;

use scb_syntax::Span;
use scb_types::Program;
use scb_types::decls::DeclId;
use scb_types::tir::FuncId;

use crate::interp::{Interp, Natives};
use crate::value::Value;

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
    pub fn run(&mut self, program: &Arc<Program>, args: &[Vec<u8>]) -> RunOutcome {
        let code = Interp::new(self, program).run_main(args);
        let _ = self.flush_stdout();
        RunOutcome::Exit(code)
    }

    /// Buffered write to stdout.
    pub fn write_stdout(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stdout.write_all(bytes)
    }

    /// Unbuffered write to stderr.
    pub fn write_stderr(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.write_all(bytes)?;
        self.stderr.flush()
    }

    pub fn flush_stdout(&mut self) -> io::Result<()> {
        self.stdout.flush()
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
