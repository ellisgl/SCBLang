//! Every `.scb` file in the repository lexes and parses cleanly, except conformance
//! tests that expect a lexical or syntax diagnostic, which must produce exactly that kind.

use std::path::{Path, PathBuf};

use scb_syntax::{SourceMap, parse_source};

/// Diagnostic kinds the lexer and parser report (see `docs/DIAGNOSTICS.md`).
const SYNTAX_KINDS: &[&str] = &[
    // lexer
    "bom",
    "invalid-character",
    "hash-comment",
    "unterminated-comment",
    "unterminated-string",
    "unknown-escape",
    "invalid-escape",
    "legacy-octal",
    "invalid-separator",
    "invalid-number",
    "bare-range-operator",
    "invalid-variable",
    "ambiguous-interpolation",
    "invalid-interpolation",
    "bytes-interpolation",
    "invalid-heredoc",
    // parser
    "syntax-error",
    "removed-syntax",
    "not-supported-in-poc",
    "missing-namespace",
    "top-level-statement",
    "missing-type",
    "missing-return-type",
    "missing-visibility",
    "invalid-assignment-target",
    "short-ternary",
    "bool-cast",
    "bare-binding-pattern",
    "match-true",
    "redundant-final",
];

/// Tests whose expected kind is one the parser can report, but whose case needs name
/// resolution or class analysis (M3), so the parser must accept them.
const CHECKER_CASES: &[&str] = &[
    "tests/conformance/poc_scope/callable_interface.scb",
    "tests/conformance/objects/final_in_final_class_is_error.scb",
];

fn scb_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            scb_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "scb") {
            out.push(path);
        }
    }
}

/// The `EXPECT-COMPILE-ERROR` kind of a test, looking at `main.scb` for a multi-file test.
fn expected_kind(path: &Path) -> Option<String> {
    let header_file = path.with_file_name("main.scb");
    let header_file = if header_file.is_file() {
        header_file
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(header_file).ok()?;
    text.lines()
        .take_while(|l| l.starts_with("//"))
        .find_map(|l| {
            l.strip_prefix("// EXPECT-COMPILE-ERROR: ")
                .map(str::to_string)
        })
}

#[test]
fn repository_sources_parse_as_expected() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    scb_files(&root.join("tests"), &mut files);
    scb_files(&root.join("examples"), &mut files);
    assert!(files.len() > 100);

    let mut problems = Vec::new();
    let mut sources = SourceMap::new();
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let id = sources.add(path, std::fs::read(path).unwrap()).unwrap();
        let (_, diags) = parse_source(id, &sources.file(id).text);
        let got: Vec<&str> = diags.iter().map(|d| d.kind).collect();
        let want = expected_kind(path)
            .filter(|k| SYNTAX_KINDS.contains(&k.as_str()))
            .filter(|_| !CHECKER_CASES.contains(&rel_str.as_str()));
        let ok = match &want {
            // In a multi-file test the error may come from any of its files.
            Some(kind) if path.with_file_name("main.scb").is_file() => {
                got.iter().all(|g| g == kind)
            }
            Some(kind) => !got.is_empty() && got.iter().all(|g| g == kind),
            None => got.is_empty(),
        };
        if !ok {
            let detail: Vec<String> = diags.iter().map(|d| d.render(&sources)).collect();
            problems.push(format!(
                "{rel_str}: expected {want:?}, got:\n    {}",
                detail.join("\n    ")
            ));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

/// The parser never panics or loops on broken input: every repository file is parsed
/// truncated at many points and with single tokens removed.
#[test]
fn broken_inputs_terminate_without_panicking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    scb_files(&root.join("tests"), &mut files);
    let mut sources = SourceMap::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let cuts: Vec<usize> = (0..text.len())
            .step_by(11)
            .filter(|&i| text.is_char_boundary(i))
            .collect();
        for &cut in &cuts {
            let id = sources.add(path, text.as_bytes()[..cut].to_vec()).unwrap();
            let _ = parse_source(id, &sources.file(id).text);
        }
        // Drop each token in turn (by its span).
        let id = sources.add(path, text.as_bytes().to_vec()).unwrap();
        let (tokens, _) = scb_syntax::lex(id, &sources.file(id).text);
        for t in tokens.iter().step_by(3) {
            let (s, e) = (t.span.start as usize, t.span.end as usize);
            let mutated = format!("{}{}", &text[..s], &text[e..]);
            let id = sources.add(path, mutated.into_bytes()).unwrap();
            let _ = parse_source(id, &sources.file(id).text);
        }
    }
}
