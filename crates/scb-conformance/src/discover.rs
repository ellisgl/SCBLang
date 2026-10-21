//! Finding tests under `tests/conformance`.

use std::io;
use std::path::{Path, PathBuf};

use crate::header::{self, Spec};

/// One test: a single `.scb` file, or a directory whose `main.scb` carries the header.
pub struct TestCase {
    /// Path relative to the conformance root, `/`-separated.
    pub name: String,
    /// The file, or the directory for a multi-file test.
    pub path: PathBuf,
    /// The parsed header, or a format error.
    pub spec: Result<Spec, String>,
}

pub fn discover(root: &Path) -> io::Result<Vec<TestCase>> {
    let mut cases = Vec::new();
    walk(root, root, &mut cases)?;
    Ok(cases)
}

fn walk(dir: &Path, root: &Path, cases: &mut Vec<TestCase>) -> io::Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    entries.sort();
    for path in entries {
        if path.is_dir() {
            let main = path.join("main.scb");
            if main.is_file() && has_expectation(&main) {
                cases.push(case(root, &path, &main));
            } else {
                walk(&path, root, cases)?;
            }
        } else if path.extension().is_some_and(|ext| ext == "scb") {
            cases.push(case(root, &path, &path));
        }
    }
    Ok(())
}

/// A directory is a multi-file test when its `main.scb` header has an `EXPECT-*` directive.
fn has_expectation(main: &Path) -> bool {
    std::fs::read_to_string(main)
        .map(|text| {
            text.lines()
                .take_while(|l| l.starts_with("//"))
                .any(|l| l.starts_with("// EXPECT-"))
        })
        .unwrap_or(false)
}

fn case(root: &Path, path: &Path, header_file: &Path) -> TestCase {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let name = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let spec = std::fs::read_to_string(header_file)
        .map_err(|e| format!("cannot read {}: {e}", header_file.display()))
        .and_then(|text| header::parse(&text));
    TestCase {
        name,
        path: path.to_path_buf(),
        spec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conformance_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance")
    }

    #[test]
    fn every_repository_test_has_a_valid_header() {
        let cases = discover(&conformance_root()).unwrap();
        assert!(cases.len() >= 100, "found only {} tests", cases.len());
        let bad: Vec<String> = cases
            .iter()
            .filter_map(|c| c.spec.as_ref().err().map(|e| format!("{}: {e}", c.name)))
            .collect();
        assert!(bad.is_empty(), "format errors:\n{}", bad.join("\n"));
    }

    #[test]
    fn every_expected_kind_is_documented() {
        let docs = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/DIAGNOSTICS.md"),
        )
        .unwrap();
        let undocumented: Vec<String> = discover(&conformance_root())
            .unwrap()
            .into_iter()
            .filter_map(|c| c.spec.ok()?.compile_error)
            .filter(|kind| kind != "*" && !docs.contains(&format!("`{kind}`")))
            .collect();
        assert!(
            undocumented.is_empty(),
            "kinds missing from docs/DIAGNOSTICS.md: {undocumented:?}"
        );
    }

    #[test]
    fn multi_file_directories_are_one_test() {
        let cases = discover(&conformance_root()).unwrap();
        assert!(
            cases
                .iter()
                .any(|c| c.name == "modules/directory_mode" && c.path.is_dir())
        );
        assert!(
            !cases
                .iter()
                .any(|c| c.name.starts_with("modules/directory_mode/"))
        );
    }
}
