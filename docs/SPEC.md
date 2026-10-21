# SCB Language Specification

SCB (Semi-colons and Curly Braces) is a statically typed, PHP-derived language: scripted first via JIT, AOT-compilable from the same IR.

## Goals

- Scripting language first (JIT), compiled (AOT) later; both backends share one IR.
- Everything resolvable at compile time: no feature may require runtime name lookup.
- No type coercion anywhere. Statically typed.
- Familiar to PHP developers; where SCB departs from PHP it is to remove a known footgun.
- Code files contain only code: no open/close tags, no context switching.
- Migration: a PHP-to-SCB migration guide (after POC testing and approval; tooling later).

## Source files and program entry

- Source files are `.scb`, UTF-8. Templates are `.scbt`.
- Files contain declarations only; no top-level statements.
- The package manifest names the entry file; default `main.scb`. Libraries have no `main`.
- Without a manifest, the argument picks the mode. `scb run path/to/file.scb` compiles exactly that file (single-file mode), which must define `main`. `scb run path/to/dir/` compiles every `.scb` file directly in that directory (not subdirectories) as one module; `main` must be defined in `main.scb`. `scb check` takes the same arguments. A manifest (`scb.toml`, post-POC) adds package mode without changing either form.

Allowed `main` signatures:

```php
function main(): void
function main(): int
function main($args: list<string>): void
function main($args: list<string>): int
```

- `$args` holds user arguments only; the program name comes from `Process\name()`.
- Environment and stdin come from the stdlib: `Env\get()`, `Io\readLine()`, `Io\readAll()`. A reader/stream API (`Io\Reader`, alongside `Io\Writer`) is post-POC.
- `main` runs inside the root fiber, so I/O can suspend without setup.
- A `void` return exits 0. A returned `int` outside 0–255 raises a `ValueError` on POSIX (reported like any uncaught throwable, exit 1).
- An uncaught throwable prints a stack trace to stderr and exits 1.
- Stdout is buffered (size implementation-defined). The buffer is flushed when full, before a read from stdin, on normal return from `main`, on `exit`, on an uncaught throwable before the trace is printed, and on a fatal error (e.g. a throwing `__destruct`) before its message: output already echoed is never lost. Stderr is unbuffered. `Process\abort()` does not flush.
- A failed write to stdout (e.g. a closed pipe) throws `IoException`, from `echo` / `print` as from `Io\print()`. Because stdout is buffered, the failure may surface at a later write or flush than the one that produced the output. If the final flush at program end fails, the error is reported on stderr and the exit code is 1.
- `Process\exit($code: int): never` unwinds, running `finally` blocks and destructors. A `$code` outside 0–255 raises a `ValueError` at the `exit`, as a `main` return does (the code is never truncated or clamped). It is not a throwable: no `catch` clause, including `catch (Throwable)`, intercepts it. `Process\abort()` terminates immediately.
- If `exit` is reached while a `finally` block runs because of an earlier throwable or `exit`, the newest `exit` wins. What it replaces is reported on stderr, never dropped silently: a discarded throwable prints its trace marked "discarded by exit(n) at file:line"; a discarded exit code prints "exit(m) at file:line superseded by exit(n) at file:line". Likewise a `throw` leaving a `finally` while an `exit` is pending wins, and the replaced exit prints "exit(m) at file:line superseded by throw at file:line".

### Built-in statements

Keywords that desugar to stdlib calls (there is no global function fallback, so these are keywords, not aliases):

| Keyword | Desugars to | Rules |
| --- | --- | --- |
| `echo $a, $b;` | `\Io\print((string) $a); \Io\print((string) $b);` | Statement only; each operand must be `Stringable` (see Strings → Interpolation). No conversion happens for a `string` |
| `print $a;` | `\Io\print((string) $a);` | Statement only; no return value; operand rule as `echo` |
| `exit;` / `exit($code);` | `\Process\exit($code)` | `int` only, default 0; type `never`. A string argument is a compile error. `exit(…)` is also an expression (like `throw`): `\Env\get('PORT') ?? exit(2)`. The bare `exit` without parentheses is statement-only |
| `die` | Same as `exit` | Same as `exit` |

- `isset` and `empty` are removed: use `$map->has($k)`, `?->`, `??`, `!= null`, `$list->isEmpty()`, `$s == ''`. `unset($m[$k])` becomes `$m->remove($k)`. `isset`, `empty`, `unset`, `array`, `list`, `eval`, `compact` and `extract` are not keywords, but followed by `(` they are a compile error (`removed-syntax`) with a fix-it naming the replacement; `$x->empty()` and `list<T>` are unaffected.
- Magic constants kept: `::class`, `__FILE__`, `__LINE__`, `__DIR__`, `__CLASS__`, `__FUNCTION__`, `__METHOD__`, `__NAMESPACE__`. All resolve at compile time.
- Assignment is an expression: with `$line: ?string = null;` declared, `while (($line = \Io\readLine()) != null)` works; `if ($x = foo())` compiles only when `foo()` returns `bool`.

## Style and naming

Code follows PER Coding Style, enforced by the built-in formatter (`scb fmt`). Deviations from PER:

- No opening tag; no `declare(strict_types)`.
- Types are postfix everywhere: `$name: string`, `const MAX: i64`, `catch ($e: IoException)`; return types `): T`.
- Generics written `list<T>`, no inner spaces; function types written `fn(int): bool`.
- Modifier order: `abstract` / `final` → visibility → `static` → `readonly` → `mutating` (as in PER: `final public function`); `extendable` / `abstract` before `class`. The order is part of the grammar: out-of-order or repeated modifiers are a compile error.

| Element | Convention | Example |
| --- | --- | --- |
| Types | PascalCase | `UserRepository` |
| Functions and methods | camelCase | `byteLength()` |
| Constants and enum cases | UPPER_SNAKE | `MAX_SIZE`, `Shape::CIRCLE` |
| Stdlib | Grouped by namespace | `Str\`, `Math\`, `Io\` |
| Keywords | lowercase | `instanceof`, `fn` |

Standard argument order for every stdlib function:

1. Subject (the thing operated on)
2. Operands (needle, key, value)
3. Replacement or output
4. Options and flags
5. Callback last

The `$` sigil is kept on variables and properties.

## Lexical structure

- **Encoding:** UTF-8 required. A BOM is a compile error (the formatter strips it). LF and CRLF accepted; the formatter normalizes to LF. A CRLF inside a string, heredoc or nowdoc literal is read as LF, so a literal's value never depends on the file's line endings (`"\r\n"` still produces CRLF).
- **Identifiers:** ASCII only, `[A-Za-z_][A-Za-z0-9_]*`. Unicode is allowed in strings and comments.
- **Case:** everything is case-sensitive: functions, classes, namespaces, keywords. Keywords are lowercase only. Namespace-to-directory matching is case-exact.
- **Variables:** `$` + identifier; `$this` reserved. Superglobals (`$_GET`, `$_POST`, `$_SERVER`, …) are removed.
- **Comments:** `//`, non-nesting `/* */`, and `/** */` doc comments attached to the next declaration. No `#` comments; `#[` begins an attribute.
- **Statements:** semicolons required; no automatic insertion.

Reserved keywords:

```
abstract as break case catch class clone const continue default die do echo else
elseif enum exit extendable extends extern false final finally fn for foreach function if
implements inout instanceof interface internal is match mutating namespace never new
null parent print private protected public readonly return self static struct throw
true try unsafe use void while yield
```

- Contextual keywords (ordinary identifiers elsewhere): `in`, `out`, `of`, `get`, `set`, `type`, `where`.
- The prelude holds exactly the names the compiler itself depends on:
  - primitives: `bool`, `int`, `i8` `i16` `i32` `i64`, `u8` `u16` `u32` `u64`, `float`, `f32`, `f64`, `string`, `bytes`;
  - collections: `list`, `map`, `set`, `tuple`;
  - language types: `Range` (built by `..=` / `..<`), `Generator` (built by `yield`), `Iterable` (consumed by `foreach`);
  - operator interfaces: `Stringable`, `Equatable`, `Comparable`, `Hashable`, `Callable`, `Sendable`. `string`, every integer type and both float types implement `Stringable`; `bool` does not;
  - the throwable hierarchy (`Throwable`, `Error`, `Exception`, `IndexError`, …) and `Ref`.

  `void`, `never` and `null` are keywords. Everything else in the stdlib lives in a namespace (`\Math\Rounding`, `\Str\Form`, `\Float\Key`, `\Time\Duration`, …). No prelude name may be declared as a type, function or constant (`prelude-name-redeclared`); in particular `(i8)` is always a cast. This applies to template functions too: `List.scbt` must be renamed (e.g. `UserList.scbt`).
