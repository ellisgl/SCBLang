use std::path::Path;

use crate::ast::StmtKind;
use crate::{SourceMap, dump_expr, dump_file, dump_stmts, parse_source};

/// Joins a multi-line dump onto one line.
fn flat(dump: &str) -> String {
    dump.split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses a whole file; returns the dump (on one line) and the diagnostic kinds.
fn file(src: &str) -> (String, Vec<&'static str>) {
    let mut map = SourceMap::new();
    let id = map
        .add(Path::new("t.scb"), src.as_bytes().to_vec())
        .unwrap();
    let (ast, diags) = parse_source(id, &map.file(id).text);
    let kinds = diags.iter().map(|d| d.kind).collect();
    (flat(&ast.map(|a| dump_file(&a)).unwrap_or_default()), kinds)
}

/// Wraps statements in a function body.
fn body(stmts: &str) -> String {
    format!("namespace T;\nfunction f(): void\n{{\n{stmts}\n}}\n")
}

/// Diagnostic kinds for statements in a function body.
fn errors(stmts: &str) -> Vec<&'static str> {
    file(&body(stmts)).1
}

/// Diagnostic kinds for a whole file.
fn file_errors(src: &str) -> Vec<&'static str> {
    file(src).1
}

/// The statements of the function body, dumped; panics on diagnostics.
fn stmts(src: &str) -> String {
    let mut map = SourceMap::new();
    let text = body(src);
    let id = map.add(Path::new("t.scb"), text.into_bytes()).unwrap();
    let (ast, diags) = parse_source(id, &map.file(id).text);
    let rendered: Vec<String> = diags.iter().map(|d| d.render(&map)).collect();
    assert!(diags.is_empty(), "{src}:\n{}", rendered.join("\n"));
    let ast = ast.unwrap();
    let crate::ast::Decl::Function(f) = &ast.decls[0] else {
        panic!("expected a function")
    };
    flat(&dump_stmts(&f.body.as_ref().unwrap().stmts))
}

/// One expression statement, dumped.
fn expr(src: &str) -> String {
    let mut map = SourceMap::new();
    let text = body(&format!("{src};"));
    let id = map.add(Path::new("t.scb"), text.into_bytes()).unwrap();
    let (ast, diags) = parse_source(id, &map.file(id).text);
    let rendered: Vec<String> = diags.iter().map(|d| d.render(&map)).collect();
    assert!(diags.is_empty(), "{src}:\n{}", rendered.join("\n"));
    let ast = ast.unwrap();
    let crate::ast::Decl::Function(f) = &ast.decls[0] else {
        panic!("expected a function")
    };
    let StmtKind::Expr(e) = &f.body.as_ref().unwrap().stmts[0].kind else {
        panic!("expected an expression statement")
    };
    flat(&dump_expr(e))
}

// ---------- precedence (SPEC operator table) ----------

#[test]
fn arithmetic_precedence_and_associativity() {
    assert_eq!(expr("$a + $b * $c"), "(+ $a (* $b $c))");
    assert_eq!(expr("$a - $b - $c"), "(- (- $a $b) $c)");
    assert_eq!(expr("$a ~/ $b % $c"), "(% (~/ $a $b) $c)");
    assert_eq!(expr("$a << 1 + 2"), "(<< $a (+ 1 2))");
}

#[test]
fn power_is_right_associative_and_binds_tighter_than_unary() {
    assert_eq!(expr("2 ** 3 ** 2"), "(** 2 (** 3 2))");
    assert_eq!(expr("-2 ** 2"), "(- (** 2 2))");
    assert_eq!(expr("2 ** -1"), "(** 2 (- 1))");
}

#[test]
fn concat_is_looser_than_additive() {
    assert_eq!(expr("'a' . $x + 1"), "(. \"a\" (+ $x 1))");
}

#[test]
fn range_sits_between_concat_and_pipe() {
    assert_eq!(expr("$a + 1 ..< $b * 2"), "(..< (+ $a 1) (* $b 2))");
    assert_eq!(expr("0..=$n == $r"), "(== (..= 0 $n) $r)");
}

