use std::path::{Path, PathBuf};

use crate::Diagnostic;

/// Index of a file in a [`SourceMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(u32);

/// Byte range `start..end` in one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file: FileId, start: u32, end: u32) -> Self {
        Span { file, start, end }
    }
}

/// One `.scb` file, already validated as UTF-8.
#[derive(Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    pub text: String,
    /// Byte offset of the start of each line.
    line_starts: Vec<u32>,
}

impl SourceFile {
    /// 1-based line and column (column counted in codepoints) of a byte offset.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[line] as usize;
        let end = (offset as usize).min(self.text.len());
        let col = self.text[start..end].chars().count();
        (line as u32 + 1, col as u32 + 1)
    }
}

/// All source files of one compilation (single-file or directory mode).
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        SourceMap::default()
    }

    /// Adds a file from raw bytes. Source files must be valid UTF-8 (SPEC "Lexical structure").
    pub fn add(&mut self, path: &Path, bytes: Vec<u8>) -> Result<FileId, Diagnostic> {
        let text = String::from_utf8(bytes).map_err(|e| {
            Diagnostic::new("invalid-utf8", "source file is not valid UTF-8").with_note(format!(
                "{}: invalid byte at offset {}",
                path.display(),
                e.utf8_error().valid_up_to()
            ))
        })?;
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i as u32 + 1));
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile {
            path: path.to_path_buf(),
            text,
            line_starts,
        });
        Ok(id)
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn files(&self) -> impl Iterator<Item = (FileId, &SourceFile)> {
        self.files
            .iter()
            .enumerate()
            .map(|(i, f)| (FileId(i as u32), f))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_codepoints() {
        let mut map = SourceMap::new();
        let id = map
            .add(Path::new("a.scb"), "ab\nhé x\n".as_bytes().to_vec())
            .unwrap();
        let file = map.file(id);
        assert_eq!(file.line_col(0), (1, 1));
        assert_eq!(file.line_col(3), (2, 1));
        // "x" is after 'h', 'é' (2 bytes) and a space: byte 3 + 4 = 7, column 4.
        assert_eq!(file.line_col(7), (2, 4));
    }

    #[test]
    fn rejects_invalid_utf8() {
        let mut map = SourceMap::new();
        let err = map.add(Path::new("bad.scb"), vec![b'a', 0xFF]).unwrap_err();
        assert_eq!(err.kind, "invalid-utf8");
        assert!(map.is_empty());
    }
}