- `extern` and `unsafe` are reserved for future FFI.
- `_` is reserved: never an identifier; it is the wildcard in patterns and the discard in destructuring (`[$a, _] = $pair;`). `$_` is an ordinary variable.
- Reserved words are allowed as names where the position is unambiguous: named-argument labels (`default:`), method and property names after `->`, `?->`, `::`, class constant and enum case names, and any segment of a qualified name after a `\` (`\Io\print`). Never as the name in a function, class or type declaration.

Numeric literals:

| Form | Example | Rule |
| --- | --- | --- |
| Decimal / hex / octal / binary | `255`, `0xFF`, `0o377`, `0b1111` | No legacy `0377` octal; no leading zeros. Prefixes are lowercase only (`0X1F` is an error); hex digits may be either case |
| Separators | `1_000_000` | `_` only between digits |
| Float | `0.5`, `1.5e-3`, `1e10` | Leading digit required (no `.5`); no trailing dot (no `1.`); no leading zeros (no `01.5`); exponent marker `e` lowercase only (no `1E5`) |
| Suffixes | none | Spelling sets the kind (`1` int, `1.0` float); the declared type sets the width (see Numerics) |
| Special values | `f64::NAN`, `f64::INFINITY` | No global `NAN` / `INF` |

String literals:

| Form | Escapes |
| --- | --- |
| `'single'` | `\\` and `\'` only |
| `"double"` | `\n \t \r \v \e \f \0 \\ \$ \"`, `\u{1F600}` (valid scalar, no surrogates), `\x00`–`\x7F` |
| `b"bytes"` | Same plus `\x80`–`\xFF`; type `bytes`. No interpolation: an unescaped `$` is a compile error (`bytes-interpolation`); write `\$` |
| Heredoc / nowdoc | Kept, with flexible closing-marker indentation |

Octal escapes are dropped. Unknown escapes (`"\q"`) are a compile error. `\0` followed by a digit (`"\012"`) is a compile error (`unknown-escape`), since PHP reads it as octal; write `\x0A`, or `\0` then `\x31` for NUL followed by a digit.

## Numerics

| Kind | Types | Default | Alias |
| --- | --- | --- | --- |
| Signed int | `i8` `i16` `i32` `i64` | `i64` | `int` |
| Unsigned int | `u8` `u16` `u32` `u64` | — | — |
| Float | `f32` `f64` | `f64` | `float` |

- **A literal's kind is fixed by its spelling.** `1` is an integer literal and `1.0` a float literal; neither ever becomes the other. `$f: f64 = 1;` is a compile error (write `1.0`), and so is `2.5 * 2`.
- **Within its kind, a literal takes the width required where it appears** (declared type, parameter type, the other operand), range-checked at compile time: `$a: u8 = 200;` is fine; `$b: u8 = 300;` is a compile error; with `$x: i8`, `$x += 1` uses an `i8` `1`. Where nothing requires a width, integer literals are `i64` and float literals `f64`.
- **Negative literals:** unary `-` applied directly to a numeric literal forms one negative literal, range-checked as such: `$x: i8 = -128;` is fine, `-129` is out of range. The lexer still emits `-` separately (`$a-1` is a subtraction), and precedence is unchanged (`-2 ** 2` is `-(2 ** 2)`).
- **Unsigned negation:** unary `-` on an unsigned value that is not a literal is a compile error (`unsigned-negation`); write `0 - $u` (traps on underflow) or `u8::wrappingNeg($u)`.
- **No implicit widening.** `i32 + i64` is a compile error; cast explicitly.
- **Casts:** `(i8) $x` traps if the value is out of range. Wrapping and truncating conversions are explicit stdlib calls.
- **Overflow traps** (`OverflowError`) in every build mode.
- **Count and index type:** `i64`.

Division and remainder:

| Operator | Operands | Result | Notes |
| --- | --- | --- | --- |
| `/` | same-typed floats | float | `int / int` is a compile error |
| `~/` | same-typed ints | int | Truncates toward zero; `~/=` compound form |
| `%` | same-typed ints or floats | same type | Sign follows the dividend; floats behave as fmod |

- Exponent `**`: for an integer base of type `T`, the exponent is `u32` and the result is `T` (literal exponents adapt: `$x ** 2`; a variable needs `(u32) $n`, which traps `CastError` if negative). Overflow traps (`OverflowError`); `0 ** 0` is 1. For floats both operands are the same float type, with IEEE `pow` semantics.
- `MIN ~/ -1` traps (overflow).
- Integer divide by zero traps (`DivisionByZeroError`); floats follow IEEE (inf / NaN).
- Helpers: `Math\floorDiv($a, $b)`, `Math\mod($a, $b)` (non-negative for a positive divisor).

### Casts and conversions

Primitive types carry static methods for conversions and checked arithmetic, e.g. `i8::wrap($x)`.

| Conversion | Form | Behavior |
| --- | --- | --- |
| Int → narrower int | `(i8) $x` | Traps on out of range (`CastError`) |
| | `i8::wrap($x)` | Wraps modulo 2^8 |
| | `i8::saturate($x)` | Clamps to `i8::MIN` / `i8::MAX` |
| | `i8::tryFrom($x)` | Returns `?i8` |
| Float → int | `(i64) $f` | Truncates toward zero; traps on NaN, ±inf, out of range |
| | `i64::round($f, \Math\Rounding::HALF_EVEN)` | Modes: `HALF_UP`, `HALF_EVEN`, `FLOOR`, `CEIL`, `TRUNCATE` |
| Int → float | `(f64) $i` | Rounds to nearest (lossy above 2^53) |
| | `f64::exactFrom($i)` | Returns `?f64` |
| Float → narrower float | `(f32) $d` | Rounds to nearest; overflow becomes ±inf |
| Value → string | `(string) $x` | `Stringable` only (includes `string`, ints, floats, and unions whose members all are) |
| String → number | `int::parse($s)` | Throws `ParseException` |
| | `int::tryParse($s)` | Returns `?int` |
| Any → bool | `(bool)` | Banned; write `$x != 0` |

Float to string (`(string)`, `echo`, interpolation): the shortest digits that round-trip to the same value of that type (`f32` uses `f32` precision). Integral values keep `.0` (`3.0`, `-0.0`). Plain decimal when `1e-6 ≤ |x| < 1e21`, otherwise exponent form with the same mantissa rule and no `+` or leading zeros (`1.0e21`, `1.5e-7`). `NAN`, `INF`, `-INF`. Rounded display is an explicit formatting call (post-POC).

Casts exist only for the primitive types in this table. `(Name) expr` with any other name is not a cast: it parses as a parenthesized expression, so `(MAX) - 1` is a subtraction. Conversions involving classes, structs and enums use methods (`Status::from()`) or `instanceof` narrowing.

Checked arithmetic, for `Add`, `Sub`, `Mul`, `Neg`, `Pow`. Plain operators still trap:

```php
i32::wrappingAdd($a, $b)    // wraps
i32::saturatingAdd($a, $b)  // clamps
i32::checkedAdd($a, $b)     // ?i32
```

Bitwise `&`, `|`, `^`, `~` take same-typed integers only. On `bool` they are a compile error (`bitwise-on-bool`; fix-its `&&`, `||`, and `!=` for XOR).

Shifts: the amount is always `u32` (literal amounts adapt; a variable needs `(u32) $n`, which traps `CastError` if negative). An amount ≥ the bit width traps (`OverflowError`); `wrappingShl` / `wrappingShr` mask it. `>>` is arithmetic for signed types, logical for unsigned.

## Strings

- `string` is always valid UTF-8. `bytes` holds raw data.
- Encoding conversion happens only at I/O boundaries: `$bytes->decode('ISO-8859-1'): string` (throws `EncodingException`; post-POC), alongside `decodeUtf8()`.
- No integer subscript (`$s[0]` is a compile error). Use explicit views:

| View | Element | Indexing | Length |
| --- | --- | --- | --- |
| `$s->bytes()` (returns `bytes`) | `u8` | O(1) | `byteLength()` |
| `$s->codepoints()` (returns `list<u32>`) | `u32` | iterate | `codepointLength()` |
| `$s->graphemes()` (returns `list<string>`) | `string` | iterate | `graphemeLength()` |

- No bare `length()`; no `char` type.
- Search and slicing (`$s->find()`, `$s->slice()`) use byte offsets. Slicing at a non-boundary traps (`BoundaryError`).
- `==` and `<=>` compare bytes (equal to codepoint order). Normalization is explicit: `\Str\normalize($s, \Str\Form::NFC)`. Locale collation is a separate API (`Str\Collator`).
- Concatenation: `.` and `.=`. Both operands must be `string`; anything else is a compile error (use interpolation or `(string)`).

Interpolation:

- `"$name"` and `"{$expr}"`; `{}` accepts any expression. Single quotes do not interpolate. Heredoc and nowdoc are kept.
- The simple form is `$name` only. `$name` immediately followed by `->`, `?->` or `[` is a compile error (`ambiguous-interpolation`, fix-it: `{$name->prop}`); PHP's one-level `"$a->b"` / `"$a[0]"` forms are not supported, so nothing is silently printed literally. For a literal `->` after a variable write `{$name}->`.
- PHP's `"${name}"` form is a compile error (`invalid-interpolation`, fix-it: `{$name}`).
- Interpolated values, `echo` / `print` operands and `(string)` operands must be `Stringable`: `string`, ints and floats (format: see Casts and conversions) are; a union is when every member is (`int|string` prints whichever value it holds). Anything else, including `bool` and `?T` (`null` is not `Stringable`), is a compile error. `bool` has no string conversion at all (not via `echo`, interpolation or `(string)`); the diagnostic's fix-it is `$b ? 'true' : 'false'`.

## Collections and values

PHP's hybrid array is split into distinct types:

| Type | Replaces | Notes |
| --- | --- | --- |
| `list<T>` | Sequential arrays | No holes; `[1, 2, 3]` literal |
| `map<K, V>` | Associative arrays | No key coercion; `K` must be `Hashable` |
| `set<T>` | `array_flip` / `array_unique` hacks | `T` must be `Hashable` |
| `tuple<A, B, …>` | `[$a, $b] = fn()` returns | Fixed arity |
| `struct` | Shape arrays, DTOs | See Structs and classes |