#[test]
fn comparison_and_equality_do_not_chain() {
    assert_eq!(expr("$a < $b == $c"), "(== (< $a $b) $c)");
    assert_eq!(errors("$a < $b < $c;"), ["syntax-error"]);
    assert_eq!(errors("$a == $b != $c;"), ["syntax-error"]);
    assert_eq!(errors("$a .. $b;"), ["bare-range-operator"]);
}

#[test]
fn logical_and_bitwise_levels() {
    assert_eq!(expr("$a || $b && $c"), "(|| $a (&& $b $c))");
    assert_eq!(expr("$a | $b ^ $c & $d"), "(| $a (^ $b (& $c $d)))");
    assert_eq!(expr("$a & $b == $c"), "(& $a (== $b $c))");
    assert_eq!(expr("$a ?? $b ?? $c"), "(?? $a (?? $b $c))");
    assert_eq!(expr("$a ?? $b || $c"), "(?? $a (|| $b $c))");
}

#[test]
fn not_is_looser_than_instanceof() {
    assert_eq!(expr("!$x instanceof Foo"), "(! (instanceof $x (path Foo)))");
    assert_eq!(expr("!$a && $b"), "(&& (! $a) $b)");
    assert_eq!(expr("!$a * $b"), "(* (! $a) $b)");
}

#[test]
fn identity_operator() {
    assert_eq!(expr("$a is $b"), "(is $a $b)");
}

#[test]
fn ternary_and_assignment() {
    assert_eq!(expr("$x = $c ? 1 : 2"), "(= $x (?: $c 1 2))");
    assert_eq!(expr("$a = $b = 3"), "(= $a (= $b 3))");
    assert_eq!(expr("$a ?? $b ? 1 : 2"), "(?: (?? $a $b) 1 2)");
    assert_eq!(
        expr("$c ? ($d ? 1 : 2) : 3"),
        "(?: $c (paren (?: $d 1 2)) 3)"
    );
    assert_eq!(errors("$c ? 1 : $d ? 2 : 3;"), ["syntax-error"]);
    assert_eq!(errors("$c ? $d ? 1 : 2 : 3;"), ["syntax-error"]);
}

#[test]
fn compound_assignments() {
    assert_eq!(expr("$x ~/= 2"), "(~/= $x 2)");
    assert_eq!(expr("$x ??= []"), "(??= $x (list))");
    assert_eq!(expr("$x .= 'a'"), "(.= $x \"a\")");
    assert_eq!(expr("$m[$k][] = 1"), "(= (push-slot (index $m $k)) 1)");
    assert_eq!(errors("1 = 2;"), ["invalid-assignment-target"]);
    assert_eq!(errors("f() = 2;"), ["invalid-assignment-target"]);
}

#[test]
fn assignment_inside_condition() {
    assert_eq!(
        stmts("while (($line = \\Io\\readLine()) != null) { }"),
        "(while (!= (paren (= $line (call \\Io\\readLine))) null) (block))"
    );
}

#[test]
fn throw_and_exit_are_expressions() {
    assert_eq!(
        expr("$x ?? throw new E('no')"),
        "(?? $x (throw (new E \"no\")))"
    );
    assert_eq!(
        expr("\\Env\\get('PORT') ?? exit(2)"),
        "(?? (call \\Env\\get \"PORT\") (exit 2))"
    );
    assert_eq!(stmts("exit;"), "(exit-stmt)");
    assert_eq!(stmts("die;"), "(exit-stmt)");
    assert_eq!(stmts("exit();"), "(exit)");
    assert_eq!(errors("$x = exit;"), ["syntax-error"]);
}

#[test]
fn unary_and_postfix() {
    assert_eq!(expr("-$x"), "(- $x)");
    assert_eq!(expr("~$x"), "(~ $x)");
    assert_eq!(expr("++$i"), "(++pre $i)");
    assert_eq!(expr("$i--"), "(post-- $i)");
    assert_eq!(expr("-$a->b"), "(- (-> $a b))");
}

// ---------- casts ----------

