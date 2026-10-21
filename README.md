# SCB

SCB (Semi-colons and Curly Braces): a statically typed, PHP-derived language with value semantics, no type coercion, and a JIT/AOT-compilable design. This repository holds the spec and the Rust POC interpreter.

## Layout

| Path | Contents |
| --- | --- |
| `CLAUDE.md` | Instructions for Claude Code working in this repo |
| `docs/SPEC.md` | Language specification (source of truth) |
| `docs/POC_PLAN.md` | POC milestones M0–M8 with exit criteria |
| `docs/OPEN_QUESTIONS.md` | Spec gaps found so far and how they were resolved |
| `docs/DIAGNOSTICS.md` | Compile-error kinds, output format, `scb` exit codes |
| `crates/` | The Rust workspace: `scb-syntax`, `scb-types`, `scb-runtime`, `scb-stdlib`, `scb-cli`, `scb-conformance` |
| `tests/conformance/` | Language conformance tests (format in its README) |
| `examples/hello/` | Small program exercising args, interpolation and struct value semantics |
| `examples/cgi/` | CGI smoke test behind NGINX via fcgiwrap (milestone M8) |

## Building and running

The interpreter is a Cargo workspace.

```sh
cargo run -p scb-cli -- run path/to/main.scb [args…]   # run a program
cargo run -p scb-cli -- check path/to/main.scb         # type-check only
cargo run -p scb-conformance [-- <path-filter>]        # conformance tests
```

Quality bar before a milestone is done: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