Literal inference:

| Literal | No context | With context |
| --- | --- | --- |
| `[]` | Compile error: annotate | As declared: `$x: list<int> = []` |
| `[1, 2]` | `list<int>` | `list<T>`, `set<T>` or `tuple<…>` |
| `['a' => 1]` | `map<string, int>` | `map<K, V>` |
| `[1, 'a']` | Compile error: mixed element types | `list<int\|string>` if annotated |

`set<T>` has no literal of its own; a list literal initializes it from context: `$s: set<int> = [1, 2];`

Access:

- `$list[10]` past the end traps (`IndexError`). `$map[$k]` on a missing key traps (`KeyError`).
- Tuple elements: `$t[0]`, `$t[1]`, … with an integer literal index, typed per element (`$pair[0]: K` for `tuple<K, V>`). A non-literal or out-of-range index is a compile error (`tuple-index`). Writes follow Nested mutation: `$pair[1] = 5;`.
- `$map->get($k)` returns `?V`. `$map[$k] ?? $default` desugars to `get()` and never traps.
- Destructuring: `[$a, $b] = $tuple;` and `foreach ($pairs as [$k, $v])`. `foreach` over a map yields `$k => $v`; over a list, `$i => $v` gives the position `$i: int` (0 to `count() - 1`).

Value semantics:

- `string`, `bytes`, `list`, `map`, `set`, `tuple`, `struct` and enums are value types with copy-on-write. Classes are reference types.
- Copy-on-write mutates in place when the refcount is 1, so the JIT and AOT backends elide most copies.
- PHP's `&` is removed. A function mutates a caller's value only through `inout`, which is parameter-only and explicit at the call site:

```php
function addItem(inout $items: list<int>, $v: int): void
{
    $items->push($v);
}

addItem(inout $list, 5);
```

Mutability:

- Variables are mutable by default.
- `const`: compile-time constant, at class, namespace or local level.
- `readonly`: assign-once, for locals and properties. Deep, because of value semantics.

### Nested mutation

A write through a value path happens in place: `$order->lines[2]->qty += 1;` walks the path from the root, makes each value-typed level unique (keep it if refcount is 1, else copy that level only), then mutates the leaf. Unshared paths cost zero copies.

| Expression | Writable? |
| --- | --- |
| `$x` | Yes |
| `L->prop`, `L` a writable struct | Yes, written through |
| `expr->prop`, `expr` a class instance | Yes, even from a call (`getUser()->name = 'x'`) |
| `L[$k]`, `L` a writable list or map | Yes |
| `L[0]`, `L` a writable tuple (literal index) | Yes |
| `L[] = $v` | Yes (push) |
| `C::$prop`, a static property | Yes; exclusivity checked at runtime |
| Value-typed call result (`getLines()[0]->qty = 1`) | Compile error: mutating a temporary |
| Through `?->` | Compile error |

- `mutating` methods follow the same rule; calling one on a temporary is a compile error.
- `inout` arguments follow the same rule: any writable place in this table is accepted (`inout $p->x`, `inout $list[$i]`, `inout $obj->prop`, `inout Foo::$count`), except the push form `L[]`.
- A `readonly` value-typed level anywhere on the path makes the write a compile error. The path stops at a class reference: `$this->logger->level = 3` is fine even if `$logger` is `readonly`.
- Writing through a hooked property is get → modify → set (one O(n) copy, since the `get` result is shared).
- No autovivification: `$m['a'][] = 1` traps with `KeyError` if `'a'` is missing. Idiom: `$m[$k] ??= []; $m[$k][] = $x;`
- Order: path subexpressions left to right, then the RHS, then the write.

### Exclusivity

A modifying access (`inout` argument, `mutating` call, compound assignment, nested path write) must not overlap any other access to the same storage. Closures capture by value and `&` is gone, so locals cannot be aliased; conflicts on locals are checked at compile time.

| Code | Result |
| --- | --- |
| `swap(inout $a, inout $a)` | Error |
| `f(inout $a, inout $a[0])` | Error: overlapping paths |
| `$a += g(inout $a)` | Error: the `+=` access spans the call |
| `$a = g(inout $a)` | OK: the write starts after the RHS |
| `f(inout $a, $a[0])` | OK: the by-value argument is read first |
| `f(inout $p->x, inout $p->y)` | OK: disjoint struct fields |
| `swap(inout $list[0], inout $list[1])` | Error: use `$list->swap(0, 1)` |

- Class properties and statics are checked at runtime: the location is flagged during a modifying access, and an overlapping access throws `ExclusivityError` (an `Error`). On in every build mode; elided when the compiler proves no alias.
- `foreach` iterates a snapshot, so mutating the collection inside the loop is not a conflict.

### Closures and captured state

- Assigning to a captured variable is a compile error (PHP silently mutates a per-call copy).
- In a struct method, a closure captures a snapshot of `$this` and cannot write it.
- In a class method, a closure captures the reference; `$this` is captured only if used, so no `static fn`.
- Shared mutable state is explicit through stdlib `Ref<T>` (a final class with `$value`; not `Sendable`):

```php
$count: Ref<int> = new Ref(0);
$list->each(fn($x: int): void { $count->value++; });
```

- Closures may take `inout` parameters; the type is spelled `fn(inout int): void`.
- A capture costs a refcount increment, never an O(n) copy unless the original is later mutated.

## Type system

- Statically typed. Function signatures, properties and every local declaration require explicit types: `$count: int = 0;`. There is no local type inference for declarations.
- Assigning to a variable that has not been declared is a compile error (fix-it: add the type). Reassignment needs no type: `$count = 5;`.
- **Scope:** a declaration is visible from its point to the end of its enclosing block (`undeclared-variable` after that). Declaring a name that is still visible is an error (`redeclared-variable`): no shadowing, including closure parameters and pattern bindings. Sibling blocks may each declare the same name. `for`-init, `foreach` and pattern bindings are scoped to their loop body or arm. A local's destructor runs when its block ends.
- Bindings whose type comes from an already-typed source are not annotated: `foreach` variables, `match` and `instanceof` pattern bindings, destructuring, `catch` variables (which state their class) and closure parameters (typed in the signature).
- Pattern bindings (destructuring, `foreach`, `match`, `instanceof`) always declare new variables. A binding whose name is already visible is a compile error (`redeclared-variable`); patterns never assign to existing variables. PHP's `[$a, $b] = [$b, $a]` swap is therefore an error; use a temporary, or `$list->swap()` for list elements.
- **Nullability:** no implicit null. `?T` (sugar for `T|null`) must be explicit. Null-safe `?->` and `??`.
- **Unions:** `int|string`, narrowed by `instanceof`, `match` or flow typing.
- **Intersections:** `A&B`, interfaces only.
- **`never`** for `exit`, throw-only functions and infinite loops. **`void`** for no return.
- **Banned:** `mixed`, `callable`, `iterable`, `object`. Dynamic data uses tagged enums (e.g. `Json\Value`); callables use `fn(...)` types; iteration uses `Iterable<T>`.
- **Conditions must be `bool`.** `if ($count)` and `!$list` are compile errors.
- **Type test:** `$x instanceof Foo`. Null check: `$x == null`. When `$x` has a union type, `instanceof` accepts any member of the union, primitives included: `if ($x instanceof string)` narrows `int|string` to `string`.

Flow typing narrows after `if ($x != null)`, `if ($x instanceof Foo)`, `match` arms, early `return` / `throw`, and `&&` chains. A `while` condition narrows inside the loop body the same way, and a condition on an assignment narrows the assigned variable: in `while (($line = \Io\readLine()) != null)`, `$line` is `string` in the body. It applies to locals and `readonly` properties only; mutable properties can change between check and use.

Generics:

| Aspect | Decision |
| --- | --- |
| Semantics | Reified: `$x instanceof list<int>`, `new T()`, `T::class` work |
| Implementation | Backend's choice: JIT shares code via type dictionaries; AOT monomorphizes |
| Bounds | `<T of Comparable>`; multiple bounds `<T of Comparable & Hashable>` |
| Variance | Declaration-site `in` / `out` (e.g. `Iterable<out T>`); concrete collections invariant |
| Call syntax | Inferred by default; turbofish `foo::<Bar>($x)` fallback. In type contexts `<` always parses as generic |
| Scope | Classes, interfaces, structs, functions, methods |
| Extras | Default type args (`class Cache<K, V = string>`); type aliases (`type Pairs = list<tuple<string, int>>`) |

Conditional methods: a method can add bounds on its class's type parameters with `where`; it exists only when they hold:

```php
public function join($sep: string): string where T of Stringable
```

## Equality and comparison

`===` and `!==` are removed.

| Operator | Meaning | Valid on |
| --- | --- | --- |
| `==` / `!=` | Structural equality | Primitives, structs, collections, tuples, enums, `Equatable` classes; compile error otherwise |
| `is` | Identity | Class instances only; compile error on value types |
| `<=>` | Ordering | Types implementing `Comparable` |

- `Equatable` and `Comparable` are interface-based operator overloading. Their methods, where `self` is the implementing type: `Equatable::equals($other: self): bool`, `Hashable::hash(): int` (`Hashable` extends `Equatable`: equal values hash equally), `Comparable::compareTo($other: self): int` (negative, zero or positive, like `<=>`).
- `Hashable` governs map keys and set members.
- Built-in conformance: integer types, `string` and `bytes` are `Equatable`, `Hashable` and `Comparable` (`string` / `bytes` byte-wise); float types are `Equatable` (IEEE) and `Comparable` (`totalOrder`), not `Hashable`; `bool` is `Equatable` and `Hashable`, not `Comparable`.

