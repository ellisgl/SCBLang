//! Running one test as an `scb` subprocess and judging the result.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::header::Spec;

/// `scb` exits with this code when compilation fails (`docs/DIAGNOSTICS.md`).
const EXIT_COMPILE_ERROR: i32 = 2;

/// Diagnostic kind for a compiler stage that does not exist yet.
const NOT_IMPLEMENTED: &str = "not-implemented";

pub struct Output {
    /// `None` when the process was killed (signal or timeout).
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

/// Runs `scb check` (compile-error tests) or `scb run` with the test's args, env and stdin.
/// The child sees only the test's `ENV` variables.
pub fn run(scb: &Path, path: &Path, spec: &Spec, timeout: Duration) -> io::Result<Output> {
    let mut cmd = Command::new(scb);
    if spec.compile_error.is_some() {
        cmd.arg("check").arg(path);
    } else {
        cmd.arg("run")
            .arg(path)
            .args(spec.args.iter().map(|a| os(a.clone())));
    }
    cmd.env_clear();
    for (key, value) in &spec.env {
        cmd.env(key, os(value.clone()));
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = spec.stdin.clone();
    let writer = thread::spawn(move || {
        // A program that never reads stdin closes the pipe early; that is not an error.
        let _ = stdin.write_all(&input);
    });
    let stdout = collect(child.stdout.take().expect("piped stdout"));
    let stderr = collect(child.stderr.take().expect("piped stderr"));

    let start = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
        }
        thread::sleep(Duration::from_millis(2));
    };
    let _ = writer.join();
    Ok(Output {
        code: status.and_then(|s| s.code()),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
        timed_out,
    })
}

fn collect(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// Compares an output with the expectations; returns the reasons it fails (empty = pass).
pub fn judge(spec: &Spec, out: &Output, timeout: Duration) -> Vec<String> {
    let mut fails = Vec::new();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.timed_out {
        fails.push(format!("timed out after {}s", timeout.as_secs()));
        return fails;
    }

    if let Some(kind) = &spec.compile_error {
        if out.code != Some(EXIT_COMPILE_ERROR) {
            fails.push(format!(
                "expected a compile error (exit {EXIT_COMPILE_ERROR}), got {}",
                describe(out.code)
            ));
        }
        // `not-implemented` marks a missing compiler stage, never the error a test expects.
        let kinds: Vec<&str> = stderr
            .match_indices("error[")
            .filter_map(|(i, m)| stderr[i + m.len()..].split_once(']').map(|(k, _)| k))
            .filter(|k| *k != NOT_IMPLEMENTED)
            .collect();
        let found = if kind == "*" {
            !kinds.is_empty()
        } else {
            kinds.contains(&kind.as_str())
        };
        if !found {
            fails.push(format!("stderr has no `error[{kind}]`"));
        }
        // A test that also fails for another reason (a typo, a second mistake)
        // proves nothing about the expected error.
        if kind != "*" {
            let mut others: Vec<&str> = kinds.iter().copied().filter(|k| k != kind).collect();
            others.dedup();
            if !others.is_empty() {
                fails.push(format!("unexpected error kind(s): {}", others.join(", ")));
            }
        }
        check_stderr_contains(spec, &stderr, &mut fails);
        return fails;
    }

    if out.code == Some(EXIT_COMPILE_ERROR) && stderr.contains("error[") {
        fails.push("did not compile".to_string());
    }
    let expected = spec.expected_exit();
    if out.code != Some(expected) {
        fails.push(format!(
            "expected exit {expected}, got {}",
            describe(out.code)
        ));
    }
    if let Some(want) = &spec.stdout
        && *want != out.stdout
    {
        fails.push(format!("stdout differs\n{}", diff(want, &out.stdout)));
    }
    check_stderr_contains(spec, &stderr, &mut fails);
    if let Some(class) = &spec.runtime_error
        && !stderr.contains(class.as_str())
    {
        fails.push(format!("stderr does not name `{class}`"));
    }
    fails
}

/// `EXPECT-STDERR-CONTAINS` applies to every kind of test, including compile errors
/// (e.g. to pin a fix-it text).
fn check_stderr_contains(spec: &Spec, stderr: &str, fails: &mut Vec<String>) {
    for needle in &spec.stderr_contains {
        if !stderr.contains(needle.as_str()) {
            fails.push(format!("stderr has no `{needle}`"));
        }
    }
}

fn describe(code: Option<i32>) -> String {
    code.map_or("no exit code (killed by a signal)".to_string(), |c| {
        format!("exit {c}")
    })
}

/// Expected and actual output side by side, one escaped line each, capped.
fn diff(want: &[u8], got: &[u8]) -> String {
    const CAP: usize = 20;
    let show = |label: &str, bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes);
        let mut lines: Vec<String> = text
            .split_inclusive('\n')
            .map(|l| format!("    {label} {}", l.escape_debug()))
            .collect();
        if lines.is_empty() {
            lines.push(format!("    {label} (empty)"));
        }
        if lines.len() > CAP {
            let more = lines.len() - CAP;
            lines.truncate(CAP);
            lines.push(format!("    {label} … {more} more line(s)"));
        }
        lines.join("\n")
    };
    format!("{}\n{}", show("expected", want), show("actual  ", got))
}