#[test]
fn casts_only_for_primitive_types() {
    assert_eq!(expr("(i8) $x"), "(cast-i8 $x)");
    assert_eq!(expr("(string) $x . 'a'"), "(. (cast-string $x) \"a\")");
    assert_eq!(expr("(u32) -1"), "(cast-u32 (- 1))");
    assert_eq!(expr("(MAX) - 1"), "(- (paren MAX) 1)");
    assert_eq!(expr("(i64) $f ** 2"), "(cast-i64 (** $f 2))");
}

#[test]
fn bool_cast_is_diagnosed() {
    assert_eq!(errors("$b = (bool) $n;"), ["bool-cast"]);
    assert_eq!(errors("$b = (array) $n;"), ["removed-syntax"]);
    // A parenthesized constant named `bool` would be an expression, not a cast.
    assert_eq!(errors("$b = (bool);"), Vec::<&str>::new());
}

// ---------- postfix chains, names, literals ----------

#[test]
fn member_access_and_calls() {
    assert_eq!(expr("$a?->b()->c()"), "(call (-> (call (?-> $a b)) c))");
    assert_eq!(expr("Foo::$count++"), "(post++ (:: Foo $count))");
    assert_eq!(
        expr("Status::ACTIVE->value"),
        "(-> (:: Status ACTIVE) value)"
    );
    assert_eq!(expr("Foo::class"), "(:: Foo class)");
    assert_eq!(expr("$obj->default"), "(-> $obj default)");
    assert_eq!(
        expr("parent::__construct($m)"),
        "(call (:: parent __construct) $m)"
    );
    assert_eq!(
        expr("parent(Named)::describe()"),
        "(call (:: (parent Named) describe))"
    );
    assert_eq!(expr("self::MAX"), "(:: self MAX)");
    assert_eq!(expr("new Foo()->bar()"), "(call (-> (new Foo) bar))");
}

#[test]
fn qualified_names_and_keyword_segments() {
    assert_eq!(expr("\\Io\\print('x')"), "(call \\Io\\print \"x\")");
    assert_eq!(expr("App\\Env\\get()"), "(call App\\Env\\get)");
    assert_eq!(expr("__LINE__"), "__LINE__");
}

#[test]
fn arguments() {
    assert_eq!(
        expr("f(1, name: 'x', ...$rest)"),
        "(call f 1 (name: \"x\") (... $rest))"
    );
    assert_eq!(expr("f(default: 1)"), "(call f (default: 1))");
    assert_eq!(
        expr("swap(inout $a, inout $p->x[0])"),
        "(call swap (inout $a) (inout (index (-> $p x) 0)))"
    );
    assert_eq!(expr("\\Str\\normalize(...)"), "(callable \\Str\\normalize)");
    assert_eq!(expr("$obj->method(...)"), "(callable (-> $obj method))");
    assert_eq!(expr("f(1,)"), "(call f 1)");
}

#[test]
fn collection_literals() {
    assert_eq!(expr("[]"), "(list)");
    assert_eq!(expr("[1, 2,]"), "(list 1 2)");
    assert_eq!(
        expr("['a' => 1, 'b' => 2]"),
        "(map (=> \"a\" 1) (=> \"b\" 2))"
    );
}

#[test]
fn literals() {
    assert_eq!(expr("1_000"), "1000");
    assert_eq!(expr("1.5e-3"), "1.5e-3f");
    assert_eq!(expr("true"), "true");
    assert_eq!(expr("null"), "null");
    assert_eq!(expr("b\"a\\xFF\""), "(bytes 97 255)");
}

#[test]
fn interpolation_parses_embedded_expressions() {
    assert_eq!(
        expr("\"a {$u->name} b $x\""),
        "(interp \"a \" (-> $u name) \" b \" $x)"
    );
    assert_eq!(
        expr("\"{$m['k'] ?? 'none'}\""),
        "(interp (?? (index $m \"k\") \"none\"))"
    );
    assert_eq!(errors("$s = \"{$a +}\";"), ["syntax-error"]);
}