Null checks: comparing any `?T` with the `null` literal via `==` / `!=` is always allowed, including non-`Equatable` classes. Comparing a non-nullable `T` with `null` is a compile error (always false).

Auto-derivation (v1, built into the compiler, no generators):

- A struct is automatically `Equatable` when every field is `Equatable`, and automatically `Hashable` when every field is `Hashable`.
- A struct with a float field is auto-`Equatable` (IEEE semantics, NaN caveat applies) but never auto-`Hashable`; wrap the field in `Float\Key` if it must be a key.
- An explicit `implements Equatable` / `Hashable` with its own methods overrides the derived version.

Derivation for other types:

| Type | `Equatable` | `Hashable` | `Comparable` |
| --- | --- | --- | --- |
| Tuple | If all components are | If all components are | Lexicographic, if all components are |
| Pure / backed enum | Always | Always | Opt-in: `implements Comparable` derives declaration order |
| Data-carrying enum | If all payloads are | If all payloads are | Opt-in: case order, then payload |
| `list<T>` | If `T` is | If `T` is | Lexicographic, if `T` is |
| `set<T>`, `map<K, V>` | If elements / values are | Order-independent hash | Never |
| Struct | Derived (above) | Derived (above) | Never automatic |

Floats:

- `==` and `<` follow IEEE 754, bare and inside structural comparison. A struct containing NaN is not equal to itself.
- Floats are not `Hashable`; neither are structs with float fields. `Float\Key` (bitwise, normalized: all NaNs equal, -0.0 equals +0.0) wraps a float for use as a key.
- `<=>` on floats uses IEEE `totalOrder`: sorting is total and deterministic, NaN sorts last. Note `-0.0 <=> 0.0` is -1 while `-0.0 == 0.0` is true.

## Functions

- **Function types:** `fn(int, string): bool`.
- **Closures:** `fn` only, with an expression or block body. Captures by value at creation; no by-reference capture. `function () use (...)` is removed.

```php
$scale: fn(int): int = fn($x: int): int => $x * $factor;
$fmt: fn(User): string = fn($u: User): string {
    return "{$u->name} <{$u->email}>";
};
```

- **First-class callables:** `\Str\normalize(...)`, `$obj->method(...)`, `Foo::bar(...)`.
- **Named arguments:** supported; parameter names are part of the public API.
- **Defaults:** constant expressions plus `new` in initializers.
- **Variadics:** `...$xs: int` arrives as `list<int>`. Spread `...$list` passes a `list<T>` to a variadic `...$xs: T` parameter; spreading into fixed parameters or `...$map` into named arguments is not supported (it could only be checked at runtime).
- **No overloading.** Use defaults, unions and named arguments.
- **Pipe and partial application:** methods chain; the pipe is for free functions.

```php
$out: string = $input->trim()->replace('a', 'b')   // methods: chain
    |> \Str\normalize(?, \Str\Form::NFC)   // free functions: pipe
    |> slugify(...);
```

- **Generators:** `yield`, typed `Generator<TYield, TSend, TReturn>`, which implements `Iterable<TYield>`.

### Evaluation order

- Strictly left to right: the receiver before arguments, arguments in written order (named arguments included).
- `&&`, `||`, `??` short-circuit. `?->` short-circuits the whole chain: `$a?->b()->c()` stops at the first null.
- Assignment: path, RHS, write. Compound assignment: path, read, RHS, write.

## Structs, classes and interfaces

| | `struct` | `class` |
| --- | --- | --- |
| Semantics | Value, copy-on-write | Reference |
| Inheritance | None | Single; final unless `extendable class` |
| Interfaces | Yes | Yes |
| Construction | `new Point(x: 1.0, y: 2.0)` | `new Foo()` |
| `is` identity | Compile error | Yes |
| Destructor | No | `__destruct` |
| Field-changing methods | Must be marked `mutating` | No marker |

```php
struct Point
{
    public function __construct(public $x: f64, public $y: f64) {}

    public mutating function shift($dx: f64): void
    {
        $this->x += $dx;
    }
}
```

- Calling a `mutating` method on a `readonly` or `const` value is a compile error.
- **Implicit constructor:** a class or struct that declares no `__construct` gets a no-argument constructor, valid only when every property has a default. A subclass without its own `__construct` inherits its parent's. There is no implicit memberwise constructor; use constructor promotion.
- **Property initialization** is checked at compile time (`uninitialized-property`):
  1. Every instance property without a default must be assigned on every path through the constructor. A class or struct with no `__construct` and such a property is an error at its declaration.
  2. Until all properties are assigned, `$this` may only be used to assign properties: no method calls, no passing `$this`, no closures over it.
  3. `?T` properties are not implicitly `null`; they need `= null` or an assignment.
  4. When the parent declares a constructor, a subclass constructor must call `parent::__construct(...)` exactly once on every path, before any other use of `$this` (`missing-parent-constructor-call`).
  5. Static properties always have an initializer (evaluated lazily; see Static initialization).
- `abstract` classes are implicitly extendable. No separate record type.
- Methods of an `extendable` or `abstract` class are overridable unless marked `final`. `final` is a method modifier only: on a class, a struct, an interface method, an `abstract` method, or a method of a non-extendable class it is a compile error (redundant or contradictory). Overriding a `final` method is a compile error.
- Kept from PHP: interfaces, abstract classes, constructor promotion, property hooks, asymmetric visibility, statics, readonly classes.
- **Reuse:** interface default methods (no state). No traits.
- **Visibility:** required explicitly on every property, method and constant. Levels: `public`, `protected`, `private`, `internal` (package-only).
- **Magic methods:** `__construct` and `__destruct` kept. `__toString` → `Stringable::toString()`, `__invoke` → `Callable`. `__clone` dropped. `__get`, `__set`, `__call` banned.
- **Statics:** kept. Statics are isolate-local: each thread gets its own copy; only `Sync\*` statics are process-global.

### Default-method conflicts

1. The class wins: a method defined in the class or inherited from a superclass beats any interface default.
2. Two interface defaults with the same signature are a compile error unless the class overrides the method. The override can pick one explicitly: `return parent(Named)::describe();`
3. An interface extending two interfaces with conflicting defaults follows the same rule.

### Late static binding

- `self`, `parent`, `static::` and the `static` return type are kept. `static::` dispatch compiles to a vtable slot, so it is AOT-safe; only `extendable` classes pay for it.
- `new static(...)` is allowed only when the constructor is `final`, or is declared by an interface or abstract class in the hierarchy. A subclass cannot change a constructor that `new static` relies on.
- `new self(...)` builds the class whose body contains it, with no late binding: a subclass that inherits the method still gets the parent class. Abstract classes, interfaces and enums cannot be instantiated this way either.

### clone

- `clone $obj` makes a shallow copy of a class instance: value-typed fields are copied (copy-on-write), reference-typed fields are shared.
- Clone-with uses type-checked named arguments: `clone($user, name: 'Ken')`. This is the main use for `readonly` classes.
- `clone` on a struct is a compile error; assignment already copies.
- No `__clone` hook; deep-copying a reference field is an explicit method.

## Enums and match

Enums are value types with methods, interfaces and constants. Three kinds; an enum is backed or carries data, never both:

```php
enum Suit { case HEARTS; case SPADES; }                  // pure
enum Status: string { case ACTIVE = 'active'; }          // backed (int|string)
enum Shape {                                             // data-carrying
    case CIRCLE($r: f64);
    case RECT($w: f64, $h: f64);
}
```

- Construction is call-like, without `new`, mirroring the pattern: `Shape::CIRCLE(1.5)`, `Shape::RECT(w: 2.0, h: 3.0)` (named arguments allowed); pure and backed cases have no parentheses (`Suit::HEARTS`). A data case without arguments is a compile error (`missing-case-arguments`); it is not a function value.
- Backed API: `from()` traps (`ValueError`), `tryFrom()` returns `?Self`. Pure and backed enums have `cases()`, every case in declaration order; data-carrying enums do not (their cases need payloads).

`match`:

- Exhaustiveness is checked at compile time over enums, unions and `bool`. Anything not provably exhaustive needs `default`. No runtime `UnhandledMatchError`. Arms with an `if` guard never count toward exhaustiveness; only unguarded arms and `default` cover a case.
- Patterns: destructuring of enum data, tuples and structs, with `if` guards. Integer range patterns: `90..=100`, `80..<90`.
- A `match` that begins a statement is a statement, like `if` (it replaces `switch`): arms may be blocks `=> { … }` or expressions, any result is discarded, and no `;` follows the closing `}` (a `;` is tolerated; the formatter removes it). Anywhere else `match` is an expression: arms are expressions, and the enclosing statement ends with its own `;`.
- Type of a `match` expression: all arms have the same type, or the expected type accepts every arm (`int|string`). `never` arms (`throw`, `exit(…)`) fit any type. Without an expected type, differing arm types are a compile error (`type-mismatch`).
- `switch` is removed.

```php
$area: f64 = match ($shape) {
    Shape::CIRCLE($r)               => \Math\PI * $r * $r,
    Shape::RECT($w, $h) if $w == $h => $w * $w,
    Shape::RECT($w, $h)             => $w * $h,
};
```

