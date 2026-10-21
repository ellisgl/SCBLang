//! Checker unit tests: one rule per test, valid and invalid snippets side by side.

use std::path::Path;

use scb_syntax::SourceMap;

use crate::check;

/// Diagnostic kinds for a whole file (empty when it checks clean).
fn file(src: &str) -> Vec<&'static str> {
    let mut sources = SourceMap::new();
    sources
        .add(Path::new("main.scb"), src.as_bytes().to_vec())
        .unwrap();
    match check(sources) {
        Ok(_) => Vec::new(),
        Err((_, diags)) => diags.iter().map(|d| d.kind).collect(),
    }
}

/// Diagnostic kinds for statements in `main`, after optional declarations.
fn body_with(decls: &str, body: &str) -> Vec<&'static str> {
    file(&format!(
        "namespace T;\n{decls}\nfunction main(): void\n{{\n{body}\n}}\n"
    ))
}

fn body(src: &str) -> Vec<&'static str> {
    body_with("", src)
}

const OK: [&str; 0] = [];

// ---------- declarations and locals ----------

#[test]
fn locals_need_a_declaration_and_do_not_shadow() {
    assert_eq!(body("$x: int = 1; $x = 2;"), OK);
    assert_eq!(body("$x = 1;"), ["undeclared-variable"]);
    assert_eq!(body("echo $nope;"), ["undeclared-variable"]);
    assert_eq!(body("$x: int = 1; $x: int = 2;"), ["redeclared-variable"]);
    assert_eq!(body("$x: int = $x;"), ["undeclared-variable"]);
    assert_eq!(body("if (true) { $a: int = 1; } else { $a: int = 2; }"), OK);
    assert_eq!(
        body("const N: int = 3; N = 4;"),
        ["invalid-assignment-target"]
    );
    assert_eq!(body("readonly $r: int = 1; $r = 2;"), ["readonly-write"]);
}

#[test]
fn literal_widths_and_ranges() {
    assert_eq!(
        body("$a: u8 = 255; $b: i8 = -128; $c: u64 = 18446744073709551615;"),
        OK
    );
    assert_eq!(body("$a: u8 = 256;"), ["literal-out-of-range"]);
    assert_eq!(body("$a: u8 = -1;"), ["literal-out-of-range"]);
    assert_eq!(body("$f: f64 = 1;"), ["type-mismatch"]);
    assert_eq!(body("$i: int = 1.5;"), ["type-mismatch"]);
    assert_eq!(body("$f: f32 = 1.5; $g: f32 = $f * 2.0;"), OK);
    assert_eq!(body("$x: i8 = 1; $x += 1; $y: i8 = $x + 100;"), OK);
    assert_eq!(
        body("$x: i8 = 1; $y: i8 = $x + 200;"),
        ["literal-out-of-range"]
    );
    assert_eq!(body("$n: ?u8 = 5;"), OK);
}

#[test]
fn no_implicit_widening_and_division_rules() {
    assert_eq!(
        body("$a: i32 = 1; $b: i64 = 2; $c: i64 = $a + $b;"),
        ["type-mismatch"]
    );
    assert_eq!(body("$a: i32 = 1; $c: i64 = (i64) $a + 2;"), OK);
    assert_eq!(
        body("$x: f64 = 7.0 / 2.0; $y: int = 7 ~/ 2; $z: int = 7 % 2;"),
        OK
    );
    assert_eq!(body("$x: int = 7 / 2;"), ["type-mismatch"]);
    assert_eq!(body("$x: f64 = 7.0 ~/ 2.0;"), ["type-mismatch"]);
    assert_eq!(body("$x: f64 = 2.5 * 2;"), ["type-mismatch"]);
}

#[test]
fn shifts_powers_and_bitwise() {
    assert_eq!(
        body("$x: u8 = 1; $y: u8 = $x << 3; $n: u32 = 2; $z: u8 = $x << $n;"),
        OK
    );
    assert_eq!(
        body("$x: u8 = 1; $n: int = 3; $y: u8 = $x << $n;"),
        ["type-mismatch"]
    );
    assert_eq!(body("$p: int = 2 ** 10; $q: f64 = 2.0 ** 0.5;"), OK);
    assert_eq!(body("$n: int = 3; $r: int = 2 ** $n;"), ["type-mismatch"]);
    assert_eq!(
        body("$a: bool = true; $b: bool = $a & false;"),
        ["bitwise-on-bool"]
    );
    assert_eq!(body("$a: int = 12; $b: int = $a & 10 | ~$a ^ 3;"), OK);
}

#[test]
fn unsigned_negation() {
    assert_eq!(body("$u: u8 = 5; $v: u8 = -$u;"), ["unsigned-negation"]);
    assert_eq!(body("$i: i8 = 5; $j: i8 = -$i;"), OK);
}

#[test]
fn conditions_must_be_bool() {
    assert_eq!(body("$n: int = 1; if ($n) { }"), ["non-bool-condition"]);
    assert_eq!(
        body("$l: list<int> = []; if (!$l) { }"),
        ["non-bool-condition"]
    );
    assert_eq!(body("$n: int = 1; while ($n) { }"), ["non-bool-condition"]);
    assert_eq!(
        body("$n: int = 1; $s: string = $n ? 'a' : 'b';"),
        ["non-bool-condition"]
    );
    assert_eq!(
        body("$n: int = 1; if ($n > 0 && $n < 5 || !($n == 3)) { }"),
        OK
    );
    assert_eq!(body("$x: int = 0; if ($x = 5) { }"), ["non-bool-condition"]);
}

