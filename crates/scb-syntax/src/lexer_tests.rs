use std::path::Path;

use super::lex;
use crate::token::{Keyword as K, Punct as P, StrPart, TokenKind as T};
use crate::{FileId, SourceMap};

fn file() -> FileId {
    SourceMap::new()
        .add(Path::new("t.scb"), Vec::new())
        .unwrap()
}

/// Token kinds without the trailing `Eof`; fails on any diagnostic.
fn kinds(src: &str) -> Vec<T> {
    let (tokens, diags) = lex(file(), src);
    assert!(
        diags.is_empty(),
        "{src:?}: unexpected diagnostics {diags:?}"
    );
    let mut kinds: Vec<T> = tokens.into_iter().map(|t| t.kind).collect();
    assert_eq!(kinds.pop(), Some(T::Eof));
    kinds
}

fn one(src: &str) -> T {
    let mut k = kinds(src);
    assert_eq!(k.len(), 1, "{src:?}: {k:?}");
    k.remove(0)
}

/// Diagnostic kinds for `src`.
fn errors(src: &str) -> Vec<&'static str> {
    lex(file(), src).1.into_iter().map(|d| d.kind).collect()
}

fn parts(src: &str) -> Vec<StrPart> {
    match one(src) {
        T::Str(s) => s.parts,
        other => panic!("{src:?}: expected a string, got {other:?}"),
    }
}

fn text(src: &str) -> String {
    match parts(src).as_slice() {
        [] => String::new(),
        [StrPart::Text(t)] => t.clone(),
        other => panic!("{src:?}: expected plain text, got {other:?}"),
    }
}

fn ident(s: &str) -> T {
    T::Ident(s.into())
}

fn var(s: &str) -> T {
    T::Variable(s.into())
}

fn float(s: &str) -> T {
    T::Float(s.into())
}

// ---------- SPEC numeric literals table ----------

#[test]
fn integer_forms() {
    assert_eq!(one("255"), T::Int(255));
    assert_eq!(one("0xFF"), T::Int(255));
    assert_eq!(one("0o377"), T::Int(255));
    assert_eq!(one("0b1111"), T::Int(15));
    assert_eq!(one("0"), T::Int(0));
    assert_eq!(one("18446744073709551615"), T::Int(u64::MAX));
}

#[test]
fn legacy_octal_is_an_error() {
    assert_eq!(errors("0377"), ["legacy-octal"]);
    assert_eq!(errors("00"), ["legacy-octal"]);
}

#[test]
fn separators() {
    assert_eq!(one("1_000_000"), T::Int(1_000_000));
    assert_eq!(one("0xFF_FF"), T::Int(0xFFFF));
    assert_eq!(one("1_000.000_5"), float("1000.0005"));
    for bad in ["1__0", "1_000_", "0x_FF", "1_.5", "1.5_", "1e1_"] {
        assert_eq!(errors(bad), ["invalid-separator"], "{bad}");
    }
}

#[test]
fn float_forms() {
    assert_eq!(one("0.5"), float("0.5"));
    assert_eq!(one("1.5e-3"), float("1.5e-3"));
    assert_eq!(one("1e10"), float("1e10"));
    assert_eq!(one("2e+3"), float("2e+3"));
}

#[test]
fn float_needs_leading_and_trailing_digits() {
    assert_eq!(errors(".5"), ["invalid-number"]);
    assert_eq!(errors("1."), ["invalid-number"]);
    assert_eq!(errors("1.e5"), ["invalid-number"]);
    assert_eq!(errors("01.5"), ["invalid-number"]);
}

#[test]
fn no_suffixes_or_bad_digits() {
    assert_eq!(errors("200u8"), ["invalid-number"]);
    assert_eq!(errors("1.5f32"), ["invalid-number"]);
    assert_eq!(errors("1E5"), ["invalid-number"]);
    assert_eq!(errors("0b102"), ["invalid-number"]);
    assert_eq!(errors("0x"), ["invalid-number"]);
    assert_eq!(errors("0XFF"), ["invalid-number"]);
}

#[test]
fn integer_too_large_for_64_bits() {
    assert_eq!(errors("18446744073709551616"), ["literal-out-of-range"]);
}

#[test]
fn special_values_are_static_access() {
    assert_eq!(
        kinds("f64::NAN"),
        [ident("f64"), T::Punct(P::ColonColon), ident("NAN")]
    );
}

#[test]
fn digit_then_range_lexes_as_int_and_operator() {
    assert_eq!(
        kinds("1..<3"),
        [T::Int(1), T::Punct(P::DotDotLt), T::Int(3)]
    );
    assert_eq!(
        kinds("0..=9"),
        [T::Int(0), T::Punct(P::DotDotEq), T::Int(9)]
    );
    assert_eq!(
        kinds("1..=-1"),
        [
            T::Int(1),
            T::Punct(P::DotDotEq),
            T::Punct(P::Minus),
            T::Int(1)
        ]
    );
    assert_eq!(kinds("...$xs"), [T::Punct(P::Ellipsis), var("xs")]);
    assert_eq!(errors("1..5"), ["bare-range-operator"]);
}

