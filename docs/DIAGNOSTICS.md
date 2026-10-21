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
| 70 | Internal: the program could not be started (the OS refused its thread) |
| 134 | `scb run`: the program called `Process\abort()` |

A program can itself exit with 2, 64, 66 or 70; tools that need to tell the cases apart check stderr for `error[`.

## Kinds

| Kind | Meaning | SPEC section |
| --- | --- | --- |
| `not-implemented` | A compiler stage that does not exist yet. Never satisfies a conformance test | — (POC scaffolding) |
| `internal-error` | The compiler could not run (e.g. the OS refused to start its thread) | — |
| `too-deeply-nested` | Nesting beyond the parser's depth limit (512 levels; each operator in a chain counts one level) | — (implementation limit) |
| `invalid-utf8` | Source file is not valid UTF-8 | Lexical structure |
| `file-too-large` | Source file of 4 GiB or more (positions are 32-bit) | — (implementation limit) |
| `missing-main` | Directory mode without `main.scb` | Source files and program entry |
| `bom` | Byte order mark at the start of a file | Lexical structure |
| `invalid-character` | Character that cannot start a token (e.g. non-ASCII outside strings and comments, `@`) | Lexical structure |
| `hash-comment` | `#` comment (only `#[` is valid) | Lexical structure |
| `unterminated-comment` | `/*` or `/**` never closed | Lexical structure |
| `unterminated-string` | String literal or `{$…}` interpolation never closed | String literals |
| `unknown-escape` | Escape not in the table (`"\q"`), or `\0` followed by a digit (octal) | String literals |
| `invalid-escape` | Malformed `\x`/`\u{…}`, a non-scalar codepoint, or `\x80`–`\xFF` outside `b"…"` | String literals |
| `legacy-octal` | Integer literal with a leading `0` (`0377`) | Numeric literals |
| `invalid-separator` | `_` not between two digits | Numeric literals |
| `invalid-number` | `.5`, `1.`, literal suffixes, bad digits for the radix, leading zeros in floats | Numeric literals |
| `bare-range-operator` | `..` without `=` or `<` | Ranges |
| `invalid-variable` | `$` without a name, including variable variables (`$$x`, `${x}`) | Lexical structure; Removed from PHP |
| `invalid-interpolation` | `"${name}"` | Strings → Interpolation |
| `bytes-interpolation` | `$name` or `{$…}` inside `b"…"` | String literals |
| `invalid-heredoc` | Missing closing marker, under-indented body line, mixed tab/space indentation | String literals |
| `ambiguous-interpolation` | `"$a->b"`, `"$a?->b"` or `"$a[0]"` in a string | Strings → Interpolation |
| `syntax-error` | Input that does not match the grammar; the message names what was expected. Also: chained non-associative operators (`$a < $b < $c`), unparenthesized nested ternaries, a local declaration without an initializer, block arms in a `match` expression, PHP's prefix types (`int $x`), modifiers out of order or repeated | Grammar sketch |
| `removed-syntax` | PHP syntax SCB removed, with a targeted fix-it: `<?php`, `declare`, `===` / `!==`, `and` / `or` / `xor`, `switch`, `&` references, `function () use`, `static fn`, `isset` / `empty` / `unset` / `array()` / `list()` / `eval` / `compact` / `extract` before `(`, alternative syntax, braceless bodies, `else if`, `global`, `static` locals, `goto`, `var`, traits, `new $class`, `->$name`, `instanceof $var`, PHP-only casts, the `mixed` / `callable` / `iterable` / `object` / `array` types | Removed from PHP |
| `missing-namespace` | File without a leading `namespace` declaration | Modules and packages |
| `top-level-statement` | Statement outside a function (files contain declarations only) | Source files and program entry |
| `missing-type` | Parameter, property or constant without a type | Type system |
| `missing-visibility` | Property, method or class constant without a visibility | Structs, classes and interfaces |
| `invalid-assignment-target` | Assignment to something that is not a variable, property, element or static property | Nested mutation |
| `match-true` | `match (true)` with condition arms (use the subjectless `match`) | Enums and match |
| `assign-to-capture` | Assignment to a variable captured by a closure | Closures and captured state |
| `bare-binding-pattern` | Top-level `$var` pattern without a guard | Enums and match |
| `bitwise-on-bool` | `&`, `\|`, `^` on `bool` | Numerics |
| `bool-cast` | `(bool) $x` | Casts and conversions |
| `cannot-infer` | Collection literal with no declared type, e.g. `[]` | Collections and values |
| `assert-not-callable` | `assert(...)` / `debugAssert(...)` used as a value | Errors |
| `control-leaves-finally` | `return` / `break` / `continue` directly in a `finally` body | Errors |
| `exclusivity-violation` | Overlapping modifying access to a local | Exclusivity |
| `literal-out-of-range` | Numeric literal does not fit its type (the lexer reports values beyond 64 bits) | Numerics |
| `missing-case-arguments` | Data-carrying enum case used without arguments | Enums and match |
| `missing-parent-constructor-call` | Subclass constructor does not call `parent::__construct()` exactly once | Structs, classes and interfaces |
| `missing-return-type` | Function, method, closure or function type without a return type | Grammar sketch → Notes |
| `mutating-temporary` | Write or `inout` through a value-typed temporary; a `mutating` method of a value type used as a function value (Q44) | Nested mutation |
| `namespace-mismatch` | Directory mode with differing namespaces | Modules and packages |
| `non-bool-condition` | Condition (`if`, `while`, ternary, …) not of type `bool` | Type system |
| `non-exhaustive-match` | `match` not covering every case (guarded arms do not count) | Enums and match |
| `not-supported-in-poc` | Syntax of a feature outside POC scope; also a map key or set element type hashed by a user `hash()` | Implementation plan → POC scope |
| `override-final` | Overriding a `final` method | Structs, classes and interfaces |
| `prelude-name-redeclared` | Type, function or constant named after a prelude name | Lexical structure |
| `redeclared-variable` | Declaring or binding a name that is still visible | Type system |
| `redundant-final` | `final` where it is meaningless (on a class, or on a method that cannot be overridden) | Structs, classes and interfaces |
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
| `abstract-method` | Abstract method outside an abstract class, or a non-interface method without a body | Structs, classes and interfaces |
| `abstract-call` | Calling a method without a body where nothing dispatches: `parent::m()`, `parent(I)::m()`, `X::m()`, `X::m(...)` | Structs, classes and interfaces |
| `clone-value-type` | `clone` on a struct or enum (assignment already copies) | clone |
| `default-method-conflict` | Two interface defaults for one method, not overridden by the class | Default-method conflicts |
| `duplicate-declaration` | A type, function, constant, member or enum case declared twice | Modules and packages |
| `extend-final` | Extending a class that is not `extendable` or `abstract` | Structs, classes and interfaces |
| `incompatible-override` | Overriding method with different parameter types or an unrelated return type | Structs, classes and interfaces |
| `inheritance-cycle` | A class that extends itself, directly or indirectly | Structs, classes and interfaces |
| `inout-mismatch` | `inout` argument for a by-value parameter, or the reverse | Collections and values (`inout`) |
| `invalid-break` | `break` / `continue` outside a loop, or with a depth larger than the loop nesting | Grammar sketch |
| `invalid-main` | `main` with a signature other than the four allowed | Source files and program entry |
| `missing-method` | Concrete class, struct or enum missing an abstract or interface method | Structs, classes and interfaces |
| `missing-mutating` | Struct method changing `$this` without being `mutating` | Structs, classes and interfaces |
| `missing-return` | A non-`void` function that can end without returning (or a `never` function that can return) | Type system |
| `non-constant-expression` | Constant initializer or default value that is not a compile-time constant | Functions; Mutability |
| `readonly-write` | Write to a `readonly` local, property or static outside its initialization, to a `const`, or to `$this`; `mutating` call on a readonly value | Mutability; Nested mutation |
| `refutable-pattern` | Destructuring or `foreach` pattern that may not match | Destructuring outside match |
| `static-call` | Instance method called statically, or static method called on an instance | Structs, classes and interfaces |
| `unknown-member` | Property, method, constant or enum case that does not exist on the type | Structs, classes and interfaces |
| `unsatisfied-bound` | A type lacking a required capability: `==` on a non-`Equatable` type, `<` on a non-`Comparable` one, a non-`Hashable` map key, a conditional method (`join`, `sort`, …) whose `where` bound fails | Equality and comparison; Generics |
| `visibility` | Access to a private or protected member from outside where it is visible | Structs, classes and interfaces |
| `wrong-arguments` | Missing, extra, unknown, duplicate or misplaced arguments | Functions |
| `const-cycle` | A constant whose initializer depends on itself | Mutability |
| `invalid-member` | A property in an interface or enum; `__destruct` outside a class or with parameters | Structs, classes and interfaces |
| `invalid-modifier` | `mutating` on a class, enum or static method | Structs, classes and interfaces |
| `wrong-type-arguments` | Wrong number of type arguments (`list<int, int>`), or type arguments on a non-generic type | Type system |