#[test]
fn new_and_clone() {
    assert_eq!(expr("new Ref<int>(0)"), "(new (generic Ref int) 0)");
    assert_eq!(
        expr("new \\Task\\Channel<T>(capacity: 0)"),
        "(new (generic \\Task\\Channel T) (capacity: 0))"
    );
    assert_eq!(expr("clone $u"), "(clone $u)");
    assert_eq!(expr("clone($u, name: 'Ken')"), "(clone $u (name: \"Ken\"))");
    assert_eq!(errors("$x = clone($u, 'Ken');"), ["syntax-error"]);
    assert_eq!(errors("$x = new Foo;"), ["syntax-error"]);
}

#[test]
fn closures() {
    assert_eq!(
        expr("fn($x: int): int => $x * 2"),
        "(fn (params (param $x int)) (returns int) (=> (* $x 2)))"
    );
    assert_eq!(
        expr("fn(inout $n: int): void { $n++; }"),
        "(fn (params (param $n inout int)) (returns void) (block (post++ $n)))"
    );
    assert_eq!(errors("$f = fn($x: int) => $x;"), ["missing-return-type"]);
    assert_eq!(errors("$f = fn($x): int => $x;"), ["missing-type"]);
}

// ---------- types ----------

#[test]
fn types() {
    assert_eq!(
        stmts("$m: map<string, list<int>> = [];"),
        "(local $m (generic map string (generic list int)) (list))"
    );
    assert_eq!(
        stmts("$m: list<list<int>>= [];"),
        "(local $m (generic list (generic list int)) (list))"
    );
    assert_eq!(stmts("$x: ?int = null;"), "(local $x (? int) null)");
    assert_eq!(
        stmts("$x: int|string|null = 1;"),
        "(local $x (union int string null) 1)"
    );
    assert_eq!(
        stmts("$f: fn(inout int, string): ?bool = $g;"),
        "(local $f (fn (params (inout int) string) (? bool)) $g)"
    );
    assert_eq!(
        stmts("$t: tuple<string, int> = ['a', 1];"),
        "(local $t (generic tuple string int) (list \"a\" 1))"
    );
}

#[test]
fn removed_types() {
    assert_eq!(errors("$x: mixed = 1;"), ["removed-syntax"]);
    assert_eq!(errors("$x: array = [];"), ["removed-syntax"]);
}

// ---------- statements ----------

#[test]
fn local_declarations() {
    assert_eq!(stmts("readonly $x: int = 1;"), "(readonly-local $x int 1)");
    assert_eq!(stmts("const MAX: int = 10;"), "(const MAX int 10)");
    assert_eq!(errors("$x: int;"), ["syntax-error"]);
}

#[test]
fn destructuring_declarations() {
    assert_eq!(
        stmts("[$a, _] = $pair;"),
        "(destructure (list-pattern (bind $a) _) $pair)"
    );
    assert_eq!(
        stmts("\\App\\Point { x: $x } = $p;"),
        "(destructure (struct-pattern \\App\\Point (x: (bind $x))) $p)"
    );
    // `[…]` not followed by `=` is an expression.
    assert_eq!(stmts("[1, 2] == $x;"), "(== (list 1 2) $x)");
}

#[test]
fn control_flow() {
    assert_eq!(
        stmts("if ($a) { } elseif ($b) { } else { }"),
        "(if $a (block) (elseif $b (block)) (else (block)))"
    );
    assert_eq!(stmts("do { } while ($a);"), "(do-while (block) $a)");
    assert_eq!(
        stmts("for ($i: int = 0; $i < 10; $i++) { }"),
        "(for (local $i int 0) (< $i 10) (exprs (post++ $i)) (block))"
    );
    assert_eq!(
        stmts("for (;;) { break 2; }"),
        "(for _ _ (exprs) (block (break 2)))"
    );
    assert_eq!(
        stmts("foreach ($m as $k => $v) { continue; }"),
        "(foreach $m (key $k) (bind $v) (block (continue)))"
    );
    assert_eq!(
        stmts("foreach ($pairs as [$k, $v]) { }"),
        "(foreach $pairs (list-pattern (bind $k) (bind $v)) (block))"
    );
}

