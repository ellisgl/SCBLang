# Diagnostics

Compile errors reported by `scb check` and `scb run`. Kinds are stable identifiers; tests match on them (`// EXPECT-COMPILE-ERROR: <kind>`).

## Output format

One diagnostic per entry on stderr:

```
path/to/file.scb:LINE:COL: error[kind]: message
  note: extra context (zero or more)
  fix: suggested change (optional)
```

`LINE` and `COL` are 1-based; `COL` counts codepoints. Diagnostics without a source position omit the `path:LINE:COL: ` prefix.

## `scb` exit codes

| Code | Meaning |
| --- | --- |
| 0–255 | `scb run`: the program's own exit code (SPEC "Source files and program entry"); an uncaught throwable exits 1 |
| 0 | `scb check`: no errors |
| 2 | Compile error (one or more diagnostics on stderr) |
| 64 | Usage error (bad command line, path is neither a `.scb` file nor a directory) |
| 66 | Input missing or unreadable |
| 70 | Internal: interpreter not implemented yet (M0–M3 only) |

A program can itself exit with 2, 64, 66 or 70; tools that need to tell the cases apart check stderr for `error[`.

## Kinds

| Kind | Meaning | SPEC section |
| --- | --- | --- |
| `not-implemented` | A compiler stage that does not exist yet. Never satisfies a conformance test | — (POC scaffolding) |
| `invalid-utf8` | Source file is not valid UTF-8 | Lexical structure |
| `missing-main` | Directory mode without `main.scb` | Source files and program entry |
| `ambiguous-interpolation` | `"$a->b"`, `"$a?->b"` or `"$a[0]"` in a string | Strings → Interpolation |
| `assign-to-capture` | Assignment to a variable captured by a closure | Closures and captured state |
| `bare-binding-pattern` | Top-level `$var` pattern without a guard | Enums and match |
| `bitwise-on-bool` | `&`, `\|`, `^` on `bool` | Numerics |
| `bool-cast` | `(bool) $x` | Casts and conversions |
| `cannot-infer` | Collection literal with no declared type, e.g. `[]` | Collections and values |
| `control-leaves-finally` | `return` / `break` / `continue` directly in a `finally` body | Errors |
| `exclusivity-violation` | Overlapping modifying access to a local | Exclusivity |
| `literal-out-of-range` | Numeric literal does not fit its type | Numerics |
| `missing-case-arguments` | Data-carrying enum case used without arguments | Enums and match |
| `missing-parent-constructor-call` | Subclass constructor does not call `parent::__construct()` exactly once | Structs, classes and interfaces |
| `missing-return-type` | Function, method or closure without a return type | Grammar sketch → Notes |
| `mutating-temporary` | Write or `inout` through a value-typed temporary | Nested mutation |
| `namespace-mismatch` | Directory mode with differing namespaces | Modules and packages |
| `non-bool-condition` | Condition (`if`, `while`, ternary, …) not of type `bool` | Type system |
| `non-exhaustive-match` | `match` not covering every case (guarded arms do not count) | Enums and match |
| `not-supported-in-poc` | Syntax of a feature outside POC scope | Implementation plan → POC scope |
| `override-final` | Overriding a `final` method | Structs, classes and interfaces |
| `prelude-name-redeclared` | Type, function or constant named after a prelude name | Lexical structure |
| `redeclared-variable` | Declaring or binding a name that is still visible | Type system |
| `redundant-final` | `final` where it is meaningless | Structs, classes and interfaces |
| `reserved-namespace` | User file declaring a stdlib root namespace | Modules and packages |
| `short-ternary` | `?:` | Grammar sketch → Notes |
| `string-subscript` | `$s[0]` on a `string` | Strings |
| `tuple-index` | Tuple index that is not an in-range integer literal | Collections and values |
| `type-mismatch` | Operand, argument, assignment or arm of the wrong type | Type system |
| `undeclared-variable` | Use of, or assignment to, a variable not declared in scope | Type system |
| `uninitialized-property` | Property not definitely assigned before construction ends, or `$this` used too early | Structs, classes and interfaces |
| `unresolved-name` | Name that does not resolve (including relative `Env\get` and `f16`) | Modules and packages |
| `unsigned-negation` | Unary `-` on a non-literal unsigned value | Numerics |
| `write-through-nullsafe` | Write or `inout` through `?->` | Nested mutation |
