//! End-to-end runs (check, then interpret with the stdlib) for properties the
//! conformance suite cannot observe, such as copy-on-write cost.

use std::cell::RefCell;
use std::io::{self, Cursor, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use scb_runtime::{Isolate, IsolateIo, MapEnv, RunOutcome};
use scb_syntax::SourceMap;

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

/// Checks and runs a program; returns (exit code, stdout, stderr).
fn run(src: &str) -> (i32, String, String) {
    let mut sources = SourceMap::new();
    sources
        .add(Path::new("main.scb"), src.as_bytes().to_vec())
        .unwrap();
    let program = match scb_types::check(sources) {
        Ok(p) => Arc::new(p),
        Err((sources, diags)) => panic!(
            "{}",
            diags
                .iter()
                .map(|d| d.render(&sources))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    };
    let (out, err) = (Shared::default(), Shared::default());
    let io = IsolateIo {
        stdout: Box::new(out.clone()),
        stderr: Box::new(err.clone()),
        stdin: Box::new(Cursor::new(Vec::new())),
        env: Box::new(MapEnv::default()),
    };
    let RunOutcome::Exit(code) =
        Isolate::with_natives(io, scb_stdlib::natives()).run(&program, &[]);
    let text = |s: &Shared| String::from_utf8_lossy(&s.0.borrow()).into_owned();
    (code, text(&out), text(&err))
}

/// SPEC "Collections and values": copy-on-write mutates in place when the value is
/// not shared. Appending to an unshared list (directly, through `inout`, through a
/// struct field, through a `mutating` method) must therefore be O(1) per element; a
/// copy per append would make this loop quadratic (minutes instead of a second).
#[test]
fn appending_to_unshared_lists_does_not_copy() {
    let src = r#"namespace T;
struct Bag
{
    public function __construct(public $items: list<int>) {}
    public mutating function add($v: int): void { $this->items->push($v); }
}
function add(inout $items: list<int>, $v: int): void { $items->push($v); }
function main(): void
{
    $direct: list<int> = [];
    $viaInout: list<int> = [];
    $field: Bag = new Bag([]);
    $method: Bag = new Bag([]);
    $text: string = '';
    for ($i: int = 0; $i < 50000; $i++) {
        $direct->push($i);
        add(inout $viaInout, $i);
        $field->items[] = $i;
        $method->add($i);
        $text .= 'x';
    }
    echo $direct->count() + $viaInout->count() + $field->items->count() + $method->items->count(), ' ', $text->byteLength(), "\n";
}
"#;
    let start = Instant::now();
    let (code, out, err) = run(src);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (0, "200000 50000\n", "")
    );
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "appending took {:?}: values are being copied",
        start.elapsed()
    );
}

/// A shared list is copied on write, and only the writer sees the change.
#[test]
fn shared_values_are_copied_on_write() {
    let src = r#"namespace T;
function main(): void
{
    $a: list<list<int>> = [[1], [2]];
    $b: list<list<int>> = $a;
    $b[0][] = 9;
    echo $a[0]->count(), ' ', $b[0]->count(), ' ', $a[1] == $b[1] ? 'same' : 'different', "\n";
}
"#;
    assert_eq!(run(src), (0, "1 2 same\n".into(), String::new()));
}

#[test]
fn runaway_recursion_is_a_fatal_error_not_a_crash() {
    let src = "namespace T;\nfunction f($n: int): int { return f($n + 1); }\nfunction main(): void { echo f(0); }\n";
    let handle = std::thread::Builder::new()
        .stack_size(1024 * 1024 * 1024)
        .spawn(move || run(src))
        .unwrap();
    let (code, _, err) = handle.join().unwrap();
    assert_eq!(code, 1);
    assert!(err.contains("stack overflow"), "{err}");
}