#[test]
fn try_catch_finally() {
    assert_eq!(
        stmts("try { } catch ($e: A | \\B) { } catch (C) { } finally { }"),
        "(try (block) (catch $e (types A \\B) (block)) (catch (types C) (block)) (finally (block)))"
    );
    assert_eq!(errors("try { }"), ["syntax-error"]);
    assert_eq!(errors("try { } catch (Exception $e) { }"), ["syntax-error"]);
}

#[test]
fn echo_print_return() {
    assert_eq!(stmts("echo $a, 'b';"), "(echo $a \"b\")");
    assert_eq!(stmts("print $a;"), "(print $a)");
    assert_eq!(stmts("return;"), "(return)");
    assert_eq!(errors("$x = print 1;"), ["syntax-error"]);
}

// ---------- match and patterns ----------

#[test]
fn match_statement_and_expression() {
    assert_eq!(
        stmts("match ($x) { 1 => { echo 'a'; } default => f(), }"),
        "(match-stmt (subject $x) (arm (lit 1) (=> (block (echo \"a\")))) (arm default (=> (call f))))"
    );
    assert_eq!(
        stmts("match ($x) { default => 1 };"),
        "(match-stmt (subject $x) (arm default (=> 1)))"
    );
    assert_eq!(
        errors("$y: int = match ($x) { default => { return; } };"),
        ["syntax-error"]
    );
}

#[test]
fn patterns() {
    let m = |p: &str| stmts(&format!("match ($x) {{ {p} => 1 }}"));
    assert_eq!(m("_"), "(match-stmt (subject $x) (arm _ (=> 1)))");
    assert_eq!(
        m("-1"),
        "(match-stmt (subject $x) (arm (lit (- 1)) (=> 1)))"
    );
    assert_eq!(
        m("90..=100"),
        "(match-stmt (subject $x) (arm (..= 90 100) (=> 1)))"
    );
    assert_eq!(
        m("-5..<0"),
        "(match-stmt (subject $x) (arm (..< (- 5) 0) (=> 1)))"
    );
    assert_eq!(
        m("0, 1, 2"),
        "(match-stmt (subject $x) (arm (lit 0) (lit 1) (lit 2) (=> 1)))"
    );
    assert_eq!(
        m("_: int"),
        "(match-stmt (subject $x) (arm (typed _ int) (=> 1)))"
    );
    assert_eq!(
        m("MAX"),
        "(match-stmt (subject $x) (arm (path MAX) (=> 1)))"
    );
    assert_eq!(
        m("self::HEARTS"),
        "(match-stmt (subject $x) (arm (path self ::HEARTS) (=> 1)))"
    );
    assert_eq!(
        m("Shape::RECT(_, $h: f64)"),
        "(match-stmt (subject $x) (arm (path Shape ::RECT (payload _ (typed $h f64))) (=> 1)))"
    );
    assert_eq!(
        m("[$a, [_, $b]]"),
        "(match-stmt (subject $x) (arm (list-pattern (bind $a) (list-pattern _ (bind $b))) (=> 1)))"
    );
    assert_eq!(
        m("Point { x: 0, y: $y }"),
        "(match-stmt (subject $x) (arm (struct-pattern Point (x: (lit 0)) (y: (bind $y))) (=> 1)))"
    );
    assert_eq!(errors("match ($x) { 1.5..=2 => 1 }"), ["syntax-error"]);
}

#[test]
fn bare_binding_needs_a_guard() {
    assert_eq!(
        errors("match ($x) { $limit => 1, default => 2 }"),
        ["bare-binding-pattern"]
    );
    assert_eq!(
        errors("match ($x) { $n if $n > 1 => 1, default => 2 }"),
        Vec::<&str>::new()
    );
    // Nested bindings are fine.
    assert_eq!(
        errors("match ($x) { Shape::CIRCLE($r) => 1 }"),
        Vec::<&str>::new()
    );
}