// ---------- strings ----------

#[test]
fn concatenation_and_stringable() {
    assert_eq!(body("$s: string = 'a' . 'b';"), OK);
    assert_eq!(body("$s: string = 'a' . 5;"), ["type-mismatch"]);
    assert_eq!(
        body("$n: int = 5; $s: string = \"n={$n}\"; echo $n, 1.5, 'x'; $t: string = (string) $n;"),
        OK
    );
    assert_eq!(body("echo true;"), ["type-mismatch"]);
    assert_eq!(body("$m: ?string = null; echo $m;"), ["type-mismatch"]);
    assert_eq!(body("$m: ?string = null; echo $m ?? '-';"), OK);
    assert_eq!(body("$v: int|string = 1; echo \"{$v}\";"), OK);
    assert_eq!(body("$b: bytes = b\"x\"; echo $b;"), ["type-mismatch"]);
    assert_eq!(
        body("$s: string = 'x'; $c: string = $s[0];"),
        ["string-subscript"]
    );
}

#[test]
fn stringable_classes() {
    let decls = "class Name implements Stringable { public function __construct(private $v: string) {} public function toString(): string { return $this->v; } }\nclass Plain { }";
    assert_eq!(
        body_with(decls, "$n: Name = new Name('a'); echo $n, \"{$n}\";"),
        OK
    );
    assert_eq!(
        body_with(decls, "$p: Plain = new Plain(); echo $p;"),
        ["type-mismatch"]
    );
    assert_eq!(
        file("namespace T;\nclass N implements Stringable { }\nfunction main(): void {}"),
        ["missing-method"]
    );
}

// ---------- nullability and flow typing ----------

#[test]
fn nullable_flow_typing() {
    assert_eq!(
        body("$v: ?int = null; if ($v != null) { $w: int = $v; }"),
        OK
    );
    assert_eq!(body("$v: ?int = null; $w: int = $v;"), ["type-mismatch"]);
    assert_eq!(
        body("$v: ?int = null; if ($v == null) { return; } $w: int = $v;"),
        OK
    );
    assert_eq!(
        body("$v: ?int = null; if ($v == null) { $w: int = $v; }"),
        ["type-mismatch"]
    );
    assert_eq!(body("$v: ?int = null; if ($v != null && $v > 3) { }"), OK);
    assert_eq!(
        body(
            "$line: ?string = null; while (($line = \\Io\\readLine()) != null) { $s: string = $line; }"
        ),
        OK
    );
    assert_eq!(body("$v: ?int = null; $w: int = $v ?? 0;"), OK);
}

#[test]
fn loops_invalidate_narrowing() {
    assert_eq!(
        body("$v: ?int = 1; if ($v != null) { while (true) { $w: int = $v; $v = null; } }"),
        ["type-mismatch"]
    );
}

#[test]
fn null_comparisons() {
    assert_eq!(body("$n: int = 1; if ($n == null) { }"), ["type-mismatch"]);
    assert_eq!(body("$n: ?int = 1; if ($n == null) { }"), OK);
}

#[test]
fn unions_and_instanceof() {
    assert_eq!(
        body(
            "$u: int|string = 1; if ($u instanceof string) { $s: string = $u; } else { $i: int = $u; }"
        ),
        OK
    );
    assert_eq!(body("$u: int|string = 1; $i: int = $u;"), ["type-mismatch"]);
    assert_eq!(
        body("$i: int = 1; if ($i instanceof string) { }"),
        ["type-mismatch"]
    );
}

// ---------- equality and identity ----------

#[test]
fn equality_rules() {
    let decls = "struct P { public function __construct(public $x: int) {} }\nclass C { }\nclass E implements Equatable { public function equals($other: self): bool { return true; } }";
    assert_eq!(
        body_with(
            decls,
            "$a: P = new P(1); $b: P = new P(1); $eq: bool = $a == $b;"
        ),
        OK
    );
    assert_eq!(
        body_with(
            decls,
            "$a: C = new C(); $b: C = new C(); $eq: bool = $a == $b;"
        ),
        ["unsatisfied-bound"]
    );
    assert_eq!(
        body_with(
            decls,
            "$a: C = new C(); $b: C = new C(); $same: bool = $a is $b;"
        ),
        OK
    );
    assert_eq!(
        body_with(
            decls,
            "$a: P = new P(1); $b: P = new P(1); $same: bool = $a is $b;"
        ),
        ["type-mismatch"]
    );
    assert_eq!(
        body_with(
            decls,
            "$a: E = new E(); $b: E = new E(); $eq: bool = $a == $b;"
        ),
        OK
    );
    assert_eq!(
        body("$a: list<int> = [1]; $b: list<int> = [1]; $eq: bool = $a == $b;"),
        OK
    );
}

#[test]
fn comparable_rules() {
    assert_eq!(
        body("$a: string = 'a'; $lt: bool = $a < 'b'; $c: int = 1 <=> 2;"),
        OK
    );
    assert_eq!(
        body("$a: bool = true; $lt: bool = $a < false;"),
        ["unsatisfied-bound"]
    );
    assert_eq!(
        body("$f: list<bool> = [true]; $f->sort();"),
        ["unsatisfied-bound"]
    );
    assert_eq!(
        body("$f: list<f64> = [1.5]; $f->sort(); $m: ?f64 = $f->min();"),
        OK
    );
}

// ---------- collections ----------

