//! The typed IR (TIR): the checker's output and the interpreter's input.
//!
//! The TIR is the AST after name resolution and type checking, made explicit so the
//! interpreter never has to think about names or types again:
//!
//! - every expression carries its [`Ty`];
//! - variables are numbered slots in their function's frame ([`LocalId`]); closures
//!   list which outer slots they copy into which of their own;
//! - calls name their target ([`FuncId`] for SCB code, [`Builtin`] for the core
//!   library) and list arguments by parameter index, in the order they were written;
//! - literals have their final width; `echo`, interpolation and `(string)` become
//!   explicit [`ExprKind::ToString`] conversions;
//! - assignment targets are [`Place`]s (a root plus a path), which is what nested
//!   mutation and copy-on-write work on.
//!
//! Spans are kept for runtime error messages and stack traces.

use scb_syntax::Span;

use crate::decls::{CaseIdx, ConstId, DeclId, FieldIdx, StaticId};
use crate::ty::Ty;

/// A function body: free function, method, closure or constant initializer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

/// A slot in a function's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

#[derive(Debug)]
pub struct Func {
    /// Qualified name for stack traces: `App\main`, `App\User::save`, `{closure}`.
    pub name: String,
    /// Owning class, struct or enum for methods.
    pub owner: Option<DeclId>,
    pub params: Vec<Param>,
    /// `$this` in instance methods (and closures that captured it).
    pub this: Option<LocalId>,
    pub ret: Ty,
    /// Every local of the frame: `$this` first in instance methods, then parameters,
    /// then closure captures and declared locals in the order they appear.
    pub locals: Vec<Local>,
    pub body: Body,
    pub span: Span,
}

#[derive(Debug)]
pub enum Body {
    Block(Block),
    /// `fn(…): T => expr`
    Expr(Expr),
    /// Implemented natively by the core library.
    Builtin(Builtin),
    /// Abstract or interface method without a default.
    Abstract,
}

#[derive(Debug)]
pub struct Param {
    pub local: LocalId,
    pub ty: Ty,
    pub inout: bool,
    /// `...$xs: T` collects the remaining positional arguments into `list<T>`.
    pub variadic: bool,
    /// Evaluated at each call that omits the argument.
    pub default: Option<Expr>,
    /// Constructor promotion: the argument is stored into this field.
    pub promote: Option<FieldIdx>,
}

#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
}

/// A native core-library entry point, named `Type.method` or `Ns\function`
/// (`list.push`, `Io\print`, `i8::wrap`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Builtin(pub String);

#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Locals declared directly in this block, in declaration order: their
    /// destructors run in reverse order when the block ends (SPEC "Destructors").
    pub locals: Vec<LocalId>,
}

#[derive(Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum StmtKind {
    Expr(Expr),
    /// A declaration: `$x: T = value;` (also `const` locals).
    Let {
        local: LocalId,
        value: Expr,
    },
    /// `[$a, $b] = value;` / `Point { x: $x } = value;`
    Destructure {
        pattern: Pattern,
        value: Expr,
    },
    Block(Block),
    If {
        cond: Expr,
        then: Block,
        otherwise: Option<Block>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    DoWhile {
        body: Block,
        cond: Expr,
    },
    /// The init declarations live in `scope` (their own block).
    For {
        init: Vec<Stmt>,
        cond: Option<Expr>,
        step: Vec<Expr>,
        body: Block,
        scope: Vec<LocalId>,
    },
    Foreach {
        subject: Expr,
        iter: IterKind,
        key: Option<LocalId>,
        value: Pattern,
        body: Block,
    },
    Try {
        body: Block,
        catches: Vec<Catch>,
        finally: Option<Block>,
    },
    Match(Match),
    /// One `echo` / `print` operand, already converted to `string`.
    Print(Expr),
    Return(Option<Expr>),
    /// `break n;` / `continue n;` with `n ≥ 1` loops.
    Break(u32),
    Continue(u32),
}

/// What `foreach` iterates and what it yields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IterKind {
    /// Elements; the key is the position (`int`).
    List,
    /// Values; the key is the map key.
    Map,
    Set,
    Range,
    /// `u8` values; the key is the position.
    Bytes,
}

