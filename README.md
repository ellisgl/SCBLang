# SCB

SCB (Semi-colons and Curly Braces): a statically typed, PHP-derived language with value semantics, no type coercion, and a JIT/AOT-compilable design. This repository holds the spec and the Rust POC interpreter.

## Layout

| Path | Contents |
| --- | --- |
| `CLAUDE.md` | Instructions for Claude Code working in this repo |
| `docs/SPEC.md` | Language specification (source of truth) |
| `docs/POC_PLAN.md` | POC milestones M0–M8 with exit criteria |
| `docs/OPEN_QUESTIONS.md` | Spec gaps found so far, with provisional POC behavior |
| `tests/conformance/` | Language conformance tests (format in its README) |
| `examples/hello/` | Small program exercising args, interpolation and struct value semantics |
| `examples/cgi/` | CGI smoke test behind NGINX via fcgiwrap (milestone M8) |

## Building and running

The interpreter is a Cargo workspace (set up in milestone M0).

```sh
cargo run -p scb-cli -- run path/to/main.scb [args…]   # run a program
cargo run -p scb-cli -- check path/to/main.scb         # type-check only
cargo run -p scb-conformance [-- <path-filter>]        # conformance tests
```

Quality bar before a milestone is done: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