#[test]
fn collection_literals() {
    assert_eq!(
        body("$l: list<int> = []; $m: map<string, int> = []; $s: set<int> = [1, 2];"),
        OK
    );
    assert_eq!(body("foreach ([] as $x) { }"), ["cannot-infer"]);
    assert_eq!(body("foreach ([1, 'a'] as $x) { }"), ["type-mismatch"]);
    assert_eq!(body("$l: list<int|string> = [1, 'a'];"), OK);
    assert_eq!(
        body("$m: map<f64, int> = [1.5 => 1];"),
        ["unsatisfied-bound"]
    );
    assert_eq!(
        body("$t: tuple<int, string> = [1, 'a']; $i: int = $t[0]; $s: string = $t[1];"),
        OK
    );
    assert_eq!(
        body("$t: tuple<int, string> = [1, 'a']; $x: int = $t[2];"),
        ["tuple-index"]
    );
    assert_eq!(
        body("$t: tuple<int, string> = [1, 'a', 2];"),
        ["type-mismatch"]
    );
}

#[test]
fn collection_methods_and_generics() {
    assert_eq!(
        body(
            "$l: list<int> = [1, 2]; $s: list<string> = $l->map(fn($x: int): string => (string) $x);"
        ),
        OK
    );
    assert_eq!(
        body(
            "$l: list<int> = [1, 2]; $n: int = $l->reduce(0, fn($a: int, $x: int): int => $a + $x);"
        ),
        OK
    );
    assert_eq!(
        body("$l: list<int> = [1, 2]; $e: list<int> = $l->filter(fn($x: int): bool => $x > 1);"),
        OK
    );
    assert_eq!(
        body("$l: list<int> = [1]; $l->push('a');"),
        ["type-mismatch"]
    );
    assert_eq!(
        body("$m: map<string, int> = ['a' => 1]; $v: ?int = $m->get('a'); $n: int = $m['a'] ?? 0;"),
        OK
    );
    assert_eq!(
        body(
            "$m: map<string, int> = ['a' => 1]; foreach ($m as $k => $v) { $s: string = $k; $i: int = $v; }"
        ),
        OK
    );
    assert_eq!(
        body(
            "$l: list<string> = ['a']; echo $l->join(','); $n: list<int> = [1]; echo $n->join(',');"
        ),
        OK
    );
    assert_eq!(
        body("$l: list<list<int>> = [[1]]; echo $l->join(',');"),
        ["unsatisfied-bound"]
    );
    assert_eq!(
        body("$r: Range<int> = (0..=9)->step(3); foreach ($r as $i) { $j: int = $i; }"),
        OK
    );
}

#[test]
fn strings_are_not_iterable() {
    assert_eq!(body("foreach ('abc' as $c) { }"), ["type-mismatch"]);
}

// ---------- functions, closures, calls ----------

#[test]
fn calls_and_arguments() {
    let decls = "function f($a: int, $b: string = 'x', ...$rest: int): int { return $a; }";
    assert_eq!(
        body_with(
            decls,
            "$r: int = f(1); $s: int = f(1, 'y', 2, 3); $t: int = f(b: 'z', a: 2);"
        ),
        OK
    );
    assert_eq!(body_with(decls, "$r: int = f();"), ["wrong-arguments"]);
    assert_eq!(
        body_with(decls, "$r: int = f(1, c: 2);"),
        ["wrong-arguments"]
    );
    assert_eq!(
        body_with(decls, "$r: int = f(a: 1, 'y');"),
        ["wrong-arguments"]
    );
    assert_eq!(body_with(decls, "$r: int = f('a');"), ["type-mismatch"]);
    assert_eq!(
        body_with(decls, "$xs: list<int> = [1]; $r: int = f(1, 'y', ...$xs);"),
        OK
    );
    assert_eq!(body("$x: int = nope();"), ["unresolved-name"]);
}

#[test]
fn returns() {
    assert_eq!(
        file("namespace T;\nfunction f(): int { }\nfunction main(): void {}"),
        ["missing-return"]
    );
    assert_eq!(
        file(
            "namespace T;\nfunction f($b: bool): int { if ($b) { return 1; } else { return 2; } }\nfunction main(): void {}"
        ),
        OK
    );
    assert_eq!(
        file("namespace T;\nfunction f(): int { while (true) { } }\nfunction main(): void {}"),
        OK
    );
    assert_eq!(
        file(
            "namespace T;\nfunction f(): int { throw new Exception('x'); }\nfunction main(): void {}"
        ),
        OK
    );
    assert_eq!(
        file("namespace T;\nfunction f(): void { return 1; }\nfunction main(): void {}"),
        ["type-mismatch"]
    );
    assert_eq!(
        file("namespace T;\nfunction f(): never { exit(1); }\nfunction main(): void {}"),
        OK
    );
}

#[test]
fn closures_capture_by_value() {
    assert_eq!(
        body("$n: int = 1; $f: fn(): int = fn(): int => $n; $n = 2;"),
        OK
    );
    assert_eq!(
        body("$n: int = 1; $f: fn(): void = fn(): void { $n++; };"),
        ["assign-to-capture"]
    );
    assert_eq!(
        body("$l: list<int> = []; $f: fn(): void = fn(): void { $l->push(1); };"),
        ["assign-to-capture"]
    );
    assert_eq!(
        body("$n: int = 1; $f: fn(int): int = fn($n: int): int => $n;"),
        ["redeclared-variable"]
    );
    assert_eq!(
        body("$c: Ref<int> = new Ref(0); $f: fn(): void = fn(): void { $c->value++; };"),
        OK
    );
    assert_eq!(
        body("$f: fn(int): int = fn($x: int): int => $x * 2; $y: int = $f(3);"),
        OK
    );
    assert_eq!(
        body("$f: fn(int): int = fn($x: int): string => 'a';"),
        ["type-mismatch"]
    );
}