#[test]
fn match_true_with_conditions() {
    assert_eq!(
        errors("$g: string = match (true) { $age < 13 => 'child', default => 'adult' };"),
        ["match-true"]
    );
    // Literal patterns on a `true` subject are still patterns.
    assert_eq!(
        errors("$g: string = match (true) { true => 'y', false => 'n' };"),
        Vec::<&str>::new()
    );
}

#[test]
fn instanceof_targets() {
    assert_eq!(
        expr("$x instanceof string"),
        "(instanceof $x (path string))"
    );
    assert_eq!(
        expr("$x instanceof list<int>"),
        "(instanceof $x (type (generic list int)))"
    );
    assert_eq!(
        expr("$v instanceof \\Json\\Value::STRING($s)"),
        "(instanceof $v (path \\Json\\Value ::STRING (payload (bind $s))))"
    );
    assert_eq!(errors("$x instanceof $cls;"), ["removed-syntax"]);
}

// ---------- declarations ----------

#[test]
fn file_structure() {
    let (dump, errs) = file(
        "namespace App\\Models;\nuse Io\\Writer;\nuse function Str\\normalize as norm;\nuse const Math\\PI;\nuse A\\{B, C as D};\nconst LIMIT: int = 3;\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(
        dump,
        "(file (namespace App\\Models) (use Io\\Writer) (use-function Str\\normalize (as norm)) (use-const Math\\PI) (use A\\B) (use A\\C (as D)) (const LIMIT int 3))"
    );
}

#[test]
fn file_level_errors() {
    assert_eq!(
        file_errors("function main(): void {}"),
        ["missing-namespace"]
    );
    assert_eq!(
        file_errors("namespace A;\n$x: int = 1;\n"),
        ["top-level-statement"]
    );
    assert_eq!(
        file_errors("namespace A;\necho 'hi';\n"),
        ["top-level-statement"]
    );
    assert_eq!(file_errors("namespace \\A;\n"), ["syntax-error"]);
    assert_eq!(
        file_errors("namespace A;\nfunction f(): void {}\nuse B;\n"),
        ["syntax-error"]
    );
    assert_eq!(
        file_errors("namespace A;\nfunction f() {}\n"),
        ["missing-return-type"]
    );
}

