//! Abstract syntax tree produced by the parser.
//!
//! The tree mirrors SPEC "Grammar sketch". Syntax that is out of POC scope is
//! diagnosed (`not-supported-in-poc`) and left out of the tree, so later stages never
//! see it. Name resolution and typing happen in `scb-types`.

use crate::Span;

/// An identifier with its span. For variables the name is stored without `$`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ident {
    pub name: Box<str>,
    pub span: Span,
}

/// `Foo`, `\Io\print`, `App\Models\User`.
#[derive(Debug, Clone, PartialEq)]
pub struct QualifiedName {
    /// Written with a leading `\`.
    pub absolute: bool,
    pub parts: Vec<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub namespace: QualifiedName,
    pub uses: Vec<UseDecl>,
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseKind {
    Type,
    Function,
    Const,
}

/// One imported name. A group `use A\{B, C as D};` yields one `UseDecl` per item.
#[derive(Debug, Clone, PartialEq)]
pub struct UseDecl {
    pub kind: UseKind,
    pub name: QualifiedName,
    pub alias: Option<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Function(FunctionDecl),
    Class(ClassDecl),
    Struct(StructDecl),
    Interface(InterfaceDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    Private,
    Internal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Constructor promotion: `public readonly $x: T`.
    pub promote: Option<Promotion>,
    pub inout: bool,
    pub variadic: bool,
    pub name: Ident,
    pub ty: Type,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Promotion {
    pub visibility: Visibility,
    pub readonly: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    /// Final (the default).
    Plain,
    Extendable,
    Abstract,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassDecl {
    pub kind: ClassKind,
    pub readonly: bool,
    pub name: Ident,
    pub extends: Option<NamedType>,
    pub implements: Vec<NamedType>,
    pub members: Vec<Member>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    pub readonly: bool,
    pub name: Ident,
    pub implements: Vec<NamedType>,
    pub members: Vec<Member>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceDecl {
    pub name: Ident,
    pub extends: Vec<NamedType>,
    pub members: Vec<Member>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub name: Ident,
    /// `: int` or `: string` for a backed enum.
    pub backing: Option<Type>,
    pub implements: Vec<NamedType>,
    pub cases: Vec<EnumCase>,
    pub members: Vec<Member>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaseBody {
    /// `case HEARTS;`
    Unit,
    /// `case ACTIVE = 'active';`
    Value(Expr),
    /// `case CIRCLE($r: f64);`
    Data(Vec<Param>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumCase {
    pub name: Ident,
    pub body: CaseBody,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    pub name: Ident,
    pub ty: Type,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Member {
    Property(Property),
    Method(Method),
    Const(ClassConst),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub visibility: Visibility,
    pub is_static: bool,
    pub readonly: bool,
    pub name: Ident,
    pub ty: Type,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Plain,
    Abstract,
    Final,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Method {
    pub kind: MethodKind,
    pub visibility: Visibility,
    pub is_static: bool,
    pub mutating: bool,
    pub name: Ident,
    pub params: Vec<Param>,
    /// `None` only for `__construct` and `__destruct`.
    pub ret: Option<Type>,
    /// `None` for `;` (abstract and interface methods).
    pub body: Option<Block>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassConst {
    pub visibility: Visibility,
    pub name: Ident,
    pub ty: Type,
    pub value: Expr,
    pub span: Span,
}

// ---------- Types ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Named(NamedType),
    /// `?T`
    Nullable(Box<Type>),
    /// `A|B|…`, two or more members.
    Union(Vec<Type>),
    Fn {
        params: Vec<FnTypeParam>,
        ret: Box<Type>,
    },
    /// `self`
    SelfType,
    Void,
    Never,
    Null,
    /// Placeholder after a `missing-type` diagnostic.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamedType {
    pub name: QualifiedName,
    pub args: Vec<Type>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnTypeParam {
    pub inout: bool,
    pub ty: Type,
}

// ---------- Statements ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Block(Block),
    /// `[readonly] $x: T = expr;`
    Local {
        readonly: bool,
        name: Ident,
        ty: Type,
        value: Expr,
    },
    /// `const X: T = expr;`
    Const(ConstDecl),
    /// `[$a, $b] = expr;`, `Point { x: $x } = expr;`
    Destructure {
        pattern: Pattern,
        value: Expr,
    },
    If {
        cond: Expr,
        then: Block,
        elseifs: Vec<(Expr, Block)>,
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
    For {
        init: ForInit,
        cond: Option<Expr>,
        step: Vec<Expr>,
        body: Block,
    },
    Foreach {
        subject: Expr,
        key: Option<Ident>,
        value: Pattern,
        body: Block,
    },
    Try {
        body: Block,
        catches: Vec<Catch>,
        finally: Option<Block>,
    },
    Match(Match),
    Echo(Vec<Expr>),
    Print(Expr),
    /// Bare `exit;` / `die;`. `exit(…)` is an expression.
    Exit,
    Return(Option<Expr>),
    Break(Option<u64>),
    Continue(Option<u64>),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ForInit {
    None,
    /// `$i: int = 0`, scoped to the loop.
    Decl(Box<ForDecl>),
    Exprs(Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForDecl {
    pub name: Ident,
    pub ty: Type,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Catch {
    pub var: Option<Ident>,
    pub types: Vec<NamedType>,
    pub body: Block,
    pub span: Span,
}

// ---------- Expressions ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(u64),
    /// Separator-free literal text; the checker parses it at the target width.
    Float(Box<str>),
    Str(String),
    /// A double-quoted string or heredoc with interpolation.
    Interpolated(Vec<InterpPart>),
    Bytes(Vec<u8>),
    Bool(bool),
    Null,
    Var(Box<str>),
    /// A constant, function or class name; also the left side of `::`.
    Name(QualifiedName),
    /// `self`
    SelfRef,
    /// `parent`, or `parent(Named)` (an explicit interface default).
    Parent(Option<NamedType>),
    /// `(expr)`. Kept so later stages and diagnostics can tell `(a ? b : c)` from `a ? b : c`.
    Paren(Box<Expr>),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `=` (`op: None`) and compound assignments.
    Assign {
        op: Option<BinaryOp>,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Cast {
        ty: Ident,
        expr: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
    },
    InstanceOf {
        expr: Box<Expr>,
        pattern: Box<Pattern>,
    },
    /// `obj->name` / `obj?->name`.
    Prop {
        object: Box<Expr>,
        name: Ident,
        nullsafe: bool,
    },
    /// `Class::NAME` (constant, enum case, static method name, or `class`).
    StaticMember {
        class: Box<Expr>,
        name: Ident,
    },
    /// `Class::$name`.
    StaticProp {
        class: Box<Expr>,
        name: Ident,
    },
    /// `expr[index]`; `expr[]` (push target) has no index.
    Index {
        object: Box<Expr>,
        index: Option<Box<Expr>>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    /// First-class callable `f(...)`.
    CallableRef(Box<Expr>),
    New {
        class: NamedType,
        args: Vec<Arg>,
    },
    /// `clone $x`, or clone-with `clone($x, name: 'Ken')`.
    Clone {
        expr: Box<Expr>,
        with: Vec<Arg>,
    },
    Closure(Box<Closure>),
    Match(Box<Match>),
    /// `exit(…)` / `die(…)`; type `never`.
    Exit(Option<Box<Expr>>),
    Throw(Box<Expr>),
    /// Placeholder after a syntax error.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Text(String),
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Plus,
    BitNot,
    Not,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Pow,
    Mul,
    Div,
    IntDiv,
    Rem,
    Add,
    Sub,
    Shl,
    Shr,
    Concat,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    NotEq,
    Spaceship,
    Is,
    BitAnd,
    BitXor,
    BitOr,
    And,
    Or,
    Coalesce,
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Pow => "**",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::IntDiv => "~/",
            BinaryOp::Rem => "%",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::Concat => ".",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::Eq => "==",
            BinaryOp::NotEq => "!=",
            BinaryOp::Spaceship => "<=>",
            BinaryOp::Is => "is",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitXor => "^",
            BinaryOp::BitOr => "|",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
            BinaryOp::Coalesce => "??",
        }
    }
}

impl UnaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Plus => "+",
            UnaryOp::BitNot => "~",
            UnaryOp::Not => "!",
            UnaryOp::PreInc => "++pre",
            UnaryOp::PreDec => "--pre",
            UnaryOp::PostInc => "post++",
            UnaryOp::PostDec => "post--",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    /// Named argument label.
    pub name: Option<Ident>,
    pub kind: ArgKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArgKind {
    Value(Expr),
    /// `...$list` / `...$map`
    Spread(Expr),
    /// `inout place`
    Inout(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Closure {
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: ClosureBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClosureBody {
    Expr(Expr),
    Block(Block),
}

// ---------- match and patterns ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    /// `None` for the subjectless form.
    pub subject: Option<Expr>,
    pub arms: Vec<Arm>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arm {
    pub test: ArmTest,
    pub guard: Option<Expr>,
    pub body: ArmBody,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArmTest {
    Default,
    /// Arms of a `match` with a subject.
    Patterns(Vec<Pattern>),
    /// Arms of a subjectless `match`: `bool` conditions.
    Conditions(Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArmBody {
    Expr(Expr),
    /// Only in a statement `match`.
    Block(Block),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    /// `_`
    Wildcard,
    /// `$x`
    Binding(Ident),
    /// `$x: T` (binds) or `_: T` (tests only).
    Typed { name: Option<Ident>, ty: Type },
    /// A literal, including a negative number literal.
    Literal(Expr),
    /// `== expr`
    Equals(Expr),
    /// `90..=100`, `80..<90`
    Range {
        start: Expr,
        end: Expr,
        inclusive: bool,
    },
    /// A constant, enum case or type name: `MAX`, `Suit::HEARTS`, `Foo`, `self::A`,
    /// with a payload for a data-carrying case: `Shape::CIRCLE($r)`.
    Path {
        name: QualifiedName,
        member: Option<Ident>,
        payload: Option<Vec<Pattern>>,
    },
    /// A type with generic arguments in `instanceof`: `$x instanceof list<int>`.
    Type(Type),
    /// `Point { x: $x, y: _ }`
    Struct {
        ty: NamedType,
        fields: Vec<(Ident, Pattern)>,
    },
    /// `[$a, [$b, $c]]`
    List(Vec<Pattern>),
}
