use std::collections::HashMap;
use std::io::{self, BufRead, BufWriter, Read, Write};
use std::sync::Arc;

use scb_types::Program;

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

/// All interpreter state for one execution of a [`Program`]. No interpreter
/// state lives anywhere else (CLAUDE.md architecture rule 1).
pub struct Isolate {
    stdout: BufWriter<Box<dyn Write>>,
    stderr: Box<dyn Write>,
    stdin: Box<dyn BufRead>,
    env: Box<dyn Env>,
}

impl Isolate {
    pub fn new(io: IsolateIo) -> Self {
        Isolate {
            stdout: BufWriter::with_capacity(STDOUT_BUFFER, io.stdout),
            stderr: io.stderr,
            stdin: io.stdin,
            env: io.env,
        }
    }

    /// Runs `main` with the user arguments (raw bytes; UTF-8 is checked before `main`).
    ///
    /// M0: the interpreter does not exist yet (M4).
    pub fn run(&mut self, _program: &Arc<Program>, _args: &[Vec<u8>]) -> RunOutcome {
        let _ = self.write_stderr(b"scb: the interpreter is not implemented yet\n");
        let _ = self.flush_stdout();
        RunOutcome::Exit(70)
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