#[test]
fn classes_and_members() {
    let (dump, errs) = file(
        "namespace A;\n\
         abstract readonly class Base extends P implements I, J\n{\n\
             public const MAX: int = 3;\n\
             protected static $count: int = 0;\n\
             private readonly $name: string;\n\
             public function __construct(public readonly $id: int, $x: ?string = null) {}\n\
             abstract public function f(): void;\n\
             final public static function g(...$xs: int): never { throw new E(); }\n\
         }\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
    assert!(
        dump.contains("(class Base abstract readonly (extends P) (implements I J)"),
        "{dump}"
    );
    assert!(dump.contains("(const MAX public int 3)"), "{dump}");
    assert!(
        dump.contains("(property $count protected static int (= 0))"),
        "{dump}"
    );
    assert!(
        dump.contains("(property $name private readonly string)"),
        "{dump}"
    );
    assert!(
        dump.contains("(params (param $id public readonly int) (param $x (? string) (= null)))"),
        "{dump}"
    );
    assert!(
        dump.contains("(method f abstract public (params) (returns void) no-body)"),
        "{dump}"
    );
    assert!(
        dump.contains("(method g final public static (params (param $xs variadic int))"),
        "{dump}"
    );
}

#[test]
fn member_errors() {
    let class = |m: &str| file_errors(&format!("namespace A;\nclass C\n{{\n{m}\n}}\n"));
    assert_eq!(class("function f(): void {}"), ["missing-visibility"]);
    assert_eq!(class("$x: int = 1;"), ["missing-visibility"]);
    assert_eq!(class("var $x: int = 1;"), ["removed-syntax"]);
    assert_eq!(class("public $x = 1;"), ["missing-type"]);
    assert_eq!(class("public function f() {}"), ["missing-return-type"]);
    assert_eq!(
        class("public function __construct(): void {}"),
        ["syntax-error"]
    );
    assert_eq!(
        class("static public function f(): void {}"),
        ["syntax-error"]
    );
    assert_eq!(
        class("public function f($x: int, public $y: int): void {}"),
        ["syntax-error"]
    );
    assert_eq!(class("use T;"), ["removed-syntax"]);
    // Recovery: the next member still parses.
    assert_eq!(
        class("public function f(: void {}\npublic $y: = 1;\npublic $z: int = 1;"),
        ["syntax-error", "syntax-error"]
    );
}

#[test]
fn final_class_is_redundant() {
    assert_eq!(
        file_errors("namespace A;\nfinal class C\n{\n}\n"),
        ["redundant-final"]
    );
}

#[test]
fn enums_structs_interfaces() {
    let (dump, errs) = file(
        "namespace A;\n\
         enum Status: string implements HasLabel\n{\n\
             case ACTIVE = 'active';\n\
             case default = 'd';\n\
             public function label(): string { return match ($this) { self::ACTIVE => 'A', default => '?' }; }\n\
         }\n\
         readonly struct P implements Equatable { public function __construct(public $x: f64) {} }\n\
         interface Named extends A, B { public function name(): string; }\n",
    );
    assert!(errs.is_empty(), "{errs:?}");
    assert!(
        dump.contains("(enum Status (backed string) (implements HasLabel)"),
        "{dump}"
    );
    assert!(dump.contains("(case default \"d\")"), "{dump}");
    assert!(
        dump.contains("(struct P readonly (implements Equatable)"),
        "{dump}"
    );
    assert!(dump.contains("(interface Named (extends A B)"), "{dump}");
}

// ---------- out of POC scope ----------

#[test]
fn not_supported_in_poc() {
    let decl = |d: &str| file_errors(&format!("namespace A;\n{d}\n"));
    assert_eq!(decl("type Names = list<string>;"), ["not-supported-in-poc"]);
    assert_eq!(
        decl("function f<T>($x: T): T { return $x; }"),
        ["not-supported-in-poc"]
    );
    assert_eq!(decl("class Box<T> { }"), ["not-supported-in-poc"]);
    assert_eq!(
        decl("function f($x: A&B): void { }"),
        ["not-supported-in-poc"]
    );
    assert_eq!(
        decl("#[Test]\nfunction f(): void { }"),
        ["not-supported-in-poc"]
    );
    assert_eq!(
        decl("class C { public $x: int { get => 1; } }"),
        ["not-supported-in-poc"]
    );
    assert_eq!(
        decl("class C { public private(set) $x: int = 1; }"),
        ["not-supported-in-poc"]
    );
    assert_eq!(
        decl("class C { public function __construct(public private(set) $x: int) {} }"),
        ["not-supported-in-poc"]
    );
    assert_eq!(
        decl("class C { public static function m(): static { return new static(); } }"),
        ["not-supported-in-poc", "not-supported-in-poc"]
    );
    assert_eq!(
        decl("class C { public function j(): string where T of Stringable { return ''; } }"),
        ["not-supported-in-poc"]
    );
    assert_eq!(errors("yield 1;"), ["not-supported-in-poc"]);
    assert_eq!(errors("$s = 'a' |> f(...);"), ["not-supported-in-poc"]);
    assert_eq!(errors("$f = g(?, 1);"), ["not-supported-in-poc"]);
    assert_eq!(errors("$x = static::f();"), ["not-supported-in-poc"]);
    assert_eq!(errors("$x = f::<int>(1);"), ["not-supported-in-poc"]);
}

// ---------- removed PHP syntax ----------

#[test]
fn removed_syntax_is_targeted() {
    for src in [
        "$a = $b === $c;",
        "$a = $b !== $c;",
        "$a = $b and $c;",
        "$a = $b or $c;",
        "$a = $b xor $c;",
        "switch ($x) { case 1: break; }",
        "$f = function ($x) use ($y) { return 1; };",
        "$r = &$x;",
        "foreach ($xs as &$v) { }",
        "if (isset($m['a'])) { }",
        "if (empty($l)) { }",
        "unset($m['a']);",
        "$a = array(1, 2);",
        "if ($x): endif;",
        "if ($x) echo 1;",
        "while ($x) $x--;",
        "if ($x) { } else if ($y) { }",
        "global $config;",
        "static $n = 0;",
        "$o = new $cls();",
        "$v = $o->$name;",
        "$f = static fn(): int => 1;",
        "$f = fn(): int use ($x) => 1;",
        "eval('1');",
        "goto end;",
    ] {
        let errs = errors(src);
        assert!(
            !errs.is_empty() && errs.iter().all(|e| *e == "removed-syntax"),
            "{src}: {errs:?}"
        );
    }
    assert_eq!(errors("$s = $a ?: 'b';"), ["short-ternary"]);
    assert_eq!(file_errors("<?php\nnamespace A;\n"), ["removed-syntax"]);
    assert_eq!(
        file_errors("namespace A;\ndeclare(strict_types=1);\n"),
        ["removed-syntax"]
    );
    assert_eq!(
        file_errors("namespace A;\nfunction f(&$x: int): void { }\n"),
        ["removed-syntax"]
    );
    assert_eq!(
        file_errors("namespace A;\nfunction f(int $x): void { }\n"),
        ["syntax-error"]
    );
}

// ---------- recovery and spans ----------

#[test]
fn recovers_and_reports_every_statement_error() {
    assert_eq!(
        errors("$a = ;\n$b: int = 1;\n$c = );\n$d = 2;"),
        ["syntax-error", "syntax-error"]
    );
    assert_eq!(
        file_errors(
            "namespace A;\nfunction f(): void { $x = ; }\nfunction g(: void {}\nfunction h(): void {}\n"
        ),
        ["syntax-error", "syntax-error"]
    );
}

#[test]
fn spans_cover_the_source() {
    let mut map = SourceMap::new();
    let src = body("$total: int = $a + $b * 2;");
    let id = map
        .add(Path::new("t.scb"), src.clone().into_bytes())
        .unwrap();
    let (ast, diags) = parse_source(id, &map.file(id).text);
    assert!(diags.is_empty());
    let ast = ast.unwrap();
    let crate::ast::Decl::Function(f) = &ast.decls[0] else {
        panic!()
    };
    let s = &f.body.as_ref().unwrap().stmts[0];
    assert_eq!(
        &src[s.span.start as usize..s.span.end as usize],
        "$total: int = $a + $b * 2;"
    );
    let StmtKind::Local { value, .. } = &s.kind else {
        panic!()
    };
    assert_eq!(
        &src[value.span.start as usize..value.span.end as usize],
        "$a + $b * 2"
    );
    assert_eq!(
        &src[f.span.start as usize..f.span.end as usize],
        src[13..].trim_end()
    );
}

#[test]
fn diagnostic_positions() {
    let mut map = SourceMap::new();
    let id = map
        .add(Path::new("t.scb"), body("$x = $a === $b;").into_bytes())
        .unwrap();
    let (_, diags) = parse_source(id, &map.file(id).text);
    assert_eq!(
        diags[0].render(&map),
        "t.scb:4:9: error[removed-syntax]: `===` is removed\n  fix: use `==` (structural equality) or `is` (identity of class instances)"
    );
}

// ---------- review regressions ----------

/// Backtracking in `match (true)` must undo `>>` splits made while trying the arm as
/// a pattern: `A < B >> 1` is first tried as the generic type pattern `A<B>`, which
/// splits `>>`; re-parsed as a condition it must still see `>>`.
#[test]
fn backtracking_restores_split_tokens() {
    assert_eq!(
        errors("$g: string = match (true) { A < B >> 1 => 'x', default => 'y' };"),
        ["match-true"]
    );
}

#[test]
fn namespace_and_local_constants_need_identifier_names() {
    assert_eq!(
        file_errors("namespace A;\nconst null: int = 1;\n"),
        ["syntax-error"]
    );
    assert_eq!(errors("const default: int = 1;"), ["syntax-error"]);
    assert_eq!(
        file_errors("namespace A;\nclass C\n{\npublic const default: int = 1;\n}\n"),
        Vec::<&str>::new()
    );
}

#[test]
fn new_self() {
    assert_eq!(expr("new self(1)"), "(new self 1)");
}