// ---------- SPEC string literals table ----------

#[test]
fn single_quoted_escapes_only_backslash_and_quote() {
    assert_eq!(text(r"'a\'b\\c\n$x'"), r"a'b\c\n$x");
}

#[test]
fn double_quoted_escapes() {
    assert_eq!(
        text(r#""\n\t\r\v\e\f\0\\\$\"""#),
        "\n\t\r\u{0B}\u{1B}\u{0C}\0\\$\""
    );
    assert_eq!(text(r#""\u{1F600}\u{e9}\x41\x7F""#), "😀éA\x7F");
}

#[test]
fn escape_errors() {
    assert_eq!(errors(r#""\q""#), ["unknown-escape"]);
    assert_eq!(errors(r#""\012""#), ["unknown-escape"]);
    assert_eq!(errors(r#""\x80""#), ["invalid-escape"]);
    assert_eq!(errors(r#""\x4""#), ["invalid-escape"]);
    assert_eq!(errors(r#""\u{D800}""#), ["invalid-escape"]);
    assert_eq!(errors(r#""\u{110000}""#), ["invalid-escape"]);
    assert_eq!(errors(r#""\u{+41}""#), ["invalid-escape"]);
    assert_eq!(errors(r#""\u41""#), ["invalid-escape"]);
}

#[test]
fn bytes_literal_allows_high_bytes() {
    assert_eq!(
        one(r#"b"\x80\xFFa\n""#),
        T::Bytes(vec![0x80, 0xFF, b'a', b'\n'])
    );
    assert_eq!(one(r#"b"é""#), T::Bytes("é".as_bytes().to_vec()));
}

#[test]
fn bytes_literal_does_not_interpolate() {
    assert_eq!(errors(r#"b"$x""#), ["bytes-interpolation"]);
    assert_eq!(errors(r#"b"{$x}""#), ["bytes-interpolation"]);
    assert_eq!(one(r#"b"\$x""#), T::Bytes(b"$x".to_vec()));
}

#[test]
fn simple_interpolation() {
    let p = parts(r#""Hi $name!""#);
    assert!(
        matches!(p.as_slice(), [StrPart::Text(a), StrPart::Var { name, .. }, StrPart::Text(b)]
        if a == "Hi " && &**name == "name" && b == "!")
    );
    // `$` not followed by a name is literal.
    assert_eq!(text(r#""cost: $ 5, $1""#), "cost: $ 5, $1");
}

#[test]
fn brace_interpolation_lexes_the_expression() {
    let p = parts(r#""{$a->b} and {$m['k']}""#);
    let [
        StrPart::Expr { tokens: first, .. },
        StrPart::Text(mid),
        StrPart::Expr { tokens: second, .. },
    ] = p.as_slice()
    else {
        panic!("{p:?}")
    };
    let k: Vec<&T> = first.iter().map(|t| &t.kind).collect();
    assert_eq!(k, [&var("a"), &T::Punct(P::Arrow), &ident("b"), &T::Eof]);
    assert_eq!(mid, " and ");
    assert_eq!(second.len(), 5); // $m [ 'k' ] Eof
}

#[test]
fn brace_interpolation_tracks_nested_braces() {
    let p = parts(r#""{$f(fn(): int { return 1; })}!""#);
    assert!(
        matches!(p.as_slice(), [StrPart::Expr { .. }, StrPart::Text(t)] if t == "!"),
        "{p:?}"
    );
}

#[test]
fn ambiguous_interpolation() {
    for src in [r#""Hi $user->name""#, r#""$a?->b""#, r#""Item $list[0]""#] {
        assert_eq!(errors(src), ["ambiguous-interpolation"], "{src}");
    }
    assert!(errors(r#""{$name}->x""#).is_empty());
    assert!(errors(r#""\$a->b""#).is_empty());
}

#[test]
fn dollar_brace_is_rejected() {
    assert_eq!(errors(r#""${name}""#), ["invalid-interpolation"]);
}

#[test]
fn crlf_inside_literal_becomes_lf() {
    assert_eq!(text("\"a\r\nb\""), "a\nb");
    assert_eq!(text("'a\r\nb'"), "a\nb");
}

#[test]
fn unterminated_string() {
    assert_eq!(errors(r#""abc"#), ["unterminated-string"]);
    assert_eq!(errors("'abc"), ["unterminated-string"]);
    assert_eq!(errors(r#""{$a""#), ["unterminated-string"]);
}

// ---------- heredoc / nowdoc ----------

#[test]
fn heredoc_with_flexible_closing_indentation() {
    let src = "<<<EOT\n    Hello $name\n      indented\n\n    EOT;";
    let (tokens, diags) = lex(file(), src);
    assert!(diags.is_empty(), "{diags:?}");
    let T::Str(s) = &tokens[0].kind else {
        panic!("{tokens:?}")
    };
    assert!(
        matches!(s.parts.as_slice(),
        [StrPart::Text(a), StrPart::Var { name, .. }, StrPart::Text(b)]
        if a == "Hello " && &**name == "name" && b == "\n  indented\n"),
        "{:?}",
        s.parts
    );
    assert_eq!(tokens[1].kind, T::Punct(P::Semi));
}

#[test]
fn heredoc_quoted_name_and_escapes() {
    assert_eq!(text("<<<\"X\"\na\\tb \"q\"\nX"), "a\tb \"q\"");
}

#[test]
fn nowdoc_is_raw() {
    assert_eq!(text("<<<'EOT'\n  $x \\n {$y}\n  EOT"), "$x \\n {$y}");
}

#[test]
fn empty_heredoc_and_crlf() {
    assert_eq!(text("<<<EOT\nEOT"), "");
    assert_eq!(text("<<<EOT\r\n  a\r\n  b\r\n  EOT"), "a\nb");
}

#[test]
fn heredoc_errors() {
    assert_eq!(errors("<<<EOT\n  a\n    EOT"), ["invalid-heredoc"]);
    assert_eq!(errors("<<<EOT\na\n"), ["invalid-heredoc"]);
    assert_eq!(errors("<<<EOT\n \ta\n \tEOT"), ["invalid-heredoc"]);
}

#[test]
fn closing_marker_must_not_continue_as_identifier() {
    assert_eq!(text("<<<EOT\nEOTX\nEOT"), "EOTX");
}

// ---------- identifiers, keywords, variables ----------

#[test]
fn keywords_are_lowercase_and_case_sensitive() {
    assert_eq!(kinds("function If"), [T::Keyword(K::Function), ident("If")]);
    assert_eq!(
        kinds("self static"),
        [T::Keyword(K::SelfKw), T::Keyword(K::Static)]
    );
    assert_eq!(
        kinds("true false null"),
        [
            T::Keyword(K::True),
            T::Keyword(K::False),
            T::Keyword(K::Null)
        ]
    );
}

#[test]
fn contextual_keywords_and_type_names_are_identifiers() {
    assert_eq!(
        kinds("where of set type int list"),
        ["where", "of", "set", "type", "int", "list"].map(ident)
    );
}

#[test]
fn underscore_and_variables() {
    assert_eq!(
        kinds("_ $_ $this $a1"),
        [T::Underscore, var("_"), var("this"), var("a1")]
    );
    assert_eq!(errors("$$x"), ["invalid-variable"]);
    assert_eq!(errors("${x}"), ["invalid-variable"]);
    assert_eq!(errors("$1"), ["invalid-variable"]);
}

#[test]
fn identifiers_are_ascii_only() {
    assert_eq!(errors("café"), ["invalid-character"]);
    assert_eq!(errors("@foo()"), ["invalid-character"]);
}

#[test]
fn qualified_names_lex_as_parts() {
    assert_eq!(
        kinds(r"\Env\get"),
        [
            T::Punct(P::Backslash),
            ident("Env"),
            T::Punct(P::Backslash),
            ident("get")
        ]
    );
}

// ---------- comments, attributes, BOM ----------

#[test]
fn comments_doc_comments_and_attributes() {
    let k = kinds("// line\n/* block */ /**/ /** doc */ #[Test] x");
    assert_eq!(
        k,
        [
            T::DocComment(" doc ".into()),
            T::AttrOpen,
            ident("Test"),
            T::Punct(P::RBracket),
            ident("x")
        ]
    );
}

#[test]
fn hash_comment_is_an_error() {
    assert_eq!(errors("# old style\nx"), ["hash-comment"]);
}

#[test]
fn unterminated_block_comment() {
    assert_eq!(errors("/* never closed"), ["unterminated-comment"]);
}

#[test]
fn bom_is_an_error() {
    let (tokens, diags) = lex(file(), "\u{FEFF}namespace X;");
    assert_eq!(diags.iter().map(|d| d.kind).collect::<Vec<_>>(), ["bom"]);
    assert_eq!(tokens[0].kind, T::Keyword(K::Namespace));
}

// ---------- operators ----------

#[test]
fn operators_use_longest_match() {
    let src = "??= ?-> ?? <=> <<= >>= **= ~/= ~/ ... ..= ..< === !== |> :: => -> .= ++ --";
    let got: Vec<&str> = kinds(src)
        .into_iter()
        .map(|k| match k {
            T::Punct(p) => p.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(got, src.split(' ').collect::<Vec<_>>());
}

#[test]
fn spans_are_byte_ranges() {
    let (tokens, _) = lex(file(), "  $abc é");
    assert_eq!((tokens[0].span.start, tokens[0].span.end), (2, 6));
}
