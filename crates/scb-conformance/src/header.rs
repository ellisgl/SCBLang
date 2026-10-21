//! Parsing of the `//` directive header at the top of a conformance test.

/// What a test expects and the inputs it runs with.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Spec {
    pub stdout: Option<Vec<u8>>,
    pub stderr_contains: Vec<String>,
    pub exit: Option<i32>,
    pub compile_error: Option<String>,
    pub runtime_error: Option<String>,
    pub stdin: Vec<u8>,
    pub env: Vec<(String, Vec<u8>)>,
    pub args: Vec<Vec<u8>>,
    pub skip: Option<String>,
}

impl Spec {
    /// Exit code the run must end with.
    pub fn expected_exit(&self) -> i32 {
        self.exit
            .unwrap_or(if self.runtime_error.is_some() { 1 } else { 0 })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    None,
    Stdout,
    Stdin,
}

/// Parses the header: the leading run of lines that start with `//`.
pub fn parse(source: &str) -> Result<Spec, String> {
    let mut spec = Spec::default();
    let mut block = Block::None;
    let mut stdout_lines: Option<Vec<String>> = None;
    let mut stdin_lines: Option<Vec<String>> = None;
    let mut stdin_raw: Option<Vec<u8>> = None;
    let mut no_final_newline = false;

    for (index, line) in source.lines().enumerate() {
        let Some(rest) = line.strip_prefix("//") else {
            break;
        };
        let lineno = index + 1;
        let err = |msg: String| format!("line {lineno}: {msg}");

        if rest == " |" || rest.starts_with(" | ") {
            let text = rest.get(3..).unwrap_or("");
            if text.ends_with(char::is_whitespace) {
                return Err(err("text line ends in whitespace".into()));
            }
            match block {
                Block::Stdout => stdout_lines
                    .get_or_insert_with(Vec::new)
                    .push(text.to_string()),
                Block::Stdin => stdin_lines
                    .get_or_insert_with(Vec::new)
                    .push(text.to_string()),
                Block::None => {
                    return Err(err(
                        "`// |` line outside an EXPECT-STDOUT or STDIN block".into()
                    ));
                }
            }
            continue;
        }

        let directive = rest
            .strip_prefix(' ')
            .ok_or_else(|| err("expected `// DIRECTIVE`".into()))?;
        let (name, value) = match directive.split_once(':') {
            Some((name, value)) => (name, Some(value.strip_prefix(' ').unwrap_or(value))),
            None => (directive, None),
        };
        block = Block::None;
        let need = || value.ok_or_else(|| err(format!("{name} needs a value")));
        let once = |taken: bool| {
            if taken {
                Err(err(format!("{name} given twice")))
            } else {
                Ok(())
            }
        };

        match name {
            "EXPECT-STDOUT" => {
                once(stdout_lines.is_some())?;
                if value.is_some_and(|v| !v.is_empty()) {
                    return Err(err("EXPECT-STDOUT takes no value; use `// |` lines".into()));
                }
                stdout_lines = Some(Vec::new());
                block = Block::Stdout;
            }
            "EXPECT-STDOUT-NO-FINAL-NEWLINE" => {
                once(no_final_newline)?;
                no_final_newline = true;
            }
            "EXPECT-STDERR-CONTAINS" => spec.stderr_contains.push(need()?.to_string()),
            "EXPECT-EXIT" => {
                once(spec.exit.is_some())?;
                let v = need()?;
                spec.exit = Some(v.parse().map_err(|_| err(format!("bad exit code `{v}`")))?);
            }
            "EXPECT-COMPILE-ERROR" => {
                once(spec.compile_error.is_some())?;
                spec.compile_error = Some(need()?.to_string());
            }
            "EXPECT-RUNTIME-ERROR" => {
                once(spec.runtime_error.is_some())?;
                spec.runtime_error = Some(need()?.to_string());
            }
            "STDIN" => {
                once(stdin_lines.is_some())?;
                stdin_lines = Some(Vec::new());
                block = Block::Stdin;
            }
            "STDIN-RAW" => {
                once(stdin_raw.is_some())?;
                stdin_raw = Some(decode_escapes(need()?).map_err(err)?);
            }
            "ENV" | "ENV-RAW" => {
                let v = need()?;
                let (key, val) = v
                    .split_once('=')
                    .ok_or_else(|| err(format!("{name} needs NAME=value")))?;
                let val = if name == "ENV" {
                    val.as_bytes().to_vec()
                } else {
                    decode_escapes(val).map_err(err)?
                };
                spec.env.push((key.to_string(), val));
            }
            "ARG" => spec.args.push(decode_escapes(need()?).map_err(err)?),
            "SKIP" => {
                once(spec.skip.is_some())?;
                spec.skip = Some(need()?.to_string());
            }
            "SPEC-OPEN" => {}
            other => return Err(err(format!("unknown directive `{other}`"))),
        }
    }

    if stdin_lines.is_some() && stdin_raw.is_some() {
        return Err("STDIN and STDIN-RAW are mutually exclusive".into());
    }
    if let Some(lines) = stdin_lines {
        spec.stdin = lines
            .iter()
            .flat_map(|l| format!("{l}\n").into_bytes())
            .collect();
    } else if let Some(raw) = stdin_raw {
        spec.stdin = raw;
    }

    if let Some(lines) = stdout_lines {
        let mut out: Vec<u8> = lines
            .iter()
            .flat_map(|l| format!("{l}\n").into_bytes())
            .collect();
        if no_final_newline && out.pop().is_none() {
            return Err("EXPECT-STDOUT-NO-FINAL-NEWLINE needs at least one expected line".into());
        }
        spec.stdout = Some(out);
    } else if no_final_newline {
        return Err("EXPECT-STDOUT-NO-FINAL-NEWLINE without EXPECT-STDOUT".into());
    }

    match (
        &spec.compile_error,
        spec.stdout.is_some() || spec.runtime_error.is_some() || spec.exit.is_some(),
    ) {
        (Some(_), true) => Err(
            "EXPECT-COMPILE-ERROR cannot be combined with EXPECT-STDOUT, EXPECT-RUNTIME-ERROR or EXPECT-EXIT (a compile error always exits 2)"
                .into(),
        ),
        (None, false) => Err(
            "no expectation: need EXPECT-COMPILE-ERROR, EXPECT-STDOUT or EXPECT-RUNTIME-ERROR"
                .into(),
        ),
        _ => Ok(spec),
    }
}

/// Decodes SCB double-quoted escapes (SPEC "String literals"), with `\xHH` allowed
/// for the whole byte range as in `b"…"` literals.
pub fn decode_escapes(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('r') => out.push(b'\r'),
            Some('v') => out.push(0x0B),
            Some('e') => out.push(0x1B),
            Some('f') => out.push(0x0C),
            Some('0') => out.push(0),
            Some('\\') => out.push(b'\\'),
            Some('$') => out.push(b'$'),
            Some('"') => out.push(b'"'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                // `from_str_radix` alone would accept a sign (`+F`).
                let byte = u8::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 2 && hex.chars().all(|c| c.is_ascii_hexdigit()))
                    .ok_or_else(|| format!("bad escape `\\x{hex}`"))?;
                out.push(byte);
            }
            Some('u') => {
                if chars.next() != Some('{') {
                    return Err("bad escape: expected `\\u{…}`".into());
                }
                let mut hex = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    hex.push(c);
                }
                let scalar = u32::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| closed && hex.chars().all(|c| c.is_ascii_hexdigit()))
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("bad escape `\\u{{{hex}`"))?;
                let mut buf = [0; 4];
                out.extend_from_slice(scalar.encode_utf8(&mut buf).as_bytes());
            }
            Some(other) => return Err(format!("unknown escape `\\{other}`")),
            None => return Err("trailing `\\`".into()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdout_block_with_empty_line_and_no_final_newline() {
        let spec = parse("// EXPECT-STDOUT:\n// | a\n// |\n// | b\n// EXPECT-STDOUT-NO-FINAL-NEWLINE\n\nnamespace X;\n").unwrap();
        assert_eq!(spec.stdout.as_deref(), Some(&b"a\n\nb"[..]));
        assert_eq!(spec.expected_exit(), 0);
    }

    #[test]
    fn empty_stdout_block_means_no_output() {
        let spec = parse("// EXPECT-STDOUT:\n// EXPECT-EXIT: 2\nnamespace X;").unwrap();
        assert_eq!(spec.stdout.as_deref(), Some(&b""[..]));
        assert_eq!(spec.expected_exit(), 2);
    }

    #[test]
    fn runtime_error_defaults_exit_to_one_and_combines_with_stdout() {
        let spec = parse("// EXPECT-STDOUT:\n// | x\n// EXPECT-RUNTIME-ERROR: KeyError\n").unwrap();
        assert_eq!(spec.runtime_error.as_deref(), Some("KeyError"));
        assert_eq!(spec.expected_exit(), 1);
    }

    #[test]
    fn inputs_stdin_env_args() {
        let spec = parse(
            "// STDIN:\n// | one\n// ENV: A=b=c\n// ENV-RAW: B=\\xFF\n// ARG: two words\n// ARG: \\x00\n// EXPECT-STDOUT:\n",
        )
        .unwrap();
        assert_eq!(spec.stdin, b"one\n");
        assert_eq!(
            spec.env,
            vec![("A".into(), b"b=c".to_vec()), ("B".into(), vec![0xFF])]
        );
        assert_eq!(spec.args, vec![b"two words".to_vec(), vec![0]]);
    }

    #[test]
    fn stdin_raw_is_exact() {
        let spec = parse("// STDIN-RAW: a\\r\\nb\n// EXPECT-STDOUT:\n").unwrap();
        assert_eq!(spec.stdin, b"a\r\nb");
    }

    #[test]
    fn rejects_format_errors() {
        for (src, why) in [
            ("namespace X;", "no expectation"),
            (
                "// EXPECT-COMPILE-ERROR: x\n// EXPECT-STDOUT:\n",
                "cannot be combined",
            ),
            ("// EXPECT-STDOUT:\n// | trailing \n", "whitespace"),
            ("// | stray\n// EXPECT-STDOUT:\n", "outside"),
            ("// EXPECT-STDOUT:\n// BOGUS: 1\n", "unknown directive"),
            (
                "// STDIN:\n// STDIN-RAW: x\n// EXPECT-STDOUT:\n",
                "mutually exclusive",
            ),
            ("// EXPECT-EXIT: one\n// EXPECT-STDOUT:\n", "bad exit code"),
            ("// EXPECT-STDOUT:\n// EXPECT-STDOUT:\n", "twice"),
            (
                "// EXPECT-COMPILE-ERROR: x\n// EXPECT-EXIT: 2\n",
                "cannot be combined",
            ),
        ] {
            let e = parse(src).unwrap_err();
            assert!(e.contains(why), "{src:?}: {e}");
        }
    }

    #[test]
    fn header_stops_at_first_non_comment_line() {
        let spec =
            parse("// EXPECT-COMPILE-ERROR: type-mismatch\n\n// BOGUS later comment\n").unwrap();
        assert_eq!(spec.compile_error.as_deref(), Some("type-mismatch"));
    }

    #[test]
    fn escapes() {
        assert_eq!(
            decode_escapes(r"a\tb\x41\u{e9}\\\$").unwrap(),
            b"a\tbA\xC3\xA9\\$"
        );
        assert!(decode_escapes(r"\q").is_err());
        assert!(decode_escapes(r"\x4").is_err());
        assert!(decode_escapes(r"\x+F").is_err());
        assert!(decode_escapes(r"\u{+41}").is_err());
        assert!(decode_escapes(r"\u{D800}").is_err());
        assert!(decode_escapes(r"\u{41").is_err());
    }
}
