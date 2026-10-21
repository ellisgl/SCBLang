//! Every `.scb` file in the repository lexes cleanly, except conformance tests that
//! expect a lexical diagnostic, which must produce exactly that kind.

use std::path::{Path, PathBuf};

use scb_syntax::{SourceMap, lex};

/// Diagnostic kinds the lexer reports (see `docs/DIAGNOSTICS.md`).
const LEXER_KINDS: &[&str] = &[
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
fn repository_sources_lex_as_expected() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    scb_files(&root.join("tests"), &mut files);
    scb_files(&root.join("examples"), &mut files);
    assert!(files.len() > 100);

    let mut problems = Vec::new();
    let mut sources = SourceMap::new();
    for path in &files {
        let id = sources.add(path, std::fs::read(path).unwrap()).unwrap();
        let (_, diags) = lex(id, &sources.file(id).text);
        let got: Vec<&str> = diags.iter().map(|d| d.kind).collect();
        let want = expected_kind(path).filter(|k| LEXER_KINDS.contains(&k.as_str()));
        let ok = match &want {
            // In a multi-file test the error may come from any of its files.
            Some(kind) if path.with_file_name("main.scb").is_file() => {
                got.iter().all(|g| g == kind)
            }
            Some(kind) => got == [kind.as_str()],
            None => got.is_empty(),
        };
        if !ok {
            let rel = path.strip_prefix(&root).unwrap_or(path);
            problems.push(format!(
                "{}: expected {want:?}, lexer reported {got:?}",
                rel.display()
            ));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}