Pattern meaning:

- A bare `$var` in a pattern binds. Literals, constants and enum cases compare directly.
- A typed binding `$v: T` tests that the value is a `T` and binds `$v` narrowed to `T`; `_: T` tests without binding. Typed bindings are not bare bindings (no guard needed) and work in any pattern position, including enum payloads and list patterns. For exhaustiveness, each member of a union (and `null` in `?T`) must be covered (example below).
- Comparison against any expression uses the `== expr` pattern.
- A top-level bare binding with no guard is a compile error (fix-it: `== $var` or `default`). This catches PHP's `match ($x) { $limit => … }`, which compares.

```php
$label: string = match ($opt) {      // ?int
    null    => 'none',
    $v: int => "value {$v}",
};
$size: int = match ($x) {            // int|string
    $n: int    => $n,
    $s: string => $s->byteLength(),
};
```

```php
match ($x) {
    0              => 'zero',
    == $limit      => 'at limit',
    == maxFor($u)  => 'at max',
    $n if $n > 100 => 'big',
    default        => 'other',
}
```

Subjectless `match` replaces PHP's `match (true)` idiom. Each arm is a `bool` condition and `default` is required; `match (true)` with condition arms is a compile error with a fix-it:

```php
$group: string = match {
    $age < 13 => 'child',
    $age < 20 => 'teen',
    default   => 'adult',
};
```

### Ranges

`..=` (inclusive) and `..<` (half-open) build a `Range<T>` value for integer types and `u32` codepoints. There is no bare `..`; a digit followed by `..` always lexes as an int plus a range operator.

```php
foreach (0..<10 as $i) { … }
$mid: list<int> = $list->slice(1..<3);
if ((1..=12)->contains($month)) { … }
```

A range whose start is past its end (`5..=1`, `0..=-1`) is empty: `count()` 0, iterates nothing. Count down with `(1..=5)->reversed()`. `slice()` with an empty range returns an empty value; bounds outside the collection still trap (`IndexError`, or `BoundaryError` off a string character boundary).

Range patterns in `match` are integer-only.

### Destructuring outside match

Irrefutable patterns (tuples, structs) work in assignment and `foreach`:

```php
Point { x: $x, y: $y } = $p;
foreach ($points as Point { x: $x }) { … }
[$a, [$b, $c]] = $nested;
```

Refutable patterns (enum cases) use `instanceof`; bindings are flow-typed inside the branch:

```php
if ($v instanceof \Json\Value::STRING($s)) {
    echo $s;
}
```

Parameters cannot be destructured; parameter names are public API through named arguments.

## Errors

Exceptions are unchecked. User classes extend `Error` or `Exception`; they cannot implement `Throwable` directly. The specific built-in throwables (`IndexError`, `ParseException`, …) are final. `Error` and `Exception` are concrete: `throw new Exception('failed');` is valid. The whole hierarchy is in the prelude, so `IndexError` and `\IndexError` name the same class.

```
Throwable
├── Error       programming faults: OverflowError, CastError, DivisionByZeroError,
│               IndexError, KeyError, BoundaryError, AssertionError,
│               ExclusivityError, StaticInitError, ValueError (argument outside its valid domain)
└── Exception   expected failures: IoException, ParseException, EncodingException, …
```

