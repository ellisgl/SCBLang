//! Conformance runner: `cargo run -p scb-conformance [-- [--scb PATH] [FILTER]]`.
//!
//! Builds `scb` (unless `--scb` is given), runs every test under `tests/conformance`
//! whose name contains FILTER, prints PASS/FAIL/SKIP per test with reasons, then
//! totals. Exits 1 if any test fails. Format: `tests/conformance/README.md`.

mod discover;
mod exec;
mod header;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const USAGE: &str = "usage: scb-conformance [--scb PATH] [FILTER]";

fn main() -> ExitCode {
    let mut scb: Option<PathBuf> = None;
    let mut filter = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scb" => match args.next() {
                Some(p) => scb = Some(PathBuf::from(p)),
                None => return usage(),
            },
            "-h" | "--help" => return usage(),
            _ if filter.is_empty() && !arg.starts_with('-') => filter = arg,
            _ => return usage(),
        }
    }

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let scb = match scb {
        Some(p) => p,
        None => match build_scb(&workspace) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("scb-conformance: {e}");
                return ExitCode::from(2);
            }
        },
    };

    let root = workspace.join("tests/conformance");
    let cases = match discover::discover(&root) {
        Ok(cases) => cases,
        Err(e) => {
            eprintln!("scb-conformance: cannot read {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };

    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    for case in cases.iter().filter(|c| c.name.contains(&filter)) {
        let spec = match &case.spec {
            Ok(spec) => spec,
            Err(e) => {
                failed += 1;
                println!("FAIL {}\n  bad test header: {e}", case.name);
                continue;
            }
        };
        if let Some(reason) = &spec.skip {
            skipped += 1;
            println!("SKIP {} ({reason})", case.name);
            continue;
        }
        let fails = match exec::run(&scb, &case.path, spec, TIMEOUT) {
            Ok(out) => {
                let mut fails = exec::judge(spec, &out, TIMEOUT);
                if !fails.is_empty() && !out.stderr.is_empty() {
                    fails.push(format!(
                        "stderr:\n{}",
                        indent(&String::from_utf8_lossy(&out.stderr))
                    ));
                }
                fails
            }
            Err(e) => vec![format!("cannot run {}: {e}", scb.display())],
        };
        if fails.is_empty() {
            passed += 1;
            println!("PASS {}", case.name);
        } else {
            failed += 1;
            println!("FAIL {}", case.name);
            for f in fails {
                println!("  {f}");
            }
        }
    }

    let total = passed + failed + skipped;
    println!("\n{total} tests: {passed} passed, {failed} failed, {skipped} skipped");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Builds the `scb` binary with the runner's own profile so results never come
/// from a stale binary, and returns its path (next to the runner's executable).
fn build_scb(workspace: &Path) -> Result<PathBuf, String> {
    let cargo = option_env!("CARGO").unwrap_or("cargo");
    let mut cmd = Command::new(cargo);
    cmd.current_dir(workspace)
        .args(["build", "--quiet", "-p", "scb-cli"]);
    if !cfg!(debug_assertions) {
        cmd.arg("--release");
    }
    let status = cmd
        .status()
        .map_err(|e| format!("cannot run {cargo}: {e}"))?;
    if !status.success() {
        return Err("building scb failed".to_string());
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate runner: {e}"))?;
    let scb = exe.with_file_name(format!("scb{}", std::env::consts::EXE_SUFFIX));
    if scb.is_file() {
        Ok(scb)
    } else {
        Err(format!("built scb not found at {}", scb.display()))
    }
}

fn indent(text: &str) -> String {
    const CAP: usize = 20;
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = lines.iter().take(CAP).map(|l| format!("    {l}")).collect();
    if lines.len() > CAP {
        out.push(format!("    … {} more line(s)", lines.len() - CAP));
    }
    out.join("\n")
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