#[test]
fn inout_and_exclusivity() {
    let decls = "function swap(inout $a: int, inout $b: int): void { }\nfunction bump(inout $n: int): void { }\nfunction g(inout $n: int): int { return 0; }";
    assert_eq!(
        body_with(decls, "$a: int = 1; $b: int = 2; swap(inout $a, inout $b);"),
        OK
    );
    assert_eq!(
        body_with(decls, "$a: int = 1; swap(inout $a, inout $a);"),
        ["exclusivity-violation"]
    );
    assert_eq!(
        body_with(
            decls,
            "$l: list<int> = [1, 2]; swap(inout $l[0], inout $l[1]);"
        ),
        ["exclusivity-violation"]
    );
    assert_eq!(
        body_with(decls, "$a: int = 1; $a += g(inout $a);"),
        ["exclusivity-violation"]
    );
    assert_eq!(body_with(decls, "$a: int = 1; $a = g(inout $a);"), OK);
    assert_eq!(
        body_with(decls, "$a: int = 1; bump($a);"),
        ["inout-mismatch"]
    );
}

#[test]
fn struct_disjoint_fields_are_not_overlapping() {
    let decls = "struct P { public function __construct(public $x: int, public $y: int) {} }\nfunction swap(inout $a: int, inout $b: int): void { }";
    assert_eq!(
        body_with(
            decls,
            "$p: P = new P(1, 2); swap(inout $p->x, inout $p->y);"
        ),
        OK
    );
}

// ---------- nested mutation ----------

#[test]
fn nested_mutation_rules() {
    let decls = "struct L { public function __construct(public $qty: int) {} }\nfunction lines(): list<L> { return []; }\nclass U { public function __construct(public $name: string) {} }\nfunction user(): U { return new U('a'); }";
    assert_eq!(
        body_with(decls, "$ls: list<L> = [new L(1)]; $ls[0]->qty += 1;"),
        OK
    );
    assert_eq!(
        body_with(decls, "lines()[0]->qty = 1;"),
        ["mutating-temporary"]
    );
    assert_eq!(body_with(decls, "user()->name = 'x';"), OK);
    assert_eq!(
        body_with(decls, "$u: ?U = null; $u?->name = 'x';"),
        ["write-through-nullsafe"]
    );
    assert_eq!(
        body_with(decls, "readonly $l: L = new L(1); $l->qty = 2;"),
        ["readonly-write"]
    );
    assert_eq!(
        body("$m: map<string, list<int>> = []; $m['a'] ??= []; $m['a'][] = 1;"),
        OK
    );
}

#[test]
fn mutating_methods() {
    let decls = "struct P { public function __construct(public $x: int) {} public mutating function inc(): void { $this->x++; } public function bad(): void { $this->x = 1; } }\nfunction mk(): P { return new P(1); }";
    assert_eq!(
        body_with(decls, "$p: P = new P(1); $p->inc();"),
        ["missing-mutating"]
    );
    let ok = "struct P { public function __construct(public $x: int) {} public mutating function inc(): void { $this->x++; } }\nfunction mk(): P { return new P(1); }";
    assert_eq!(body_with(ok, "$p: P = new P(1); $p->inc();"), OK);
    assert_eq!(body_with(ok, "mk()->inc();"), ["mutating-temporary"]);
    assert_eq!(
        body_with(ok, "readonly $p: P = new P(1); $p->inc();"),
        ["readonly-write"]
    );
    assert_eq!(body("[1, 2]->push(3);"), ["mutating-temporary"]);
}

// ---------- classes ----------

#[test]
fn class_rules() {
    assert_eq!(
        file("namespace T;\nclass A { }\nclass B extends A { }\nfunction main(): void {}"),
        ["extend-final"]
    );
    assert_eq!(
        file(
            "namespace T;\nextendable class A { }\nclass B extends A { }\nfunction main(): void {}"
        ),
        OK
    );
    assert_eq!(
        file(
            "namespace T;\nclass A { final public function f(): void {} }\nfunction main(): void {}"
        ),
        ["redundant-final"]
    );
    assert_eq!(
        file(
            "namespace T;\nstruct S { final public function f(): void {} }\nfunction main(): void {}"
        ),
        ["redundant-final"]
    );
    assert_eq!(
        file(
            "namespace T;\nabstract class A { abstract public function f(): int; }\nclass B extends A { }\nfunction main(): void {}"
        ),
        ["missing-method"]
    );
    assert_eq!(
        file(
            "namespace T;\nabstract class A { abstract public function f(): int; }\nclass B extends A { public function f(): int { return 1; } }\nfunction main(): void { $b: A = new B(); $n: int = $b->f(); }"
        ),
        OK
    );
    assert_eq!(
        body_with("abstract class A { }", "$a: A = new A();"),
        ["type-mismatch"]
    );
    assert_eq!(
        body_with(
            "class A { private $x: int = 1; }",
            "$a: A = new A(); echo $a->x;"
        ),
        ["visibility"]
    );
    assert_eq!(
        body_with(
            "class A { public $x: int = 1; }",
            "$a: A = new A(); echo $a->y;"
        ),
        ["unknown-member"]
    );
    assert_eq!(
        file("namespace T;\nclass E extends Throwable { }\nfunction main(): void {}"),
        ["type-mismatch"]
    );
}

