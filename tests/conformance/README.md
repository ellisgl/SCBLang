# Conformance tests

One `.scb` file per test (or one directory; see Multi-file tests), grouped by area in subdirectories. Expectations live in a header of `//` comments at the top of the file, before `namespace`.

## Directives

| Directive | Meaning |
| --- | --- |
| `// EXPECT-STDOUT:` | Followed by zero or more `// \| <text>` lines. Stdout must equal those lines, each followed by `\n`. An empty block means no output. |
| `// EXPECT-STDOUT-NO-FINAL-NEWLINE` | With `EXPECT-STDOUT`: the last expected line is not followed by `\n`. |
| `// EXPECT-STDERR-CONTAINS: <text>` | Stderr must contain `<text>`. Repeatable. Also applies to compile-error tests (e.g. to check a fix-it). |
| `// EXPECT-EXIT: <n>` | Process exit code. Default 0, or 1 when `EXPECT-RUNTIME-ERROR` is present. Not allowed with `EXPECT-COMPILE-ERROR` (always exit 2). |
| `// EXPECT-COMPILE-ERROR: <kind>` | `scb check` must fail with a diagnostic of that kind (see `docs/DIAGNOSTICS.md`). `<kind>` may be `*` to accept any compile error while kinds are still being named. No run happens. |
| `// EXPECT-RUNTIME-ERROR: <Class>` | The program compiles, runs, and dies with an uncaught throwable of that class; stderr contains the class name. May be combined with `EXPECT-STDOUT` to check output written before the error. |
| `// STDIN:` | Followed by `// \| <text>` lines fed to stdin. |
| `// STDIN-RAW: <escaped>` | Exact stdin bytes on one line, using SCB double-quoted escapes (`\n`, `\r`, `\t`, `\\`, `\xHH` including `\x80`–`\xFF`). No newline is added. Use instead of `STDIN` for CRLF, a missing final newline, or invalid UTF-8. |
| `// ENV: NAME=value` | Environment variable visible to `Env\get()`. Repeatable. Nothing else from the real environment is visible. |
| `// ENV-RAW: NAME=<escaped>` | As `ENV`, with the value in `STDIN-RAW` escapes (e.g. invalid UTF-8). |
| `// ARG: <escaped>` | One command-line argument passed to the program (`$args`), in `STDIN-RAW` escapes. Repeatable, in order. Default: none. |
| `// SKIP: <reason>` | Test is reported as skipped. |

Each file has exactly one of: `EXPECT-COMPILE-ERROR`; `EXPECT-STDOUT`; `EXPECT-RUNTIME-ERROR`; or `EXPECT-STDOUT` together with `EXPECT-RUNTIME-ERROR`. Without `EXPECT-STDOUT`, stdout is not checked.

Text lines (`// | <text>`, used by `EXPECT-STDOUT` and `STDIN`): the runner strips `// |` and one following space; the rest is literal. A bare `// |` is an empty line. A text line ending in whitespace is a format error (so an editor stripping it cannot silently change a test).

## Multi-file tests

A directory containing a `main.scb` whose header has an `EXPECT-*` directive is one test, run in directory mode (`scb run <dir>/`). Its files are not discovered as separate tests. Directives live only in `main.scb`.

## Conventions

- Every test file uses `namespace Conformance;` (single-file mode; SPEC "Modules").
- Test names describe the behavior: `cow_list_copy.scb`, not `test1.scb`.
- Compile-error tests contain exactly one error.
- When a test exercises a `SPEC-OPEN` provisional behavior, add `// SPEC-OPEN: Qn` to its header.

## Runner

`cargo run -p scb-conformance [-- <path-filter>]`. Output: one line per test (`PASS`/`FAIL`/`SKIP`), a diff for failures, and totals. Exit non-zero if any test fails.
