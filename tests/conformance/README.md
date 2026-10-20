# Conformance tests

One `.scb` file per test, grouped by area in subdirectories. Expectations live in a header of `//` comments at the top of the file, before `namespace`.

## Directives

| Directive | Meaning |
| --- | --- |
| `// EXPECT-STDOUT:` | Followed by zero or more `// \| <text>` lines. Stdout must equal those lines, each followed by `\n`. An empty block means no output. |
| `// EXPECT-STDOUT-NO-FINAL-NEWLINE` | With `EXPECT-STDOUT`: the last expected line is not followed by `\n`. |
| `// EXPECT-STDERR-CONTAINS: <text>` | Stderr must contain `<text>`. Repeatable. |
| `// EXPECT-EXIT: <n>` | Process exit code. Default 0, or 1 when `EXPECT-RUNTIME-ERROR` is present. |
| `// EXPECT-COMPILE-ERROR: <kind>` | `scb check` must fail with a diagnostic of that kind (see `docs/DIAGNOSTICS.md`). `<kind>` may be `*` to accept any compile error while kinds are still being named. No run happens. |
| `// EXPECT-RUNTIME-ERROR: <Class>` | The program compiles, runs, and dies with an uncaught throwable of that class; stderr contains the class name. May be combined with `EXPECT-STDOUT` to check output written before the error. |
| `// STDIN:` | Followed by `// \| <text>` lines fed to stdin. |
| `// ENV: NAME=value` | Environment variable visible to `Env\get()`. Repeatable. Nothing else from the real environment is visible. |
| `// SKIP: <reason>` | Test is reported as skipped. |

Each file has exactly one of: `EXPECT-COMPILE-ERROR`; `EXPECT-STDOUT`; `EXPECT-RUNTIME-ERROR`; or `EXPECT-STDOUT` together with `EXPECT-RUNTIME-ERROR`. Without `EXPECT-STDOUT`, stdout is not checked.

Text lines (`// | <text>`, used by `EXPECT-STDOUT` and `STDIN`): the runner strips `// |` and one following space; the rest is literal. A bare `// |` is an empty line. A text line ending in whitespace is a format error (so an editor stripping it cannot silently change a test).

## Conventions

- Every test file uses `namespace Conformance;` (single-file mode; see `docs/OPEN_QUESTIONS.md` Q3).
- Test names describe the behavior: `cow_list_copy.scb`, not `test1.scb`.
- Compile-error tests contain exactly one error.
- When a test exercises a `SPEC-OPEN` provisional behavior, add `// SPEC-OPEN: Qn` to its header.

## Runner

`cargo run -p scb-conformance [-- <path-filter>]`. Output: one line per test (`PASS`/`FAIL`/`SKIP`), a diff for failures, and totals. Exit non-zero if any test fails.