/// An isolate run twice starts each run fresh: statics are not carried over.
#[test]
fn reused_isolate_starts_fresh() {
    let src = r#"namespace T;
class Counter { public static $n: int = 0; }
function main(): void { Counter::$n++; echo Counter::$n, "\n"; }
"#;
    let mut sources = SourceMap::new();
    sources
        .add(Path::new("main.scb"), src.as_bytes().to_vec())
        .unwrap();
    let program = Arc::new(scb_types::check(sources).unwrap_or_else(|_| panic!("compiles")));
    let out = Shared::default();
    let io = IsolateIo {
        stdout: Box::new(out.clone()),
        stderr: Box::new(Shared::default()),
        stdin: Box::new(Cursor::new(Vec::new())),
        env: Box::new(MapEnv::default()),
    };
    let mut iso = Isolate::with_natives(io, scb_stdlib::natives());
    for _ in 0..2 {
        assert_eq!(iso.run(&program, &[]), RunOutcome::Exit(0));
    }
    assert_eq!(String::from_utf8_lossy(&out.0.borrow()), "1\n1\n");
}

/// A stdout whose every write fails, like a closed pipe.
struct ClosedPipe;

impl Write for ClosedPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed pipe"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Runs a program with a stdout that fails; returns (exit code, stderr).
fn run_with_closed_stdout(src: &str) -> (i32, String) {
    let mut sources = SourceMap::new();
    sources
        .add(Path::new("main.scb"), src.as_bytes().to_vec())
        .unwrap();
    let program = Arc::new(scb_types::check(sources).unwrap_or_else(|_| panic!("compiles")));
    let err = Shared::default();
    let io = IsolateIo {
        stdout: Box::new(ClosedPipe),
        stderr: Box::new(err.clone()),
        stdin: Box::new(Cursor::new(Vec::new())),
        env: Box::new(MapEnv::default()),
    };
    let RunOutcome::Exit(code) =
        Isolate::with_natives(io, scb_stdlib::natives()).run(&program, &[]);
    let text = String::from_utf8_lossy(&err.0.borrow()).into_owned();
    (code, text)
}

/// SPEC "Source files and program entry": a failed final flush is reported on
/// stderr and the exit code is 1 (a buffered `echo` itself succeeded).
#[test]
fn failed_final_flush_exits_1() {
    let (code, err) =
        run_with_closed_stdout("namespace T;\nfunction main(): void { echo \"hi\\n\"; }\n");
    assert_eq!(code, 1);
    assert!(err.contains("cannot write to stdout"), "{err}");
}

/// SPEC: a failed write throws `IoException` from `echo`, catchable like any other.
#[test]
fn failed_echo_throws_io_exception() {
    let src = r#"namespace T;
function main(): void
{
    try {
        echo 'x'->repeat(100000);
    } catch (\IoException) {
        \Io\eprint("caught\n");
    }
}
"#;
    assert_eq!(run_with_closed_stdout(src), (0, "caught\n".into()));
}

/// A failed write is reported once: the uncaught `IoException`, not again by the
/// flush at program end (the undeliverable buffer is dropped with the failure).
#[test]
fn failed_write_is_reported_once() {
    let src = r#"namespace T;
function main(): void
{
    for ($i: int = 0; $i < 10000; $i++) {
        echo "line\n";
    }
}
"#;
    let (code, err) = run_with_closed_stdout(src);
    assert_eq!(code, 1);
    assert!(err.starts_with("Uncaught IoException"), "{err}");
    assert!(!err.contains("cannot write to stdout"), "{err}");
}

/// A long chain of destructible objects is destructed iteratively (no Rust stack
/// growth per link), in order, each destructor before its fields.
#[test]
fn long_destructor_chain() {
    let src = r#"namespace T;
class Node
{
    public static $count: int = 0;
    public function __construct(public $next: ?Node) {}
    public function __destruct() { Node::$count++; }
}
function build(): void
{
    $head: ?Node = null;
    for ($i: int = 0; $i < 100000; $i++) {
        $head = new Node($head);
    }
}
function main(): void
{
    build();
    echo Node::$count, "\n";
}
"#;
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || run(src))
        .unwrap();
    assert_eq!(
        handle.join().unwrap(),
        (0, "100000\n".into(), String::new())
    );
}