#[derive(Debug)]
pub struct Catch {
    pub local: Option<LocalId>,
    /// Throwable classes; a thrown value matches when it is an instance of any.
    pub classes: Vec<DeclId>,
    pub body: Block,
}

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    /// Any integer type; the expression's `ty` gives the width.
    Int(i128),
    /// `f32` literals are stored already rounded to `f32`.
    Float(f64),
    Bool(bool),
    Null,
    Str(String),
    Bytes(Vec<u8>),
}

#[derive(Debug)]
pub enum ExprKind {
    Lit(Lit),
    Local(LocalId),
    /// A namespace or class constant (evaluated once, on first use).
    Const(ConstId),
    /// `C::class`: the fully qualified name, without a leading `\`.
    ClassName(DeclId),
    List(Vec<Expr>),
    Set(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Tuple(Vec<Expr>),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    /// Arithmetic, bitwise, shifts, concatenation and comparisons. Both operands have
    /// the same type except for shifts and `**` on integers (right operand `u32`).
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Short-circuit `&&` / `||`.
    Logic {
        and: bool,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `lhs ?? rhs`: `rhs` is evaluated only when `lhs` is `null`.
    Coalesce {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// `place = value`; the value of the expression is the assigned value.
    Assign {
        place: Place,
        value: Box<Expr>,
    },
    /// `place op= value` (`??=` included: `value` is evaluated only when the place
    /// holds `null`).
    CompoundAssign {
        place: Place,
        op: BinaryOp,
        value: Box<Expr>,
    },
    /// `++` / `--`; the expression's value is the new value (prefix) or the old one.
    IncDec {
        place: Place,
        increment: bool,
        prefix: bool,
    },
    /// A read of a place (local, field, element, static property).
    Read(Place),
    /// A `?->` chain: evaluate `object`; if it is `null` the whole chain is `null`
    /// (SPEC "Evaluation order": `$a?->b()->c()` stops at the first null); otherwise
    /// store it in `local` and evaluate `then`, the rest of the chain.
    Nullsafe {
        object: Box<Expr>,
        local: LocalId,
        then: Box<Expr>,
    },
    /// `->value` of a backed enum case.
    EnumValue(Box<Expr>),
    /// `Enum::from($v)` (traps `ValueError`) / `Enum::tryFrom($v)` (`null`) on a backed
    /// enum.
    EnumFrom {
        decl: DeclId,
        value: Box<Expr>,
        try_: bool,
    },
    /// `Enum::cases()`: every case, in declaration order (pure and backed enums).
    EnumCases(DeclId),
    Call {
        target: CallTarget,
        args: Vec<Arg>,
    },
    /// `new C(…)`; `ctor` is `None` for an implicit constructor.
    New {
        class: DeclId,
        ctor: Option<FuncId>,
        args: Vec<Arg>,
    },
    /// `Enum::CASE` or `Enum::CASE(…)`.
    EnumCase {
        decl: DeclId,
        case: CaseIdx,
        args: Vec<Arg>,
    },
    /// `fn(…) => …`: each capture copies an outer slot (second) into a slot of the
    /// closure's frame (first) when the closure is created.
    Closure {
        func: FuncId,
        captures: Vec<(LocalId, LocalId)>,
    },
    /// `f(...)`, `$obj->m(...)`, `C::m(...)`.
    CallableRef(CallTarget),
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
    },
    InstanceOf {
        expr: Box<Expr>,
        pattern: Box<Pattern>,
    },
    Match(Box<Match>),
    /// A primitive cast `(T) expr` (range-checked at runtime).
    Cast(Box<Expr>),
    /// Conversion to `string` of a `Stringable` value (echo, interpolation, `(string)`).
    ToString(Box<Expr>),
    /// String interpolation: the parts are `string` expressions, concatenated.
    Interpolate(Vec<Expr>),
    Throw(Box<Expr>),
    /// `exit(code)`; type `never`.
    Exit(Option<Box<Expr>>),
    /// `clone $obj` / `clone($obj, field: value, …)`.
    Clone {
        object: Box<Expr>,
        with: Vec<(FieldIdx, Expr)>,
    },
    /// A value converted to a wider union or interface type. Runtime no-op; kept so
    /// that later backends know where boxing happens.
    Upcast(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Plus,
    BitNot,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    /// Float division.
    Div,
    /// Integer division, truncating (`~/`).
    IntDiv,
    Rem,
    Pow,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Concat,
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    /// `<=>` (result `int`).
    Cmp,
    /// `is` (identity of class instances).
    Is,
    /// Only in `CompoundAssign` (`??=`).
    Coalesce,
}

/// Who a call goes to.
#[derive(Debug)]
pub enum CallTarget {
    /// A free function or a static method.
    Func(FuncId),
    /// An instance method of a class, struct, enum or interface, dispatched on the
    /// receiver's runtime class by `name` (`func` is the statically known target; it
    /// is the one called for structs, enums and non-extendable classes).
    Method {
        receiver: Box<Receiver>,
        name: String,
        func: FuncId,
    },
    /// `parent::m(…)`, `parent::__construct(…)`, `parent(Iface)::m(…)`: no dynamic
    /// dispatch; the receiver is `$this` (absent for static methods).
    Parent {
        func: FuncId,
        receiver: Option<Box<Receiver>>,
    },
    /// A core-library function or method on a built-in type.
    Builtin {
        builtin: Builtin,
        receiver: Option<Box<Receiver>>,
    },
    /// Calling a value of function type: `$f(…)`.
    Value(Box<Expr>),
}

/// The receiver of a method call: a plain value, or a place for `mutating` methods
/// (which write back through the place).
#[derive(Debug)]
pub enum Receiver {
    Value(Expr),
    Place(Place),
}

#[derive(Debug)]
pub struct Arg {
    /// Parameter index in the callee's signature.
    pub param: u32,
    pub value: ArgValue,
}

#[derive(Debug)]
pub enum ArgValue {
    Value(Expr),
    Inout(Place),
    /// `...$list` into variadic positions (all remaining arguments).
    Spread(Expr),
}

/// A storage location: a root and a path of projections, e.g. `$order->lines[2]->qty`
/// is `Local($order)` + `[Field(lines), Index(2), Field(qty)]`.
#[derive(Debug)]
pub struct Place {
    pub root: PlaceRoot,
    pub root_ty: Ty,
    pub path: Vec<Proj>,
    /// The type after each projection (`tys[i]` is the type of `root.path[..=i]`):
    /// value-typed levels are the ones copy-on-write makes unique before a write.
    pub tys: Vec<Ty>,
    /// The type of the whole place (`root_ty` when the path is empty).
    pub ty: Ty,
    pub span: Span,
}

#[derive(Debug)]
pub enum PlaceRoot {
    Local(LocalId),
    Static(StaticId),
    /// A class instance produced by an expression (`getUser()->name = 'x'`): writes go
    /// through the reference.
    Object(Box<Expr>),
}

#[derive(Debug)]
pub enum Proj {
    /// A property of a struct or class instance.
    Field(FieldIdx),
    /// `[i]` on a list, map or bytes value.
    Index(Box<Expr>, IndexKind),
    /// `[n]` on a tuple.
    TupleField(u32),
    /// `[]` (only as the last step of an assignment: push).
    Push,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    List,
    Map,
    Bytes,
}

#[derive(Debug)]
pub struct Match {
    /// `None` for the subjectless form (arms are conditions).
    pub subject: Option<Expr>,
    pub arms: Vec<Arm>,
}

#[derive(Debug)]
pub struct Arm {
    /// Empty for `default`; several patterns mean "any of".
    pub patterns: Vec<Pattern>,
    /// Subjectless form: the condition.
    pub condition: Option<Expr>,
    pub guard: Option<Expr>,
    pub body: ArmBody,
    /// Locals bound by this arm's patterns.
    pub locals: Vec<LocalId>,
}

#[derive(Debug)]
pub enum ArmBody {
    Expr(Expr),
    Block(Block),
}

#[derive(Debug)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum PatternKind {
    /// `_`
    Wildcard,
    /// `$x`
    Bind(LocalId),
    /// `$x: T` / `_: T`: matches values of type `ty` (a union member).
    TypeTest { ty: Ty, bind: Option<LocalId> },
    /// A literal or constant compared with `==`.
    Equals(Expr),
    /// An integer range.
    Range {
        start: i128,
        end: i128,
        inclusive: bool,
    },
    /// An enum case, with payload patterns for a data-carrying case.
    Case {
        decl: DeclId,
        case: CaseIdx,
        payload: Vec<Pattern>,
    },
    /// A class instance test (`$x instanceof Foo`).
    Class(DeclId),
    /// `Point { x: p, … }`
    Struct {
        decl: DeclId,
        fields: Vec<(FieldIdx, Pattern)>,
    },
    /// `[p, q]` on a tuple (or a list of exactly that length).
    Tuple(Vec<Pattern>),
}