#[cfg(unix)]
fn os(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn os(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::parse;

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
            timed_out: false,
        }
    }

    const T: Duration = Duration::from_secs(10);

    #[test]
    fn compile_error_needs_exit_2_and_kind() {
        let spec = parse("// EXPECT-COMPILE-ERROR: type-mismatch\n").unwrap();
        assert!(
            judge(
                &spec,
                &output(2, "", "a.scb:1:1: error[type-mismatch]: x"),
                T
            )
            .is_empty()
        );
        assert_eq!(judge(&spec, &output(2, "", "error[other]: x"), T).len(), 2);
        assert_eq!(judge(&spec, &output(0, "", ""), T).len(), 2);
    }

    #[test]
    fn compile_error_tests_reject_other_kinds() {
        let spec = parse("// EXPECT-COMPILE-ERROR: type-mismatch\n").unwrap();
        let both = "a.scb:1:1: error[type-mismatch]: x\na.scb:2:1: error[undeclared-variable]: y";
        let fails = judge(&spec, &output(2, "", both), T);
        assert_eq!(fails, ["unexpected error kind(s): undeclared-variable"]);
        let twice = "a.scb:1:1: error[type-mismatch]: x\na.scb:2:1: error[type-mismatch]: y";
        assert!(judge(&spec, &output(2, "", twice), T).is_empty());
    }

    #[test]
    fn compile_error_tests_also_check_stderr_contains() {
        let spec =
            parse("// EXPECT-COMPILE-ERROR: short-ternary\n// EXPECT-STDERR-CONTAINS: use `??`\n")
                .unwrap();
        let out = output(2, "", "a.scb:1:1: error[short-ternary]: x\n  fix: use `??`");
        assert!(judge(&spec, &out, T).is_empty());
        let out = output(2, "", "a.scb:1:1: error[short-ternary]: x");
        assert_eq!(judge(&spec, &out, T).len(), 1);
    }

    #[test]
    fn wildcard_kind_accepts_any_compile_error() {
        let spec = parse("// EXPECT-COMPILE-ERROR: *\n").unwrap();
        assert!(judge(&spec, &output(2, "", "error[anything]: x"), T).is_empty());
    }

    #[test]
    fn not_implemented_never_satisfies_a_compile_error_test() {
        let spec = parse("// EXPECT-COMPILE-ERROR: *\n").unwrap();
        let out = output(2, "", "error[not-implemented]: the front end is missing");
        assert_eq!(judge(&spec, &out, T).len(), 1);
    }

    #[test]
    fn stdout_exit_and_runtime_error() {
        let spec =
            parse("// EXPECT-STDOUT:\n// | hi\n// EXPECT-RUNTIME-ERROR: KeyError\n").unwrap();
        assert!(judge(&spec, &output(1, "hi\n", "Uncaught KeyError: missing"), T).is_empty());
        let fails = judge(&spec, &output(0, "ho\n", ""), T);
        assert_eq!(fails.len(), 3, "{fails:?}");
    }

    #[test]
    fn compile_failure_on_a_run_test_is_named() {
        let spec = parse("// EXPECT-STDOUT:\n// | hi\n").unwrap();
        let fails = judge(&spec, &output(2, "", "error[not-implemented]: x"), T);
        assert_eq!(fails[0], "did not compile");
    }
}
