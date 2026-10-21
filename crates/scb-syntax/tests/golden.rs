//! Golden AST tests.
//!
//! - `tests/golden/*.scb` parse to the S-expression in the matching `.ast` file.
//! - Every ```` ```php ```` block in `docs/SPEC.md` parses (as declarations, statements,
//!   or one expression per line) to `tests/golden/spec_examples.ast`.
//!
//! Run with `SCB_BLESS=1` to rewrite the expected files after reviewing the change.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use scb_syntax::{Diagnostic, SourceMap, dump_file, parse_source};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn bless() -> bool {
    std::env::var_os("SCB_BLESS").is_some()
}

/// Compares `actual` with the file at `path`, or rewrites it when blessing.
fn check_golden(path: &Path, actual: &str) -> Result<(), String> {
    if bless() {
        std::fs::write(path, actual).unwrap();
        return Ok(());
    }
    let expected = std::fs::read_to_string(path).unwrap_or_default();
    if expected == actual {
        return Ok(());
    }
    let line = expected
        .lines()
        .zip(actual.lines())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
    Err(format!(
        "{} differs from the parser output at line {} (rerun with SCB_BLESS=1 to update)\n  expected: {:?}\n  actual:   {:?}",
        path.display(),
        line + 1,
        expected.lines().nth(line).unwrap_or("<end>"),
        actual.lines().nth(line).unwrap_or("<end>"),
    ))
}

/// Parses `text` and renders the AST followed by any diagnostics as `;` comment lines.
fn render(sources: &mut SourceMap, name: &str, text: &str) -> (String, Vec<Diagnostic>) {
    let id = sources
        .add(Path::new(name), text.as_bytes().to_vec())
        .unwrap();
    let (ast, diags) = parse_source(id, &sources.file(id).text);
    let mut out = ast.map(|a| dump_file(&a)).unwrap_or_default();
    for d in &diags {
        let (line, col) = sources.file(id).line_col(d.span.map_or(0, |s| s.start));
        let _ = writeln!(out, "; {line}:{col} error[{}]: {}", d.kind, d.message);
    }
    (out, diags)
}

#[test]
fn golden_files() {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "scb"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty());
    let mut sources = SourceMap::new();
    let mut problems = Vec::new();
    for path in entries {
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (out, _) = render(&mut sources, &name, &text);
        if let Err(e) = check_golden(&path.with_extension("ast"), &out) {
            problems.push(e);
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

/// SPEC blocks that are not SCB source (signature lists, illustrative fragments).
const NOT_SOURCE: &[(&str, &str)] = &[
    (
        "function main(): void\nfunction main(): int",
        "list of allowed signatures",
    ),
    (
        "public function join($sep: string): string where",
        "signature fragment",
    ),
];

/// Parses one SPEC block in the first mode that yields no structural error.
fn parse_example(sources: &mut SourceMap, index: usize, code: &str) -> Result<String, String> {
    // `…` stands for elided code in the SPEC.
    let code = code.replace("{ … }", "{ }").replace('…', "");
    // Declarations followed by statements that use them: split after the last `}` line.
    let lines: Vec<&str> = code.lines().collect();
    let split = lines.iter().rposition(|l| *l == "}").map_or(0, |i| i + 1);
    let decls = lines[..split].join("\n");
    let stmts = lines[split..].join("\n");
    let wrap = |body: &str| format!("namespace Spec;\nfunction example(): void\n{{\n{body}\n}}\n");
    let per_line: Vec<String> = code
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| match l.find("//") {
            Some(i) => format!("{};{}", &l[..i], &l[i..]),
            None => format!("{l};"),
        })
        .collect();
    let modes: [(&str, String); 4] = [
        ("declarations", format!("namespace Spec;\n{code}\n")),
        ("statements", wrap(&code)),
        ("expressions", wrap(&per_line.join("\n"))),
        (
            "declarations and statements",
            format!("{}{decls}", wrap(&stmts)),
        ),
    ];
    let mut last = Vec::new();
    for (mode, text) in &modes {
        let name = format!("spec-example-{index}.scb");
        let (out, diags) = render(sources, &name, text);
        if diags
            .iter()
            .all(|d| !matches!(d.kind, "syntax-error" | "top-level-statement"))
        {
            return Ok(format!("mode: {mode}\n{out}"));
        }
        last = diags.iter().map(|d| d.render(sources)).collect();
    }
    Err(last.join("\n"))
}

#[test]
fn spec_examples() {
    let spec_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/SPEC.md");
    let spec = std::fs::read_to_string(spec_path).unwrap();
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in spec.lines() {
        match &mut current {
            None if line.trim_start() == "```php" => current = Some(String::new()),
            Some(code) if line.trim_start() == "```" => {
                blocks.push(std::mem::take(code));
                current = None;
            }
            Some(code) => {
                code.push_str(line);
                code.push('\n');
            }
            None => {}
        }
    }
    assert!(
        blocks.len() >= 19,
        "found only {} SPEC blocks",
        blocks.len()
    );

    let mut sources = SourceMap::new();
    let mut out = String::new();
    let mut problems = Vec::new();
    for (i, code) in blocks.iter().enumerate() {
        let first = code.lines().next().unwrap_or("").trim();
        let _ = writeln!(out, "=== example {i}: {first}");
        if let Some((_, why)) = NOT_SOURCE.iter().find(|(s, _)| code.contains(s)) {
            let _ = writeln!(out, "skipped: {why}\n");
            continue;
        }
        match parse_example(&mut sources, i, code) {
            Ok(dump) => {
                out.push_str(&dump);
                out.push('\n');
            }
            Err(e) => problems.push(format!("SPEC example {i} ({first}) does not parse:\n{e}")),
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n\n"));
    if let Err(e) = check_golden(&golden_dir().join("spec_examples.ast"), &out) {
        panic!("{e}");
    }
}