#[test]
fn readonly_properties() {
    let decls = "class U { public function __construct(public readonly $id: int) {} public function set(): void { $this->id = 2; } }";
    assert_eq!(body_with(decls, ""), ["readonly-write"]);
    let ok = "class U { public readonly $id: int; public function __construct($id: int) { $this->id = $id; } }";
    assert_eq!(body_with(ok, "$u: U = new U(1); echo $u->id;"), OK);
    assert_eq!(
        body_with(ok, "$u: U = new U(1); $u->id = 3;"),
        ["readonly-write"]
    );
}

#[test]
fn interface_defaults() {
    let conflict = "interface A { public function d(): string { return 'a'; } }\ninterface B { public function d(): string { return 'b'; } }\nclass C implements A, B { }";
    assert_eq!(body_with(conflict, ""), ["default-method-conflict"]);
    let resolved = "interface A { public function d(): string { return 'a'; } }\ninterface B { public function d(): string { return 'b'; } }\nclass C implements A, B { public function d(): string { return parent(A)::d(); } }";
    assert_eq!(body_with(resolved, "$c: C = new C(); echo $c->d();"), OK);
}

#[test]
fn clone_rules() {
    let decls = "class U { public function __construct(public $name: string) {} }\nstruct P { public $x: int = 0; }";
    assert_eq!(
        body_with(
            decls,
            "$u: U = new U('a'); $v: U = clone $u; $w: U = clone($u, name: 'b');"
        ),
        OK
    );
    assert_eq!(
        body_with(decls, "$p: P = new P(); $q: P = clone $p;"),
        ["clone-value-type"]
    );
}

// ---------- enums and match ----------

#[test]
fn enums() {
    let decls = "enum Suit { case HEARTS; case SPADES; }\nenum Status: string { case ON = 'on'; case OFF = 'off'; }\nenum Shape { case CIRCLE($r: f64); case RECT($w: f64, $h: f64); }";
    assert_eq!(
        body_with(
            decls,
            "$s: Suit = Suit::HEARTS; $st: Status = Status::from('on'); $m: ?Status = Status::tryFrom('x'); $v: string = Status::ON->value; $all: list<Status> = Status::cases();"
        ),
        OK
    );
    assert_eq!(body_with(decls, "$all: list<Suit> = Suit::cases();"), OK);
    assert_eq!(
        body_with(
            decls,
            "$c: Shape = Shape::CIRCLE(1.0); $r: Shape = Shape::RECT(h: 2.0, w: 1.0);"
        ),
        OK
    );
    assert_eq!(
        body_with(decls, "$c: Shape = Shape::CIRCLE;"),
        ["missing-case-arguments"]
    );
    assert_eq!(
        body_with(decls, "$s: Suit = Suit::from('x');"),
        ["unknown-member"]
    );
}

#[test]
fn match_exhaustiveness() {
    let decls = "enum Suit { case HEARTS; case SPADES; }\nenum Shape { case CIRCLE($r: f64); case SQ($s: f64); }";
    assert_eq!(
        body_with(
            decls,
            "$s: Suit = Suit::HEARTS; $n: int = match ($s) { Suit::HEARTS => 1, Suit::SPADES => 2 };"
        ),
        OK
    );
    assert_eq!(
        body_with(
            decls,
            "$s: Suit = Suit::HEARTS; $n: int = match ($s) { Suit::HEARTS => 1 };"
        ),
        ["non-exhaustive-match"]
    );
    assert_eq!(
        body_with(
            decls,
            "$s: Shape = Shape::SQ(1.0); $a: f64 = match ($s) { Shape::CIRCLE($r) => $r, Shape::SQ($x) => $x };"
        ),
        OK
    );
    assert_eq!(
        body_with(
            decls,
            "$s: Shape = Shape::SQ(1.0); $a: f64 = match ($s) { Shape::CIRCLE($r) if $r > 1.0 => $r, Shape::SQ($x) => $x };"
        ),
        ["non-exhaustive-match"]
    );
    assert_eq!(
        body("$b: bool = true; $n: int = match ($b) { true => 1, false => 0 };"),
        OK
    );
    assert_eq!(
        body("$b: bool = true; $n: int = match ($b) { true => 1 };"),
        ["non-exhaustive-match"]
    );
    assert_eq!(
        body("$x: int|string = 1; $n: int = match ($x) { $i: int => $i, $s: string => 0 };"),
        OK
    );
    assert_eq!(
        body("$x: ?int = 1; $n: int = match ($x) { null => 0, $i: int => $i };"),
        OK
    );
    assert_eq!(
        body("$x: int = 1; $n: int = match ($x) { 1 => 1, 2 => 2 };"),
        ["non-exhaustive-match"]
    );
    assert_eq!(
        body("$x: int = 1; $n: int = match ($x) { 0..=9 => 1, default => 2 };"),
        OK
    );
    assert_eq!(
        body("$a: int = 1; $s: string = match { $a < 1 => 'x' };"),
        ["non-exhaustive-match"]
    );
}

#[test]
fn match_types() {
    assert_eq!(
        body("$n: int = 1; echo match ($n) { 0 => 'a', default => $n };"),
        ["type-mismatch"]
    );
    assert_eq!(
        body("$n: int = 1; $v: int|string = match ($n) { 0 => 'a', default => $n };"),
        OK
    );
    assert_eq!(
        body(
            "$n: int = 1; $s: string = match ($n) { 0 => 'a', default => throw new Exception('x') };"
        ),
        OK
    );
}

