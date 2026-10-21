//! `scb run <file.scb|dir> [args…]` and `scb check <file.scb|dir>`.
//!
//! The only crate that touches real OS handles: it wires stdout, stderr, stdin and
//! the process environment into an [`Isolate`]. Exit codes are listed in
//! `docs/DIAGNOSTICS.md`.

use std::ffi::OsString;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use scb_runtime::{Env, Isolate, IsolateIo, RunOutcome};
use scb_syntax::{Diagnostic, SourceMap};

const EXIT_COMPILE_ERROR: u8 = 2;
const EXIT_USAGE: u8 = 64;
const EXIT_NO_INPUT: u8 = 66;

const USAGE: &str = "usage: scb run <file.scb|dir> [args…]\n       scb check <file.scb|dir>";

fn main() -> ExitCode {
    let mut argv = std::env::args_os().skip(1);
    let command = argv.next();
    let path = argv.next().map(PathBuf::from);
    let (command, path) = match (command.as_ref().and_then(|c| c.to_str()), path) {
        (Some(c @ ("run" | "check")), Some(p)) => (c, p),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    if command == "check" && argv.next().is_some() {
        eprintln!("{USAGE}");
        return ExitCode::from(EXIT_USAGE);
    }

    let sources = match load(&path) {
        Ok(sources) => sources,
        Err(LoadError::Compile(diagnostic)) => {
            report(&SourceMap::new(), &[diagnostic]);
            return ExitCode::from(EXIT_COMPILE_ERROR);
        }
        Err(LoadError::Usage(message)) => {
            eprintln!("scb: {message}\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
        Err(LoadError::NoInput(message)) => {
            eprintln!("scb: {message}");
            return ExitCode::from(EXIT_NO_INPUT);
        }
    };

    let program = match scb_types::check(sources) {
        Ok(program) => Arc::new(program),
        Err((sources, diagnostics)) => {
            report(&sources, &diagnostics);
            return ExitCode::from(EXIT_COMPILE_ERROR);
        }
    };
    if command == "check" {
        return ExitCode::SUCCESS;
    }

    let args: Vec<Vec<u8>> = argv.map(os_bytes).collect();
    let io = IsolateIo {
        stdout: Box::new(io::stdout().lock()),
        stderr: Box::new(io::stderr()),
        stdin: Box::new(BufReader::new(io::stdin())),
        env: Box::new(OsEnv),
    };
    let RunOutcome::Exit(code) = Isolate::new(io).run(&program, &args);
    ExitCode::from(code.clamp(0, 255) as u8)
}

/// Loads a compilation: one file (single-file mode) or every `.scb` file directly
/// in a directory (directory mode; `main.scb` required). SPEC "Source files and
/// program entry".
fn load(path: &Path) -> Result<SourceMap, LoadError> {
    let unreadable =
        |p: &Path, e: io::Error| LoadError::NoInput(format!("cannot read {}: {e}", p.display()));
    let meta = std::fs::metadata(path).map_err(|e| unreadable(path, e))?;
    let files = if meta.is_dir() {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(path).map_err(|e| unreadable(path, e))? {
            let entry = entry.map_err(|e| unreadable(path, e))?;
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|ext| ext == "scb") {
                files.push(p);
            }
        }
        files.sort();
        if !files
            .iter()
            .any(|f| f.file_name().is_some_and(|n| n == "main.scb"))
        {
            return Err(LoadError::Compile(
                Diagnostic::new(
                    "missing-main",
                    "directory mode requires a main.scb defining main",
                )
                .with_note(format!("in {}", path.display())),
            ));
        }
        files
    } else if path.extension().is_some_and(|ext| ext == "scb") {
        vec![path.to_path_buf()]
    } else {
        return Err(LoadError::Usage(format!(
            "{} is not a .scb file or a directory",
            path.display()
        )));
    };

    let mut sources = SourceMap::new();
    for file in files {
        let bytes = std::fs::read(&file).map_err(|e| unreadable(&file, e))?;
        sources.add(&file, bytes).map_err(LoadError::Compile)?;
    }
    Ok(sources)
}

enum LoadError {
    /// Bad command line (exit 64).
    Usage(String),
    /// Input missing or unreadable (exit 66).
    NoInput(String),
    /// A compile error found while loading, e.g. invalid UTF-8 (exit 2).
    Compile(Diagnostic),
}

fn report(sources: &SourceMap, diagnostics: &[Diagnostic]) {
    let mut stderr = io::stderr().lock();
    for d in diagnostics {
        let _ = writeln!(stderr, "{}", d.render(sources));
    }
}

/// The real process environment, read as raw bytes.
struct OsEnv;

impl Env for OsEnv {
    fn get(&self, name: &str) -> Option<Vec<u8>> {
        std::env::var_os(name).map(os_bytes)
    }
}

#[cfg(unix)]
fn os_bytes(s: OsString) -> Vec<u8> {
    use std::os::unix::ffi::OsStringExt;
    s.into_vec()
}

#[cfg(not(unix))]
fn os_bytes(s: OsString) -> Vec<u8> {
    // Non-UTF-16-clean values become U+FFFD here; the POC targets POSIX.
    s.to_string_lossy().into_owned().into_bytes()
}