- Throwable API (PHP's shape): `__construct($message: string = '', $code: int = 0, $previous: ?Throwable = null)`; `final` accessors `getMessage(): string`, `getCode(): int`, `getPrevious(): ?Throwable`, `getFile(): string`, `getLine(): int`. `getTrace()` / `getTraceAsString()` are post-POC. Subclass constructors call `parent::__construct(...)` (see Property initialization).
- `Error`s are catchable, so servers and fiber schedulers can isolate a failure. Only out-of-memory, stack overflow and fatal destructor errors (see Destructors) end the process without being catchable.
- Kept from PHP 8: `finally`, multi-catch `catch ($e: A | B)`, catch without a variable, `throw` as an expression, `previous` chaining.
- `return`, `break` and `continue` written directly in a `finally` body, leaving it, are compile errors (`control-leaves-finally`). `break` / `continue` of a loop nested inside the `finally` body are fine. `exit(…)` and `throw` remain allowed.
- Anything that still leaves a `finally` while a throwable is pending (a `throw`, an `exit`, or either reached through a call) wins, and the pending throwable is never dropped silently: its trace is printed to stderr marked "discarded by throw / exit(n) at file:line". A new throwable that has no `previous` also gets the discarded one as `previous`.
- `assert()` is always on. `debugAssert()` is stripped from release builds.
- An uncaught throwable in a fiber is rethrown to whoever awaits or joins it. If none, the scheduler logs it and exits non-zero.

## Modules and packages

Modules:

- The namespace is the directory. File names are free; a file may hold any number of declarations.
- Each file keeps a `namespace` line, which the compiler requires to match the directory relative to the package root. Without a manifest there is no root, so there is no path check: single-file mode accepts any non-reserved namespace, and directory mode requires every file in the directory to declare the same namespace (`namespace-mismatch`).
- Imports use PHP syntax: `use A\B;`, `use A\{B, C};`, `use function Str\normalize;`, `use const Math\PI;`, `use A\B as C;`.
- No global fallback. Fully qualified names always work without `use`. A prelude auto-imports the names listed under Lexical structure, and nothing else.
- Name resolution follows PHP. A leading `\` makes a name fully qualified: `\Env\get()`. A qualified name without it is relative to the current namespace: inside `namespace App;`, `Env\get()` means `App\Env\get()`. Stdlib calls are therefore written `\Env\get()`, `\Io\readLine()`, or imported with `use`. Names in `use` declarations are always fully qualified and take no leading `\`.
- Prose in this spec names stdlib items without the leading `\` (`Env\get()`); code examples include it.
- Stdlib root namespaces (`Io`, `Env`, `Math`, `Str`, `Process`, `Test`, `Task`, …: every top-level namespace the stdlib defines) are reserved. A user file declaring one, or a namespace under one (`namespace Math\Extra;`), is a compile error. The package registry rejects vendor names that map onto a stdlib root.
- Circular imports are allowed; the compiler resolves the whole package.
- No `include`, `require` or autoloader; resolution is static.

Packages:

| Aspect | Decision |
| --- | --- |
| Manifest | `scb.toml`; maps vendor name (`ellisgl/foo`) to root namespace (`Ellisgl\Foo`) under `src/` |
| Lockfile | `scb.lock` with checksums, committed for apps and libraries |
| Constraints | Semver, Composer syntax (`^1.2`, `~1.2.3`, `>=1.0 <2.0`) |
| Resolution | Single version per package |
| Sources | Central registry, git, local path |
| Publishing | Versions immutable; yank only |
| Install scripts | None |
| Dependency kinds | Regular and `dev` |

- **No optional feature flags.** Optional functionality ships as split packages (`ellisgl/orm`, `ellisgl/orm-postgres`), each its own namespace. A `[suggest]` table in `scb.toml` lists companion packages.
- **Target-conditional code only:** `#[Cfg(os: 'linux')]`, `#[Cfg(arch: 'x86_64')]`, `#[Cfg(mode: 'release')]`. `#[Cfg]` never takes consumer-selected features.

Toolchain: one `scb` binary with `run`, `build`, `fmt`, `test`, `add`, `publish`.

Testing: `scb test` runs `#[Test]` functions in `tests/`; assertions come from `Test\assertEquals()` and friends.

## Runtime

- **Memory:** refcounting with a cycle collector. Refcounting also drives copy-on-write and deterministic destructors.
- **Concurrency:** fibers with a scheduler in the stdlib; no function coloring (no async/await). Threads in v2 as isolated workers (see Threads).
- **Attributes:** `#[...]`, compile-time metadata.
- **Reflection:** read-only (types, properties, attributes, enum cases). No invoking by string name, so AOT stays possible.

### Destructors

| Case | Rule |
| --- | --- |
| Timing | Deterministic: when the refcount hits 0 (scope exit, overwrite, `remove`), before the next statement runs. A value not held by any variable (a temporary) is destructed at the end of the statement that released it, never in the middle of an expression |
| Order at scope exit | Reverse declaration order |
| Fields | The object's `__destruct` runs first, then fields are released in declaration order |
| Cycles | The collector runs destructors in unspecified order; all run before any memory is freed |
| Throwing from `__destruct` | Fatal: trace to stderr, exit code 1, no further unwinding |
| `exit` inside `__destruct` | Fatal, same as throwing: stderr names the destructor and the `exit` call site |
| Program end | On a normal return from `main`, `Process\exit` and an uncaught throwable: locals are destructed as their scopes unwind, then statics in reverse initialization order. After an uncaught throwable, its trace is printed before the statics are destructed. Not after `Process\abort` or a fatal error |
| `Process\exit` | Unwinds; locals destructed; statics destructed in reverse initialization order |
| Static read after release | A destructor that runs at program end and reads a static already released throws `StaticInitError` (fatal, as it escapes a destructor) |
| `Process\abort` | No destructors run |

### Static initialization

- Statics are initialized lazily on first access, per isolate. The initializer can be any expression.
- If it throws, the exception reaches the first accessor and the static is poisoned: later accesses throw `StaticInitError` with the original as `previous`.
- Cyclic initialization is detected at runtime and throws `StaticInitError`.
- `const` values are compile-time and unaffected.

### Tasks (fiber scheduler API)

The user-facing namespace is `Task\`; fibers are the mechanism. Concurrency is structured: a scope returns only after every child task finishes.

```php
$users: list<User> = \Task\scope(fn($s: \Task\Scope): list<User> {
    $a: \Task\Handle<User> = $s->spawn(fn(): User => $repo->find(1));
    $b: \Task\Handle<User> = $s->spawn(fn(): User => $repo->find(2));
    return [$a->await(), $b->await()];
});
```

| Area | API |
| --- | --- |
| Spawn | `$scope->spawn(fn): Task\Handle<T>`; escape hatch `Task\detach(fn)` |
| Await | `$task->await(): T`; `Task\all(list<Task\Handle<T>>): list<T>`; `Task\race(...)` cancels the losers |
| Cancel | Cooperative `$task->cancel()` delivers `CancelledError` at the next suspension point; a failing child cancels its siblings |
| Timeout | `Task\timeout(Time\Duration::seconds(5), fn)` throws `TimeoutError` |
| Channels | `new Task\Channel<T>(capacity: 0)` (0 = unbuffered); `send()`, `receive(): ?T` (null = closed and drained), `close()`; `foreach` until closed |
| Sync | `Task\Mutex`, `Task\Semaphore` |
| Yield / sleep | `Task\yield()`; `Task\sleep(Time\Duration::ms(100))` |

- All stdlib I/O (`Io`, `Net`, `Http`, `Process`) is fiber-aware: it blocks the fiber, not the thread. The event loop (epoll / kqueue / IOCP) is internal.
- `Time\Duration` is a struct; no bare integers for time.

**Select** waits on several channel operations; exactly one commits. It is a library call, not syntax, and an expression of type `T`:

```php
$action: Action = \Task\select(
    $jobs->onReceive(fn($job: ?Job): Action => handle($job)),
    $quit->onReceive(fn($_: ?bool): Action => Action::STOP),
    $out->onSend($r, fn(): Action => Action::SENT),
    \Task\onTimeout(\Time\Duration::seconds(1), fn(): Action => Action::TIMEOUT),
);
```

- Atomic commit: only the chosen operation takes effect, so no message is lost (unlike `Task\race` over receives).
- Fairness: uniform random choice among ready operations.
- A receive on a closed, drained channel fires with `null`.
- `default:` named argument gives the non-blocking form.
- Suspends the fiber, not the thread; works on thread-crossing channels.
- A Go-style `select` keyword may come later as sugar over this API (`Task\select`).

### Threads (v2; model decided)

Threads are isolated workers with message passing: each thread owns its heap and runs its own fiber scheduler. Refcounts stay non-atomic except on shared immutable objects.

| Kind | Crosses threads? | How |
| --- | --- | --- |
| Value types (structs, collections, enums, tuples) with `Sendable` contents | Yes | Copied; moved for free when refcount is 1 |
| `readonly class` with `Sendable` fields | Yes | Shared, immutable; atomic refcount |
| Mutable class instances | No | Compile error |
| Shared mutable state | Only via stdlib | `Sync\Mutex<T>`, `Sync\Atomic<T>` |

```php
$h: \Thread\Handle<Summary> = \Thread\spawn(fn($jobs: \Task\Channel<Job>): Summary { … });
$summary: Summary = $h->join();                       // Summary must be Sendable

$hash: string = \Thread\run(fn(): string => \Crypto\argon2($pw)); // offload CPU work; awaitable from a fiber
$pool: \Thread\Pool = new \Thread\Pool(workers: 8);
```

- `Task\Channel<T>` is thread-safe (the same type serves fibers and threads); `T` must be `Sendable` when it crosses threads.
- `Thread\run` is also how a future FFI `#[Blocking]` call avoids stalling the scheduler.
- Statics are isolate-local: each thread gets its own lazily initialized copy; fibers within a thread share it. Process-wide state is explicit: `private static $hits: \Sync\Atomic<i64> = new \Sync\Atomic(0);`

## Templates (.scbt)

Templates compile to ordinary SCB functions with typed parameters. Delimiters follow Twig; expressions inside are SCB.

```
{% params $title: string, $users: list<User> %}
{% use App\Models\User %}

{% call Layout\main(title: $title) %}
    <h1>{{ $title->toUpper() }}</h1>
    {% foreach ($users as $user) %}
        {% if $user->active %}<p>{{ $user->name }}</p>{% endif %}
    {% endforeach %}
{% endcall %}
```

- **Naming:** follows the module rules. `src/Templates/Users/Index.scbt` becomes `App\Templates\Users\index()`, called like any function with named arguments. A template whose function name would be a prelude name (`List.scbt` → `list()`) is a compile error.
- **No filter language:** use methods, `|>` and SCB functions; `|` stays reserved for unions and bitwise OR.
- **Escaping:** context-aware, on by default. The compiler parses the HTML and picks HTML, attribute, JS, CSS or URL escaping per insertion point.
- **Raw output:** only `Html\Safe` values skip escaping, produced by `Html\raw($s)`. No `|raw` filter.
- **Composition:** components only, via `{% call %}`; the body is passed as `Html\Safe $content`. No `extends` / `block` inheritance.
- **Logic:** `if`, `foreach`, `match`, function calls, local `{% let $x: T = … %}`. No mutation of outside state.
- **Output:** writes to an `Io\Writer`; a wrapper returns `string`.
- **Whitespace control:** `{%- -%}` and `{{- -}}`.

## Core library (POC minimum)

Conventions:

1. Methods on built-in types are primary; the receiver is the subject. Namespaces (`Str\`, `Math\`, `Io\`) hold only functions with no natural subject. No duplicates such as `Str\replace($s, …)` beside `$s->replace(…)`.
2. A plain verb mutates in place and returns `void` (`sort()`, `reverse()`); the past participle returns a new value (`sorted()`, `reversed()`). `map` and `filter` always return new values.
3. `map` and `set` preserve insertion order.
4. Conditional methods use `where` bounds (e.g. `join` only when `T of Stringable`).
5. Map callbacks receive `($value, $key)`.

`list<T>`:

| Group | Methods |
| --- | --- |
| Size | `count(): int`, `isEmpty(): bool` |
| Read | `get($i): ?T`, `first(): ?T`, `last(): ?T`, `contains($v)`, `indexOf($v): ?int`, `slice($r: Range<int>)` |
| Mutate | `push($v)`, `pop(): ?T`, `insert($i, $v)`, `removeAt($i): T`, `swap($i, $j)`, `clear()`, `sort()`, `sortBy($f)` (comparator `fn(T, T): int`, like `<=>`), `reverse()` |
| New value | `concat($other)`, `sorted()`, `sortedBy($f)` (comparator), `reversed()`, `map($f)`, `filter($f)`, `reduce($init, $f)`, `chunk($size)` |
| Query | `any($f)`, `all($f)`, `find($f): ?T`, `findIndex($f): ?int`, `each($f)` |
| Conditional | `join($sep)` (Stringable), `sum(): T` (numeric; `0` / `0.0` when empty; overflow traps), `min(): ?T`, `max(): ?T` (numeric or Comparable; `null` when empty) |

`map<K, V>`:

| Group | Methods |
| --- | --- |
| Size | `count()`, `isEmpty()` |
| Read | `has($k)`, `get($k): ?V`, `keys(): list<K>`, `values(): list<V>`, `entries(): list<tuple<K, V>>` |
| Mutate | `$m[$k] = $v`, `remove($k): ?V`, `clear()` |
| New value | `merge($other)` (right wins, never renumbers), `filter($f)`, `mapValues($f)` |
| Query | `each($f)`, `any($f)`, `all($f)` |

`set<T>`: `add($v)`, `remove($v)`, `has($v)`, `count()`, `isEmpty()`, `values(): list<T>`; `union($o)`, `intersect($o)`, `diff($o)` return new sets.

`string`:

| Group | Methods |
| --- | --- |
| Views and lengths | `bytes()`, `codepoints()`, `graphemes()`, `byteLength()`, `codepointLength()`, `graphemeLength()` |
| Search | `contains($needle)`, `startsWith($p)`, `endsWith($s)`, `find($needle, $offset = 0): ?int` (byte offset) |
| Transform | `replace($needle, $replacement)`, `slice($r: Range<int>)`, `split($sep, $limit = -1): list<string>`, `trim()`, `trimStart()`, `trimEnd()`, `toUpper()`, `toLower()` (Unicode, locale-independent), `repeat($n)`, `padStart($len, $pad = ' ')`, `padEnd($len, $pad = ' ')` |
| Check | `isEmpty()` |

String edge cases:

- `padStart($len, $pad = ' ')` / `padEnd`: `$len` counts graphemes; a multi-character `$pad` repeats and is cut at a grapheme; an empty `$pad` traps (`ValueError`).
- `split($sep, $limit = -1)`: `$limit` is the maximum number of parts, the last keeping the rest; `-1` means no limit; `0` or below `-1` traps (`ValueError`). An empty `$sep` traps (`ValueError`); use `graphemes()`.
- `trim()` / `trimStart()` / `trimEnd()` strip Unicode `White_Space`. No character-list argument in the POC.
- `repeat($n)`: `$n < 0` traps (`ValueError`); `0` gives `''`.
- `replace($needle, $replacement)`: an empty `$needle` traps (`ValueError`), like an empty `split()` separator.
- `find($needle, $offset = 0)`: `$offset` past the end traps (`IndexError`), off a character boundary traps (`BoundaryError`); an empty needle matches at `$offset`.

`bytes` (a value type with copy-on-write; not `Stringable`):

| Group | Methods |
| --- | --- |
| Size | `count(): int`, `isEmpty(): bool` |
| Access | `$b[$i]: u8` read and write (O(1); out of range traps `IndexError`), `slice($r: Range<int>): bytes`, `foreach ($b as $byte)` yields `u8` |
| New value | `concat($other: bytes): bytes` (`.` is strings-only) |
| Decode | `decodeUtf8(): string` (throws `EncodingException`), `tryDecodeUtf8(): ?string` |
| Compare | `==` / `!=` byte-wise; `Hashable` |

`Range<T>`: `contains($v)`, `count()`, `toList()`, `reversed()`, `step($n)` (`$n ≤ 0` traps `ValueError`; count down with `reversed()->step($n)`).

`Io`: `Io\print($s: string)` (string only; `echo` / `print` add the `(string)`), `Io\eprint($s: string)`, `Io\readLine(): ?string`. `readLine()` returns the line without its terminator (`\n` or `\r\n`; a lone `\r` is kept), `null` at end of input, and a final unterminated line as-is. Formatting (`printf`-style) is deferred; interpolation covers the POC.

Request I/O (POC): `Env\get($name): ?string` and `Io\readAll(): bytes`. With `echo`, these form a CGI-style request contract, so the same handler runs unchanged under CGI and FastCGI.

Text from the OS must be valid UTF-8 to become a `string`. `Io\readLine()` and `Env\get()` throw `EncodingException` on invalid input; invalid command-line arguments are reported before `main` runs, like an uncaught `EncodingException` (trace naming the argument, exit 1). Raw data stays available as `bytes` through `Io\readAll()`.

## Grammar sketch

EBNF, informal. `{ x }` = zero or more, `[ x ]` = optional. Lexical details are in Lexical structure. Templates have their own grammar.

```ebnf
(* ---------- Files ---------- *)
File          = NamespaceDecl { UseDecl } { { Attribute } Declaration } ;
NamespaceDecl = "namespace" QualifiedName ";" ;
UseDecl       = "use" [ "function" | "const" ] UseClause ";" ;
UseClause     = QualifiedName [ "as" Ident ]
              | QualifiedName "\" "{" UseItem { "," UseItem } [ "," ] "}" ;
UseItem       = Ident [ "as" Ident ] ;
QualifiedName = [ "\" ] Ident { "\" Ident } ;
Attribute     = "#[" NamedType [ Args ] { "," NamedType [ Args ] } "]" ;

(* ---------- Declarations (types are always postfix) ---------- *)
Declaration   = FunctionDecl | ClassDecl | StructDecl | InterfaceDecl
              | EnumDecl | ConstDecl | TypeAlias ;
TypeAlias     = "type" Ident [ TypeParams ] "=" Type ";" ;
ConstDecl     = "const" Ident ":" Type "=" Expr ";" ;
FunctionDecl  = "function" Ident [ TypeParams ] "(" [ Params ] ")" ":" Type Block ;
TypeParams    = "<" TypeParam { "," TypeParam } ">" ;
TypeParam     = [ "in" | "out" ] Ident [ "of" Type ] [ "=" Type ] ;
Params        = Param { "," Param } [ "," ] ;
Param         = { Attribute } [ Visibility [ SetVis ] [ "readonly" ] ] [ "inout" ]
                [ "..." ] Variable ":" Type [ "=" Expr ] [ Hooks ] ;

ClassDecl     = [ "abstract" | "extendable" ] [ "readonly" ] "class" Ident [ TypeParams ]
                [ "extends" NamedType ] [ "implements" TypeList ] Body ;
StructDecl    = [ "readonly" ] "struct" Ident [ TypeParams ] [ "implements" TypeList ] Body ;
InterfaceDecl = "interface" Ident [ TypeParams ] [ "extends" TypeList ] Body ;
EnumDecl      = "enum" Ident [ TypeParams ] [ ":" ( "int" | "string" ) ]
                [ "implements" TypeList ] "{" { EnumCase | Member } "}" ;
EnumCase      = "case" Ident [ "(" Params ")" | "=" Expr ] ";" ;
Body          = "{" { { Attribute } Member } "}" ;
Member        = Property | Method | ClassConst ;
Visibility    = "public" | "protected" | "private" | "internal" ;
SetVis        = Visibility "(" "set" ")" ;
Property      = Visibility [ SetVis ] [ "static" ] [ "readonly" ] Variable ":" Type
                [ "=" Expr ] ( ";" | Hooks ) ;
Hooks         = "{" { ( "get" | "set" [ "(" Variable ":" Type ")" ] ) ( "=>" Expr ";" | Block ) } "}" ;
ClassConst    = Visibility "const" Ident ":" Type "=" Expr ";" ;
Method        = [ "abstract" | "final" ] Visibility [ "static" ] [ "mutating" ]
                "function" Ident [ TypeParams ] "(" [ Params ] ")" [ ":" Type ]
                [ "where" Ident "of" Type { "," Ident "of" Type } ]
                ( Block | ";" ) ;

(* ---------- Types ---------- *)
Type          = Intersection { "|" Intersection } ;
Intersection  = PrimaryType { "&" PrimaryType } ;
PrimaryType   = "?" PrimaryType | NamedType | FnType | "(" Type ")"
              | "void" | "never" | "null" ;
NamedType     = QualifiedName [ "<" TypeList ">" ] ;
TypeList      = Type { "," Type } ;
FnType        = "fn" "(" [ [ "inout" ] Type { "," [ "inout" ] Type } ] ")" ":" Type ;

(* ---------- Statements ---------- *)
Block         = "{" { Statement } "}" ;
Statement     = Block | LocalDecl | If | While | DoWhile | For | Foreach | Try | MatchStmt
              | "echo" Expr { "," Expr } ";" | "print" Expr ";"
              | ( "exit" | "die" ) ";"   (* bare form: statement only; exit(…) is an expression *)
              | "return" [ Expr ] ";" | "break" [ IntLit ] ";" | "continue" [ IntLit ] ";"
              | Expr ";" ;
LocalDecl     = [ "readonly" ] Variable ":" Type "=" Expr ";"
              | "const" Ident ":" Type "=" Expr ";"
              | ( ListPattern | StructPattern ) "=" Expr ";" ;
If            = "if" "(" Expr ")" Block { "elseif" "(" Expr ")" Block } [ "else" Block ] ;
While         = "while" "(" Expr ")" Block ;
DoWhile       = "do" Block "while" "(" Expr ")" ";" ;
For           = "for" "(" [ ForInit ] ";" [ Expr ] ";" [ ExprList ] ")" Block ;
ForInit       = Variable ":" Type "=" Expr | ExprList ;   (* a typed declaration is scoped to the loop *)
ExprList      = Expr { "," Expr } ;
Foreach       = "foreach" "(" Expr "as" ( Variable [ "=>" Variable ] | ListPattern | StructPattern ) ")" Block ;
Try           = "try" Block { Catch } [ "finally" Block ] ;
MatchStmt     = Match [ ";" ] ;   (* match at statement start; arms may be blocks *)
Catch         = "catch" "(" ( Variable ":" CatchTypes | CatchTypes ) ")" Block ;
CatchTypes    = NamedType { "|" NamedType } ;

(* ---------- Expressions (precedence in the table below) ---------- *)
Primary       = Literal | Variable | QualifiedName | "(" Expr ")" | ListLit | MapLit
              | "self" | "static" | "parent" | "parent" "(" NamedType ")"
              | "clone" Unary | "clone" Args
              | "new" ( NamedType | "self" ) Args | Closure | Match | Exit ;   (* new self: the enclosing class, no late binding *)
Postfix       = Primary { "->" Name | "?->" Name | "::" Name | "::" Variable | "[" Expr "]"
              | "::<" TypeList ">" | Args } ;
Args          = "(" [ Arg { "," Arg } [ "," ] ] ")" ;
Name          = Ident | Keyword ;   (* keywords allowed as labels and member names *)
Arg           = [ Name ":" ] ( Expr | "..." Expr | "?" ) | "inout" Postfix | "..." ;   (* inout: a writable place, see Nested mutation *)
Closure       = "fn" "(" [ Params ] ")" ":" Type ( "=>" Expr | Block ) ;
ListLit       = "[" [ Expr { "," Expr } [ "," ] ] "]" ;
MapLit        = "[" Expr "=>" Expr { "," Expr "=>" Expr } [ "," ] "]" ;
Unary         = ( "-" | "+" | "~" | "!" | "++" | "--" ) Unary | Cast | Postfix [ "++" | "--" ] ;
Cast          = "(" PrimitiveType ")" Unary ;   (* any other parenthesized form is an expression; "(bool)" before an operand is diagnosed as bool-cast *)
PrimitiveType = "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64"
              | "f32" | "f64" | "int" | "float" | "string" ;
Range         = Expr ( "..=" | "..<" ) Expr ;
InstanceOf    = Expr "instanceof" ( NamedType | Pattern ) ;
Exit          = ( "exit" | "die" ) "(" [ Expr ] ")" ;   (* type never; call-like primary *)
Ternary       = Expr "?" Expr ":" Expr ;   (* condition must be bool; no unparenthesized nesting *)

Match         = "match" [ "(" Expr ")" ] "{" Arm { "," Arm } [ "," ] "}" ;   (* subjectless: arms are bool conditions *)
Arm           = ( Pattern { "," Pattern } | "default" ) [ "if" Expr ] "=>" ( Expr | Block ) ;
Pattern       = [ "-" ] NumLit | Literal | Variable | "_" | ( Variable | "_" ) ":" Type   (* typed binding / type test *)
              | "==" Expr | [ "-" ] IntLit ( "..=" | "..<" ) [ "-" ] IntLit
              | ( QualifiedName | "self" ) [ "::" Name ] [ "(" Pattern { "," Pattern } ")" ]   (* constant, enum case, type *)
              | StructPattern
              | ListPattern ;
ListPattern   = "[" Pattern { "," Pattern } "]" ;
StructPattern = NamedType "{" Ident ":" Pattern { "," Ident ":" Pattern } "}" ;

Literal       = IntLit | FloatLit | StringLit | "true" | "false" | "null" ;
NumLit        = IntLit | FloatLit ;
IntLit        = Decimal | "0x" Hex | "0o" Octal | "0b" Binary ;   (* "_" separators; no legacy 0123 octal *)
```

Operator precedence, highest first:

| Level | Operators | Assoc |
| --- | --- | --- |
| 1 | `new`, `->`, `?->`, `::`, `[]`, call | left |
| 2 | `**` | right |
| 3 | unary `-` `+` `~`, `++` `--` (ints only), casts | right |
| 4 | `instanceof` | none |
| 5 | `!` | right |
| 6 | `*` `/` `~/` `%` | left |
| 7 | `+` `-` | left |
| 8 | `<<` `>>` | left |
| 9 | `.` | left |
| 9.5 | `..=` `..<` | none |
| 10 | `\|>` | left |
| 11 | `<` `<=` `>` `>=` | none |
| 12 | `==` `!=` `<=>` `is` | none |
| 13 | `&` | left |
| 14 | `^` | left |
| 15 | `\|` | left |
| 16 | `&&` | left |
| 17 | `\|\|` | left |
| 18 | `??` | right |
| 19 | `? :` (nesting needs parentheses) | none |
| 20 | `=` and compound assignments (`~/=` included) | right |
| 21 | `yield`, `throw` | right |

Notes:

- Braces are mandatory on every control-flow body; no alternative `if: … endif;` syntax.
- `__construct` and `__destruct` omit the return type; every other function, method and closure declares one.
- `and`, `or`, `xor` keyword operators are removed (use `&&`, `||`, `^`).
- Ternary: the condition must be `bool`; both branches must have the same type, or the expected type must accept both (`int|string`). The short ternary `?:` is removed (it tests truthiness); use `??` for null defaults (fix-it offered when the left side is `?T`).
- `for` init holds either expressions or one typed declaration: `for ($i: int = 0; $i < 10; $i++)`. The declared variable is scoped to the loop.
- `|>` placement matches PHP 8.5: looser than arithmetic, tighter than comparison, `??` and ternary.

## Removed from PHP

| Removed | Replacement |
| --- | --- |
| `<?php` / `?>` tags, inline HTML | Code-only files; `.scbt` templates |
| `===`, `!==` | `==` (structural), `is` (identity) |
| Hybrid `array` | `list`, `map`, `set`, `tuple`, `struct` |
| `&` references | `inout` parameters |
| `function () use (...)` | `fn` closures |
| `switch` | `match` |
| Traits | Interface default methods |
| `mixed`, `callable`, `iterable`, `object` | Tagged enums, `fn(...)`, `Iterable<T>` |
| `__toString`, `__invoke`, `__clone` | `Stringable`, `Callable`, value types |
| `#` comments | `//` |
| Global function fallback | Fully qualified names, `use` |

Banned outright (dynamic features that block AOT):

- `eval`
- variable variables
- `$obj->$name`, `new $className`, `$class::method()` via string
- `__get`, `__set`, `__call`
- runtime `include` / `require`
- `compact()`, `extract()`
- `global`, `$GLOBALS` (use statics)
- `goto`

Syntax removals:

- `and`, `or`, `xor` (bind below `=`); use `&&`, `||`, `^`
- Braceless control-flow bodies
- Alternative syntax `if: … endif;` (templates live in `.scbt`)
- Legacy `0123` octal; only explicit `0o123`
- `++` / `--` on strings; ints only

Also removed:

- `isset`, `empty`, `unset` (see Built-in statements)
- Superglobals (`$_GET`, `$_POST`, `$_SERVER`, …)
- Case-insensitive function, class and keyword names
- Non-ASCII identifiers
- `.5` float literals; octal string escapes; unknown escapes kept literally
- `exit("message")` printing and exiting 0
- Short ternary `?:` (truthiness); use `??` or a full ternary
- `print` returning 1
- Autovivification of missing nested map keys

## Implementation plan

| Decision | Choice |
| --- | --- |
| Host language | Rust |
| Parser | Hand-written recursive descent with Pratt operator parsing |
| POC execution | Typed AST interpreter or simple bytecode VM; no JIT |
| Shared IR backend (later) | Cranelift for both JIT and AOT object files; LLVM optional for peak AOT |
| Value representation | Unboxed primitives from day one; heap values carry a refcount header |
| Cycle collector | PHP-style synchronous cycle collection (Bacon–Rajan) |
| Fibers | Stackful (stack switching); choose the context-switch mechanism early, it constrains the JIT |
| Exceptions | Explicit propagation in the interpreter; table-based unwinding with Cranelift |
| Conformance tests | `.scb` files with expected output and expected compile errors, from day one |

POC scope:

- **In:**
  - Types: primitives, `string`, `bytes` (see Core library), `list<T>`, `map<K, V>`, `set<T>`, `tuple<…>`, `Range<T>`; `?T` and general unions with flow typing.
  - Functions: named arguments, defaults, variadics and spread; `fn` closures; first-class callables (`foo(...)`, `$obj->m(...)`).
  - Structs with `mutating`, copy-on-write, nested mutation, `inout`.
  - Classes: single inheritance, `extendable`, `abstract`, `final` methods, constructor promotion, `readonly` (locals, properties, `readonly class`), static properties with lazy initialization and poisoning, runtime exclusivity checks, `clone` and clone-with, `__destruct`, cycle collector. `self::` and `parent::`.
  - Interfaces with default methods; user `Equatable`, `Comparable`, `Stringable`.
  - Pure, backed and data-carrying enums; `match` with patterns, guards, ranges and subjectless form; `instanceof` patterns; destructuring.
  - Exceptions and traps, `finally`, `exit` / `die`, `Process\exit`, `Process\abort`, `Process\name`, `assert`.
  - `const` (local, namespace, class); `internal` visibility (same as `public` within one module); magic constants; heredoc and nowdoc.
  - `main`; single-file and directory mode; `Env\get()` and `Io\readAll()`; a CGI smoke test behind NGINX via `fcgiwrap`.
- **Out** (rejected with a `not-supported-in-poc` diagnostic where the syntax is recognized): user-defined generics, type aliases, intersection types; generators and `yield`; `|>` pipe and `?` partial application; property hooks and asymmetric visibility; late static binding (`static::`, `new static`, `static` return type); the `Callable` interface; fibers, tasks, channels, threads; templates; package manager and manifest; formatter; reflection; attributes; JIT; AOT.

The POC must prove the hard combination: static types with value semantics, copy-on-write and refcounting.

POC architecture requirements (so the FastCGI server is additive, not a refactor):

1. All interpreter state lives in an `Isolate` struct; no Rust globals or `static mut`.
2. Stdlib I/O and environment go through handles the isolate provides, never `std::io::stdout()` or `std::env` directly.
3. The compiled program (parsed, type-checked IR) is immutable and shareable, separate from runtime state.

### Server model

| Stage | Model | How NGINX connects |
| --- | --- | --- |
| POC | CGI: one `scb run` process per request | `fastcgi_pass` to `fcgiwrap` |
| v1 | `scb-fpm`: shared-nothing, a fresh isolate per request, statics reset, program compiled once per worker | `fastcgi_pass` to SCB workers |
| Later | Built-in long-running HTTP server (`Http\serve`), opt-in; needs fibers, `Net`, `Http` | `proxy_pass` |

`scb-fpm` needs no fibers: synchronous worker processes, as in PHP-FPM. New pieces: FastCGI protocol, process manager (socket, worker pool, respawn, max requests per worker), request lifecycle, per-request memory and time limits.

Server-mode semantics: an uncaught throwable becomes a `500` response; `exit` ends the request, not the worker.

## Deferred and open

Deferred to v2:

- TBD: FFI (`extern` and `unsafe` keywords reserved now)
- Compile-time codegen (for DI, ORMs, serializers): attribute-driven generators such as `#[Derive(Json\Codec)]`, written in SCB and run at compile time over the read-only reflection API. Sandboxed (no I/O, no network, deterministic), cached, inspectable via `scb expand`, errors mapped to user source. No syntactic macros.
- Threads implementation (model decided; see Threads)
- Package workspaces: `[workspace]` members sharing one lockfile and resolution, with `[workspace.dependencies]` version inheritance
- Exhaustiveness checking over integer ranges
- Delegation via a built-in `#[Delegate(Interface)]` generator on a readonly property: interfaces only, explicit methods override, conflicting delegates are a compile error unless overridden, forwarding not inheritance (`$this` inside the delegate is the delegate), statics and constants not delegated. No new syntax.
- PHP-to-SCB migration tooling

Still open:

- [ ] PHP migration guide (after POC testing and approval)