#[test]
fn destructuring() {
    let decls = "struct P { public function __construct(public $x: int, public $y: int) {} }";
    assert_eq!(
        body_with(
            decls,
            "$p: P = new P(1, 2); P { x: $a, y: $b } = $p; $s: int = $a + $b;"
        ),
        OK
    );
    assert_eq!(
        body("$t: tuple<int, string> = [1, 'a']; [$n, $s] = $t; $m: int = $n;"),
        OK
    );
    assert_eq!(
        body("$a: int = 0; $t: tuple<int, int> = [1, 2]; [$a, $b] = $t;"),
        ["redeclared-variable"]
    );
    assert_eq!(body("$l: list<int> = [1]; [$a] = $l;"), ["type-mismatch"]);
    assert_eq!(
        body("$ps: list<tuple<string, int>> = []; foreach ($ps as [$k, $v]) { $s: string = $k; }"),
        OK
    );
}

// ---------- errors ----------

#[test]
fn throw_and_catch() {
    assert_eq!(
        body(
            "try { throw new Exception('x'); } catch ($e: IndexError | KeyError) { $m: string = $e->getMessage(); } finally { }"
        ),
        OK
    );
    assert_eq!(body("throw 5;"), ["type-mismatch"]);
    assert_eq!(body("try { } catch (string) { }"), ["type-mismatch"]);
    assert_eq!(body("exit('bye');"), ["type-mismatch"]);
}

#[test]
fn finally_control_flow() {
    assert_eq!(
        file(
            "namespace T;\nfunction f(): int { try { return 1; } finally { return 0; } }\nfunction main(): void {}"
        ),
        ["control-leaves-finally"]
    );
    assert_eq!(
        body("while (true) { try { } finally { break; } }"),
        ["control-leaves-finally"]
    );
    assert_eq!(
        body("try { } finally { foreach ([1] as $i) { break; } }"),
        OK
    );
    assert_eq!(body("break;"), ["invalid-break"]);
}

// ---------- modules ----------

#[test]
fn name_resolution() {
    assert_eq!(body("echo \\Math\\mod(-7, 2);"), OK);
    assert_eq!(body("echo Math\\mod(-7, 2);"), ["unresolved-name"]);
    assert_eq!(
        file("namespace T;\nuse function Math\\mod;\nfunction main(): void { echo mod(7, 2); }"),
        OK
    );
    assert_eq!(
        file(
            "namespace T;\nuse Math\\Rounding;\nfunction main(): void { $r: Rounding = Rounding::FLOOR; }"
        ),
        OK
    );
    assert_eq!(body("$x: f16 = 1.0;"), ["unresolved-name"]);
    assert_eq!(
        file("namespace Math\\Extra;\nfunction main(): void {}"),
        ["reserved-namespace"]
    );
    assert_eq!(
        file("namespace T;\nclass Ref { }\nfunction main(): void {}"),
        ["prelude-name-redeclared"]
    );
}

#[test]
fn main_signature() {
    assert_eq!(
        file("namespace T;\nfunction main($args: list<string>): int { return 0; }"),
        OK
    );
    assert_eq!(
        file("namespace T;\nfunction main($n: int): void {}"),
        ["invalid-main"]
    );
    assert_eq!(
        file("namespace T;\nfunction other(): void {}"),
        ["missing-main"]
    );
}

#[test]
fn property_initialization() {
    assert_eq!(
        file("namespace T;\nclass U { public $n: string; }\nfunction main(): void {}"),
        ["uninitialized-property"]
    );
    assert_eq!(
        file("namespace T;\nclass U { public $n: ?string; }\nfunction main(): void {}"),
        ["uninitialized-property"]
    );
    assert_eq!(
        file("namespace T;\nclass U { public $n: ?string = null; }\nfunction main(): void {}"),
        OK
    );
    assert_eq!(
        file(
            "namespace T;\nclass U { public $n: string; public function __construct($b: bool) { if ($b) { $this->n = 'a'; } else { throw new Exception('x'); } } }\nfunction main(): void {}"
        ),
        OK
    );
    assert_eq!(
        file(
            "namespace T;\nclass U { public $n: string; public function __construct() { $this->n = 'a'; $this->go(); } public function go(): void {} }\nfunction main(): void {}"
        ),
        OK
    );
}

#[test]
fn numeric_statics() {
    assert_eq!(
        body(
            "$a: i8 = i8::wrap(300); $b: ?u8 = u8::tryFrom(-1); $c: int = int::parse('5'); $m: i64 = i64::MAX; $f: f64 = f64::NAN;"
        ),
        OK
    );
    assert_eq!(body("$c: ?i32 = i32::checkedAdd(1, 2);"), OK);
}

#[test]
fn magic_constants() {
    assert_eq!(
        body("$l: int = __LINE__; $f: string = __FILE__; $ns: string = __NAMESPACE__;"),
        OK
    );
}

#[test]
fn checking_produces_a_program() {
    let mut sources = SourceMap::new();
    sources
        .add(
            Path::new("main.scb"),
            b"namespace T;\nfunction main(): void { echo 'hi'; }\n".to_vec(),
        )
        .unwrap();
    let p = check(sources).expect("checks");
    assert_eq!(p.funcs[p.main.0 as usize].name, "T\\main");
}

// ---------- TIR shape ----------

fn program(src: &str) -> crate::Program {
    let mut sources = SourceMap::new();
    sources
        .add(Path::new("main.scb"), src.as_bytes().to_vec())
        .unwrap();
    match check(sources) {
        Ok(p) => p,
        Err((sources, diags)) => panic!(
            "{}",
            diags
                .iter()
                .map(|d| d.render(&sources))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

fn main_stmts(p: &crate::Program) -> &[crate::tir::Stmt] {
    match &p.funcs[p.main.0 as usize].body {
        crate::tir::Body::Block(b) => &b.stmts,
        other => panic!("unexpected body {other:?}"),
    }
}

/// The value of `let` statement `i` in `main`.
fn let_value(p: &crate::Program, i: usize) -> &crate::tir::Expr {
    match &main_stmts(p)[i].kind {
        crate::tir::StmtKind::Let { value, .. } => value,
        other => panic!("statement {i} is {other:?}"),
    }
}

#[test]
fn tir_named_arguments_keep_written_order() {
    let p = program(
        "namespace T;\nfunction f($a: int, $b: string): int { return $a; }\nfunction main(): void { $r: int = f(b: 'z', a: 2); }",
    );
    let crate::tir::ExprKind::Call { args, .. } = &let_value(&p, 0).kind else {
        panic!("expected a call")
    };
    let params: Vec<u32> = args.iter().map(|a| a.param).collect();
    assert_eq!(params, [1, 0]);
}

#[test]
fn tir_literals_take_the_declared_width() {
    use crate::tir::{ExprKind, Lit};
    let p = program("namespace T;\nfunction main(): void { $a: u8 = 200; $b: f32 = 1.5; }");
    let a = let_value(&p, 0);
    assert!(matches!(a.kind, ExprKind::Lit(Lit::Int(200))));
    assert_eq!(a.ty, crate::ty::Ty::U8);
    assert_eq!(let_value(&p, 1).ty, crate::ty::Ty::F32);
}

#[test]
fn tir_closures_record_captures() {
    use crate::tir::ExprKind;
    let p = program(
        "namespace T;\nfunction main(): void { $n: int = 1; $f: fn(): int = fn(): int => $n; }",
    );
    let ExprKind::Closure { func, captures } = &let_value(&p, 1).kind else {
        panic!("expected a closure")
    };
    assert_eq!(captures.len(), 1);
    let (inner, outer) = captures[0];
    assert_eq!(
        p.funcs[p.main.0 as usize].locals[outer.0 as usize].name,
        "n"
    );
    assert_eq!(p.funcs[func.0 as usize].locals[inner.0 as usize].name, "n");
}

#[test]
fn tir_stdlib_calls_are_builtins() {
    use crate::tir::{CallTarget, ExprKind};
    let p = program(
        "namespace T;\nfunction main(): void { $l: list<int> = [1]; $n: int = $l->count(); }",
    );
    let ExprKind::Call {
        target: CallTarget::Builtin { builtin, .. },
        ..
    } = &let_value(&p, 1).kind
    else {
        panic!("expected a builtin call")
    };
    assert_eq!(builtin.0, "list.count");
}

// ---------- robustness ----------

/// The checker never panics: every valid conformance program, mutated by swapping
/// types, literals and operators, checks to a result (usually diagnostics).
#[test]
fn checker_never_panics_on_mutated_programs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
    let mut files = Vec::new();
    collect(&root, &mut files);
    let swaps: &[(&str, &str)] = &[
        ("int", "string"),
        ("string", "?int"),
        (": void", ": int"),
        ("1", "1.5"),
        ("'", "\""),
        ("==", "<"),
        ("->", "?->"),
        ("$", "$x"),
        ("true", "null"),
        ("list<", "map<int, "),
        ("public ", "private "),
        ("(", "(inout "),
        ("return ", "return;//"),
    ];
    let mut checked = 0;
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        if text.contains("EXPECT-COMPILE-ERROR") {
            continue;
        }
        for (from, to) in swaps {
            for (i, _) in text.match_indices(from).take(6) {
                let mutated = format!("{}{to}{}", &text[..i], &text[i + from.len()..]);
                let mut sources = SourceMap::new();
                sources
                    .add(Path::new("main.scb"), mutated.into_bytes())
                    .unwrap();
                let _ = check(sources);
                checked += 1;
            }
        }
    }
    assert!(checked > 500, "only {checked} mutations");
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            if !p.join("main.scb").is_file() {
                collect(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "scb") {
            out.push(p);
        }
    }
}

#[test]
fn data_enums_have_no_cases() {
    assert_eq!(
        body_with(
            "enum Shape { case CIRCLE($r: f64); }",
            "$all: list<Shape> = Shape::cases();"
        ),
        ["unknown-member"]
    );
}

// ---------- review regressions ----------

#[test]
fn match_and_ternary_arms_fit_the_expected_type() {
    assert_eq!(
        body("$n: int = 1; $s: string = match ($n) { 0 => 'a', default => 5 };"),
        ["type-mismatch"]
    );
    assert_eq!(
        body("$b: bool = true; $s: string = $b ? 'a' : 5;"),
        ["type-mismatch"]
    );
}

#[test]
fn backed_enum_values() {
    assert_eq!(
        body_with("enum S: string { case A = 1; }", ""),
        ["type-mismatch"]
    );
    assert_eq!(
        body_with("enum S: string { case A = 'x'; case B = 'x'; }", ""),
        ["duplicate-declaration"]
    );
    assert_eq!(
        body_with("enum S: int { case A = 1; case B = -1; }", ""),
        OK
    );
}

#[test]
fn property_defaults_are_checked() {
    assert_eq!(
        body_with("class C { public $x: int = 'a'; }", ""),
        ["type-mismatch"]
    );
    assert_eq!(
        body_with(
            "function f(): int { return 1; }\nstruct P { public $x: int = f(); }",
            ""
        ),
        ["non-constant-expression"]
    );
    assert_eq!(
        body_with(
            "class B { }\nclass C { public $b: B = new B(); public $n: ?int = null; }",
            ""
        ),
        OK
    );
}

#[test]
fn members_only_where_they_belong() {
    assert_eq!(
        body_with("interface I { public $x: int; }", ""),
        ["invalid-member"]
    );
    assert_eq!(
        body_with("enum E { case A; public $x: int = 1; }", ""),
        ["invalid-member"]
    );
    assert_eq!(
        body_with("class C { public mutating function f(): void {} }", ""),
        ["invalid-modifier"]
    );
    assert_eq!(
        body_with(
            "struct P { public static mutating function f(): void {} }",
            ""
        ),
        ["invalid-modifier"]
    );
    assert_eq!(
        body_with("struct P { public function __destruct() {} }", ""),
        ["invalid-member"]
    );
}

#[test]
fn const_cycles() {
    assert_eq!(
        body_with("const A: int = B;\nconst B: int = A;", ""),
        ["const-cycle"]
    );
    assert_eq!(
        body_with("const A: int = B + 1;\nconst B: int = 2;", ""),
        OK
    );
}

#[test]
fn bounds_on_inferred_type_parameters() {
    assert_eq!(body("echo \\Math\\mod(1.5, 2.5);"), ["unsatisfied-bound"]);
    assert_eq!(body("$m: map<f64, int> = [];"), ["unsatisfied-bound"]);
    assert_eq!(body("$s: set<f64> = [];"), ["unsatisfied-bound"]);
    assert_eq!(
        body("$m: map<f64, int> = [1.5 => 1];"),
        ["unsatisfied-bound"]
    );
}

#[test]
fn readonly_assigned_once() {
    assert_eq!(
        body_with(
            "class U { public readonly $x: int = 5; public function __construct() { $this->x = 6; } }",
            ""
        ),
        ["readonly-write"]
    );
}

#[test]
fn interface_implementations_match() {
    assert_eq!(
        body_with(
            "class N implements Stringable { public function toString(): int { return 1; } }",
            ""
        ),
        ["incompatible-override"]
    );
    assert_eq!(
        body_with(
            "class V implements Equatable { public function equals($other: V): bool { return true; } }",
            ""
        ),
        OK
    );
    assert_eq!(
        body_with(
            "interface S { public function area(): f64; }\nclass Q implements S { public function area($scale: f64): f64 { return $scale; } }",
            ""
        ),
        ["incompatible-override"]
    );
}

#[test]
fn readonly_properties_narrow() {
    let decls = "class U { public function __construct(public readonly $email: ?string, public $nick: ?string) {} }";
    assert_eq!(
        body_with(
            decls,
            "$u: U = new U(null, null); if ($u->email != null) { $s: string = $u->email; }"
        ),
        OK
    );
    assert_eq!(
        body_with(
            decls,
            "$u: U = new U(null, null); if ($u->nick != null) { $s: string = $u->nick; }"
        ),
        ["type-mismatch"]
    );
    assert_eq!(
        body_with(
            decls,
            "$u: U = new U(null, null); if ($u->email != null) { $u = new U(null, null); $s: string = $u->email; }"
        ),
        ["type-mismatch"]
    );
}

#[test]
fn duplicate_use_aliases() {
    assert_eq!(
        file("namespace T;\nuse Math\\Rounding;\nuse Math\\Rounding;\nfunction main(): void {}"),
        ["duplicate-declaration"]
    );
}

#[test]
fn interface_constants_are_inherited() {
    assert_eq!(
        body_with(
            "interface I { public const X: int = 1; }\nclass C implements I { }",
            "echo C::X, I::X;"
        ),
        OK
    );
}

#[test]
fn structs_with_nullable_fields_are_equatable() {
    assert_eq!(
        body_with(
            "struct N { public $next: ?N = null; }",
            "$n: N = new N(); $eq: bool = $n == new N();"
        ),
        OK
    );
    assert_eq!(body("$a: ?int = 1; $m: map<?int, string> = [];"), OK);
}

#[test]
fn new_self_is_the_enclosing_class() {
    let decls = "extendable class Money { public function __construct(public $cents: int) {} public static function zero(): self { return new self(0); } }\nclass Euro extends Money { }";
    assert_eq!(
        body_with(
            decls,
            "$m: Money = Euro::zero(); $n: Money = Money::zero();"
        ),
        OK
    );
    // No late binding: `Euro::zero()` is a `Money`, not a `Euro`.
    assert_eq!(
        body_with(decls, "$e: Euro = Euro::zero();"),
        ["type-mismatch"]
    );
    assert_eq!(body("$x: int = new self();"), ["unresolved-name"]);
    assert_eq!(
        body_with(
            "abstract class A { public static function make(): A { return new self(); } }",
            ""
        ),
        ["type-mismatch"]
    );
    assert_eq!(
        body_with(
            "struct P { public $x: int = 0; public function copy(): P { return new self(); } }",
            ""
        ),
        OK
    );
}
