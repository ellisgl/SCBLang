//! Expressions: Pratt parsing over SPEC "Grammar sketch → Operator precedence".
//!
//! Levels 1–5 (postfix, `**`, prefix operators and casts, `instanceof`, `!`) are
//! recursive-descent functions; levels 6–18 are binary operators handled by
//! [`Parser::binary`]; the ternary, assignment and `throw` sit above them.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::{Diagnostic, Keyword, Punct, StrPart, Token, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Assoc {
    Left,
    Right,
    None,
}

#[derive(Debug, Clone, Copy)]
enum BinOpKind {
    Op(BinaryOp),
    Range { inclusive: bool },
    Pipe,
}

/// Integer cast targets (SPEC "Grammar sketch": `PrimitiveType`).
const CAST_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "int", "float", "string",
];

/// PHP casts with no SCB counterpart.
const REMOVED_CASTS: &[&str] = &[
    "array", "object", "integer", "boolean", "double", "real", "unset", "binary",
];

impl Parser {
    /// A full expression (assignment level).
    pub(crate) fn expr(&mut self) -> PResult<Expr> {
        let start = self.span();
        let lhs = self.ternary()?;
        let Some(op) = self.assign_op() else {
            return Ok(lhs);
        };
        self.bump();
        if !is_place(&lhs) {
            self.report(
                Diagnostic::new("invalid-assignment-target", "this expression cannot be assigned to")
                    .with_span(lhs.span)
                    .with_note("assignable: variables, properties, elements (`$a[$k]`, `$a[]`) and static properties"),
            );
        }
        let value = self.expr()?;
        Ok(Expr {
            kind: ExprKind::Assign {
                op,
                target: Box::new(lhs),
                value: Box::new(value),
            },
            span: self.span_from(start),
        })
    }

    fn assign_op(&self) -> Option<Option<BinaryOp>> {
        let TokenKind::Punct(p) = self.peek() else {
            return None;
        };
        Some(Some(match p {
            Punct::Eq => return Some(None),
            Punct::PlusEq => BinaryOp::Add,
            Punct::MinusEq => BinaryOp::Sub,
            Punct::StarEq => BinaryOp::Mul,
            Punct::SlashEq => BinaryOp::Div,
            Punct::TildeSlashEq => BinaryOp::IntDiv,
            Punct::PercentEq => BinaryOp::Rem,
            Punct::StarStarEq => BinaryOp::Pow,
            Punct::DotEq => BinaryOp::Concat,
            Punct::QuestionQuestionEq => BinaryOp::Coalesce,
            Punct::AmpEq => BinaryOp::BitAnd,
            Punct::PipeEq => BinaryOp::BitOr,
            Punct::CaretEq => BinaryOp::BitXor,
            Punct::ShlEq => BinaryOp::Shl,
            Punct::ShrEq => BinaryOp::Shr,
            _ => return None,
        }))
    }

    /// `cond ? a : b`; nesting needs parentheses; `?:` is removed.
    fn ternary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let cond = self.binary(0)?;
        if !self.at(Punct::Question) {
            return Ok(cond);
        }
        let q = self.bump().span;
        if self.at(Punct::Colon) {
            let span = crate::Span::new(self.file, q.start, self.span().end);
            self.bump();
            self.report(
                Diagnostic::new(
                    "short-ternary",
                    "the short ternary `?:` is removed (it tests truthiness)",
                )
                .with_span(span)
                .with_fix("use `??` for a null default, or a full `cond ? a : b`"),
            );
            self.binary(0)?;
            return Ok(Expr {
                kind: ExprKind::Error,
                span: self.span_from(start),
            });
        }
        let then = self.expr()?;
        self.no_unparenthesized_ternary(&then);
        self.expect(Punct::Colon)?;
        let otherwise = self.binary(0)?;
        let e = Expr {
            kind: ExprKind::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                otherwise: Box::new(otherwise),
            },
            span: self.span_from(start),
        };
        if self.at(Punct::Question) {
            let span = self.span();
            self.report(
                Diagnostic::new("syntax-error", "nested ternaries need parentheses")
                    .with_span(span)
                    .with_fix("parenthesize the inner `? :`, or use `match`"),
            );
            return Err(Fail);
        }
        Ok(e)
    }

    fn no_unparenthesized_ternary(&mut self, e: &Expr) {
        if matches!(e.kind, ExprKind::Ternary { .. }) {
            self.report(
                Diagnostic::new("syntax-error", "nested ternaries need parentheses")
                    .with_span(e.span)
                    .with_fix("parenthesize the inner `? :`, or use `match`"),
            );
        }
    }

    /// The binary operator at the cursor: kind, left binding power, associativity.
    fn binop(&mut self) -> Option<(BinOpKind, u8, Assoc)> {
        use BinaryOp as B;
        let op = |b| BinOpKind::Op(b);
        Some(match self.peek() {
            TokenKind::Punct(p) => match p {
                Punct::QuestionQuestion => (op(B::Coalesce), 4, Assoc::Right),
                Punct::PipePipe => (op(B::Or), 6, Assoc::Left),
                Punct::AmpAmp => (op(B::And), 8, Assoc::Left),
                Punct::Pipe => (op(B::BitOr), 10, Assoc::Left),
                Punct::Caret => (op(B::BitXor), 12, Assoc::Left),
                Punct::Amp => (op(B::BitAnd), 14, Assoc::Left),
                Punct::EqEq | Punct::StrictEq => (op(B::Eq), 16, Assoc::None),
                Punct::NotEq | Punct::StrictNotEq => (op(B::NotEq), 16, Assoc::None),
                Punct::Spaceship => (op(B::Spaceship), 16, Assoc::None),
                Punct::Lt => (op(B::Lt), 18, Assoc::None),
                Punct::Le => (op(B::Le), 18, Assoc::None),
                Punct::Gt => (op(B::Gt), 18, Assoc::None),
                Punct::Ge => (op(B::Ge), 18, Assoc::None),
                Punct::PipeGt => (BinOpKind::Pipe, 20, Assoc::Left),
                Punct::DotDotEq => (BinOpKind::Range { inclusive: true }, 22, Assoc::None),
                Punct::DotDotLt => (BinOpKind::Range { inclusive: false }, 22, Assoc::None),
                Punct::Dot => (op(B::Concat), 24, Assoc::Left),
                Punct::Shl => (op(B::Shl), 26, Assoc::Left),
                Punct::Shr => (op(B::Shr), 26, Assoc::Left),
                Punct::Plus => (op(B::Add), 28, Assoc::Left),
                Punct::Minus => (op(B::Sub), 28, Assoc::Left),
                Punct::Star => (op(B::Mul), 30, Assoc::Left),
                Punct::Slash => (op(B::Div), 30, Assoc::Left),
                Punct::TildeSlash => (op(B::IntDiv), 30, Assoc::Left),
                Punct::Percent => (op(B::Rem), 30, Assoc::Left),
                _ => return None,
            },
            TokenKind::Keyword(Keyword::Is) => (op(B::Is), 16, Assoc::None),
            TokenKind::Ident(w) => match &**w {
                // Removed keyword operators: parsed as their replacement after a diagnostic.
                "and" => (op(B::And), 8, Assoc::Left),
                "or" => (op(B::Or), 6, Assoc::Left),
                "xor" => (op(B::BitXor), 12, Assoc::Left),
                _ => return None,
            },
            _ => return None,
        })
    }

    /// Binary operators with binding power ≥ `min_bp`.
    pub(crate) fn binary(&mut self, min_bp: u8) -> PResult<Expr> {
        let start = self.span();
        let mut lhs = self.not_level()?;
        let mut chained: Option<u8> = None;
        while let Some((kind, lbp, assoc)) = self.binop() {
            if lbp < min_bp {
                break;
            }
            let tok = self.bump();
            if assoc == Assoc::None && chained == Some(lbp) {
                self.report(
                    Diagnostic::new(
                        "syntax-error",
                        format!(
                            "`{}` does not chain with operators of the same level",
                            describe_op(&tok)
                        ),
                    )
                    .with_span(tok.span)
                    .with_fix("add parentheses, or combine comparisons with `&&`"),
                );
            }
            self.removed_operator(&tok);
            let rbp = if assoc == Assoc::Right { lbp } else { lbp + 1 };
            let rhs = self.binary(rbp)?;
            let span = self.span_from(start);
            lhs = match kind {
                BinOpKind::Op(op) => Expr {
                    kind: ExprKind::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    span,
                },
                BinOpKind::Range { inclusive } => Expr {
                    kind: ExprKind::Range {
                        start: Box::new(lhs),
                        end: Box::new(rhs),
                        inclusive,
                    },
                    span,
                },
                BinOpKind::Pipe => {
                    self.not_in_poc(tok.span, "the pipe operator `|>`");
                    Expr {
                        kind: ExprKind::Error,
                        span,
                    }
                }
            };
            chained = (assoc == Assoc::None).then_some(lbp);
        }
        Ok(lhs)
    }

    fn removed_operator(&mut self, tok: &Token) {
        let (msg, fix) = match &tok.kind {
            TokenKind::Punct(Punct::StrictEq) => (
                "`===` is removed",
                "use `==` (structural equality) or `is` (identity of class instances)",
            ),
            TokenKind::Punct(Punct::StrictNotEq) => (
                "`!==` is removed",
                "use `!=` (structural), or `!($a is $b)` for identity",
            ),
            TokenKind::Ident(w) if &**w == "and" => ("the `and` operator is removed", "use `&&`"),
            TokenKind::Ident(w) if &**w == "or" => ("the `or` operator is removed", "use `||`"),
            TokenKind::Ident(w) if &**w == "xor" => (
                "the `xor` operator is removed",
                "use `^` on integers or `!=` on `bool`",
            ),
            _ => return,
        };
        self.removed(tok.span, msg, fix);
    }

    /// Level 5: prefix `!`.
    fn not_level(&mut self) -> PResult<Expr> {
        if self.at(Punct::Bang) {
            let start = self.bump().span;
            let e = self.not_level()?;
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(e),
                },
                span: self.span_from(start),
            });
        }
        self.instanceof_level()
    }

    /// Level 4: `expr instanceof Type-or-pattern` (non-associative).
    fn instanceof_level(&mut self) -> PResult<Expr> {
        let start = self.span();
        let e = self.unary()?;
        if !self.at_kw(Keyword::Instanceof) {
            return Ok(e);
        }
        self.bump();
        if let TokenKind::Variable(_) = self.peek() {
            let span = self.span();
            self.removed(
                span,
                "`instanceof $var` (a class name from a value) is banned",
                "name the type, or use a typed pattern",
            );
            return Err(Fail);
        }
        let pattern = self.pattern()?;
        let e = Expr {
            kind: ExprKind::InstanceOf {
                expr: Box::new(e),
                pattern: Box::new(pattern),
            },
            span: self.span_from(start),
        };
        if self.at_kw(Keyword::Instanceof) {
            let span = self.span();
            self.report(
                Diagnostic::new("syntax-error", "`instanceof` does not chain")
                    .with_span(span)
                    .with_fix("combine tests with `&&`"),
            );
            return Err(Fail);
        }
        Ok(e)
    }

    /// Level 3: unary `-` `+` `~`, prefix `++` `--`, casts. `!` is accepted here too
    /// (`-!$x` is unusual but unambiguous).
    fn unary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let op = match self.peek() {
            TokenKind::Punct(Punct::Minus) => Some(UnaryOp::Neg),
            TokenKind::Punct(Punct::Plus) => Some(UnaryOp::Plus),
            TokenKind::Punct(Punct::Tilde) => Some(UnaryOp::BitNot),
            TokenKind::Punct(Punct::PlusPlus) => Some(UnaryOp::PreInc),
            TokenKind::Punct(Punct::MinusMinus) => Some(UnaryOp::PreDec),
            TokenKind::Punct(Punct::Bang) => return self.not_level(),
            TokenKind::Punct(Punct::Amp) if matches!(self.peek_at(1), TokenKind::Variable(_)) => {
                let span = self.bump().span;
                self.removed(
                    span,
                    "`&` references are removed",
                    "use an `inout` parameter",
                );
                return self.unary();
            }
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let e = self.unary()?;
            return Ok(Expr {
                kind: ExprKind::Unary {
                    op,
                    expr: Box::new(e),
                },
                span: self.span_from(start),
            });
        }
        if let Some(cast) = self.cast()? {
            return Ok(cast);
        }
        self.pow()
    }

    /// `(T) operand` for the primitive cast types; `(bool)` and PHP-only casts are
    /// diagnosed. Any other parenthesized name is an expression (`(MAX) - 1`).
    fn cast(&mut self) -> PResult<Option<Expr>> {
        if !self.at(Punct::LParen) || !self.at_n(2, Punct::RParen) {
            return Ok(None);
        }
        let TokenKind::Ident(name) = self.peek_at(1).clone() else {
            return Ok(None);
        };
        let start = self.span();
        let is_cast = CAST_TYPES.contains(&&*name);
        let is_bool = &*name == "bool";
        let is_removed = REMOVED_CASTS.contains(&&*name);
        if !is_cast && !((is_bool || is_removed) && self.operand_follows(3)) {
            return Ok(None);
        }
        self.bump();
        let ty_span = self.bump().span;
        self.bump();
        let cast_span = self.span_from(start);
        if is_bool {
            self.report(
                Diagnostic::new("bool-cast", "`(bool)` casts are banned")
                    .with_span(cast_span)
                    .with_fix("compare explicitly, e.g. `$x != 0` or `$s != ''`"),
            );
        } else if is_removed {
            self.removed(
                cast_span,
                &format!("the `({name})` cast is removed"),
                "convert explicitly with a method or a primitive cast",
            );
        }
        let e = self.unary()?;
        if !is_cast {
            return Ok(Some(Expr {
                kind: ExprKind::Error,
                span: self.span_from(start),
            }));
        }
        Ok(Some(Expr {
            kind: ExprKind::Cast {
                ty: Ident {
                    name,
                    span: ty_span,
                },
                expr: Box::new(e),
            },
            span: self.span_from(start),
        }))
    }

    /// Whether the token `n` ahead can begin an operand (used to tell `(bool) $x`
    /// from a parenthesized name).
    fn operand_follows(&self, n: usize) -> bool {
        matches!(
            self.peek_at(n),
            TokenKind::Variable(_)
                | TokenKind::Int(_)
                | TokenKind::Float(_)
                | TokenKind::Str(_)
                | TokenKind::Ident(_)
                | TokenKind::Keyword(_)
                | TokenKind::Punct(
                    Punct::LParen | Punct::LBracket | Punct::Bang | Punct::Backslash
                )
        )
    }

    /// Level 2: `**`, right-associative; the exponent may carry a prefix operator.
    fn pow(&mut self) -> PResult<Expr> {
        let start = self.span();
        let base = self.postfix()?;
        if !self.at(Punct::StarStar) {
            return Ok(base);
        }
        self.bump();
        let exp = self.unary()?;
        Ok(Expr {
            kind: ExprKind::Binary {
                op: BinaryOp::Pow,
                lhs: Box::new(base),
                rhs: Box::new(exp),
            },
            span: self.span_from(start),
        })
    }

    /// Level 1: member access, `::`, indexing, calls, postfix `++` / `--`.
    pub(crate) fn postfix(&mut self) -> PResult<Expr> {
        let start = self.span();
        let mut e = self.primary()?;
        loop {
            let kind = match self.peek() {
                TokenKind::Punct(p @ (Punct::Arrow | Punct::QuestionArrow)) => {
                    let nullsafe = *p == Punct::QuestionArrow;
                    self.bump();
                    if let TokenKind::Variable(_) = self.peek() {
                        let span = self.span();
                        self.removed(
                            span,
                            "dynamic member access `->$name` is banned",
                            "name the member, or use a `map`",
                        );
                        return Err(Fail);
                    }
                    let name = self.name("a property or method name")?;
                    ExprKind::Prop {
                        object: Box::new(e),
                        name,
                        nullsafe,
                    }
                }
                TokenKind::Punct(Punct::ColonColon) => {
                    let colons = self.bump().span;
                    if !matches!(
                        e.kind,
                        ExprKind::Name(_) | ExprKind::SelfRef | ExprKind::Parent(_)
                    ) {
                        self.removed(
                            colons,
                            "`::` needs a class name, not a value",
                            "name the class: `Foo::bar()`",
                        );
                        return Err(Fail);
                    }
                    if self.at(Punct::Lt) {
                        self.turbofish()?;
                        continue;
                    }
                    match self.peek() {
                        TokenKind::Variable(_) => {
                            let name = self.variable("a static property")?;
                            ExprKind::StaticProp {
                                class: Box::new(e),
                                name,
                            }
                        }
                        _ => {
                            let name = self.name("a member name after `::`")?;
                            ExprKind::StaticMember {
                                class: Box::new(e),
                                name,
                            }
                        }
                    }
                }
                TokenKind::Punct(Punct::LBracket) => {
                    self.bump();
                    let index = if self.at(Punct::RBracket) {
                        None
                    } else {
                        Some(Box::new(self.expr()?))
                    };
                    self.expect(Punct::RBracket)?;
                    ExprKind::Index {
                        object: Box::new(e),
                        index,
                    }
                }
                TokenKind::Punct(Punct::LParen) => match self.args()? {
                    Args::List(args) => ExprKind::Call {
                        callee: Box::new(e),
                        args,
                    },
                    Args::FirstClass => ExprKind::CallableRef(Box::new(e)),
                },
                TokenKind::Punct(p @ (Punct::PlusPlus | Punct::MinusMinus)) => {
                    let op = if *p == Punct::PlusPlus {
                        UnaryOp::PostInc
                    } else {
                        UnaryOp::PostDec
                    };
                    self.bump();
                    e = Expr {
                        kind: ExprKind::Unary {
                            op,
                            expr: Box::new(e),
                        },
                        span: self.span_from(start),
                    };
                    return Ok(e);
                }
                _ => return Ok(e),
            };
            e = Expr {
                kind,
                span: self.span_from(start),
            };
        }
    }

    /// `::<T>` explicit type arguments: user generics are out of POC scope.
    fn turbofish(&mut self) -> PResult<()> {
        let start = self.expect(Punct::Lt)?;
        loop {
            self.ty()?;
            if !self.eat(Punct::Comma) {
                break;
            }
        }
        if !self.eat_close_angle() {
            return Err(self.expected("`>`"));
        }
        let span = self.span_from(start);
        self.not_in_poc(span, "explicit type arguments (`::<T>`)");
        Ok(())
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Int(n) => {
                self.bump();
                ExprKind::Int(n)
            }
            TokenKind::Float(f) => {
                self.bump();
                ExprKind::Float(f)
            }
            TokenKind::Str(lit) => {
                self.bump();
                self.string(lit)?
            }
            TokenKind::Bytes(b) => {
                self.bump();
                ExprKind::Bytes(b)
            }
            TokenKind::Variable(v) => {
                self.bump();
                ExprKind::Var(v)
            }
            TokenKind::Ident(w) => {
                self.removed_call(&w);
                ExprKind::Name(self.qualified_name(true)?)
            }
            TokenKind::Punct(Punct::Backslash) => ExprKind::Name(self.qualified_name(true)?),
            TokenKind::Punct(Punct::LParen) => {
                self.bump();
                let inner = self.expr()?;
                self.expect(Punct::RParen)?;
                ExprKind::Paren(Box::new(inner))
            }
            TokenKind::Punct(Punct::LBracket) => self.collection_literal()?,
            TokenKind::Underscore => {
                let span = self.span();
                self.report(
                    Diagnostic::new("syntax-error", "`_` is only valid in patterns")
                        .with_span(span),
                );
                return Err(Fail);
            }
            TokenKind::Keyword(k) => self.keyword_primary(k)?,
            _ => return Err(self.expected("an expression")),
        };
        Ok(Expr {
            kind,
            span: self.span_from(start),
        })
    }

    /// Calls to PHP language constructs SCB removed (`isset($x)`, `array(…)`, …).
    fn removed_call(&mut self, word: &str) {
        // SPEC "Built-in statements" (Q27): these words are reserved only when followed by `(`.
        if !self.at_n(1, Punct::LParen) {
            return;
        }
        let fix = match word {
            "isset" => "use `$map->has($k)`, `?->`, `??` or `!= null`",
            "empty" => "use `$list->isEmpty()`, `$s == ''` or `$n == 0`",
            "unset" => "use `$map->remove($k)`",
            "array" => "use a `[…]` literal",
            "list" => "destructure with `[$a, $b] = …`",
            "eval" | "compact" | "extract" => "banned: SCB resolves every name at compile time",
            _ => return,
        };
        let span = self.span();
        self.removed(span, &format!("`{word}()` is removed"), fix);
    }

    fn keyword_primary(&mut self, k: Keyword) -> PResult<ExprKind> {
        let start = self.span();
        Ok(match k {
            Keyword::True | Keyword::False => {
                self.bump();
                ExprKind::Bool(k == Keyword::True)
            }
            Keyword::Null => {
                self.bump();
                ExprKind::Null
            }
            Keyword::SelfKw => {
                self.bump();
                ExprKind::SelfRef
            }
            Keyword::Parent => {
                self.bump();
                if self.eat(Punct::LParen) {
                    let iface = self.named_type()?;
                    self.expect(Punct::RParen)?;
                    ExprKind::Parent(Some(iface))
                } else {
                    ExprKind::Parent(None)
                }
            }
            Keyword::Static => {
                self.bump();
                if self.at_kw(Keyword::Fn) {
                    self.removed(
                        start,
                        "`static fn` is removed: closures capture `$this` only when they use it",
                        "write `fn`",
                    );
                    return self.closure();
                }
                self.not_in_poc(start, "late static binding (`static::`)");
                ExprKind::SelfRef
            }
            Keyword::New => self.new_expr()?,
            Keyword::Clone => self.clone_expr()?,
            Keyword::Fn => self.closure()?,
            Keyword::Function => {
                self.removed(
                    start,
                    "`function (…) use (…)` closures are removed",
                    "write `fn(…): T => expr` or `fn(…): T { … }`",
                );
                return Err(Fail);
            }
            Keyword::Match => ExprKind::Match(Box::new(self.match_expr(false)?)),
            Keyword::Exit | Keyword::Die => {
                self.bump();
                if !self.at(Punct::LParen) {
                    self.report(
                        Diagnostic::new(
                            "syntax-error",
                            format!(
                                "bare `{}` is a statement; in an expression write `{}(…)`",
                                k.as_str(),
                                k.as_str()
                            ),
                        )
                        .with_span(start),
                    );
                    return Err(Fail);
                }
                self.bump();
                let code = if self.at(Punct::RParen) {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                self.expect(Punct::RParen)?;
                ExprKind::Exit(code)
            }
            Keyword::Throw => {
                self.bump();
                ExprKind::Throw(Box::new(self.expr()?))
            }
            Keyword::Yield => {
                self.bump();
                if !matches!(
                    self.peek(),
                    TokenKind::Punct(Punct::Semi | Punct::RParen | Punct::Comma | Punct::RBracket)
                ) {
                    self.expr()?;
                }
                let span = self.span_from(start);
                self.not_in_poc(span, "generators (`yield`)");
                ExprKind::Error
            }
            Keyword::Echo | Keyword::Print => {
                self.report(
                    Diagnostic::new(
                        "syntax-error",
                        format!("`{}` is a statement and has no value", k.as_str()),
                    )
                    .with_span(start),
                );
                return Err(Fail);
            }
            _ => return Err(self.expected("an expression")),
        })
    }

    /// `new Name<Args>(…)`
    fn new_expr(&mut self) -> PResult<ExprKind> {
        let start = self.expect_kw(Keyword::New)?;
        match self.peek() {
            TokenKind::Variable(_) => {
                let span = self.span();
                self.removed(
                    span,
                    "`new $className` is banned: classes are named at compile time",
                    "name the class, or pass a `fn(): T` factory",
                );
                return Err(Fail);
            }
            TokenKind::Keyword(Keyword::Static) => {
                let span = self.bump().span;
                self.not_in_poc(span, "late static binding (`new static`)");
                self.args()?;
                return Ok(ExprKind::Error);
            }
            TokenKind::Keyword(Keyword::Class) => {
                let span = self.span();
                self.report(
                    Diagnostic::new("syntax-error", "anonymous classes are not supported")
                        .with_span(span),
                );
                return Err(Fail);
            }
            _ => {}
        }
        let class = self.named_type()?;
        if !self.at(Punct::LParen) {
            let span = self.span_from(start);
            self.report(
                Diagnostic::new("syntax-error", "`new` needs an argument list")
                    .with_span(span)
                    .with_fix(format!("write `new {}()`", self.text_of_name(&class.name))),
            );
            return Err(Fail);
        }
        let args = match self.args()? {
            Args::List(a) => a,
            Args::FirstClass => {
                self.report(
                    Diagnostic::new("syntax-error", "`new Foo(...)` is not a callable")
                        .with_span(start),
                );
                return Err(Fail);
            }
        };
        Ok(ExprKind::New { class, args })
    }

    pub(crate) fn text_of_name(&self, name: &QualifiedName) -> String {
        let parts: Vec<&str> = name.parts.iter().map(|p| &*p.name).collect();
        format!(
            "{}{}",
            if name.absolute { "\\" } else { "" },
            parts.join("\\")
        )
    }

    /// `clone $x` or clone-with `clone($x, name: 'Ken')`.
    fn clone_expr(&mut self) -> PResult<ExprKind> {
        let start = self.expect_kw(Keyword::Clone)?;
        if !self.at(Punct::LParen) {
            let e = self.unary()?;
            return Ok(ExprKind::Clone {
                expr: Box::new(e),
                with: Vec::new(),
            });
        }
        let Args::List(mut args) = self.args()? else {
            return Err(self.expected("an object to clone"));
        };
        if args.is_empty() || args[0].name.is_some() || !matches!(args[0].kind, ArgKind::Value(_)) {
            self.report(
                Diagnostic::new(
                    "syntax-error",
                    "`clone(…)` takes the object first, then named arguments",
                )
                .with_span(self.span_from(start)),
            );
            return Err(Fail);
        }
        let first = args.remove(0);
        let ArgKind::Value(expr) = first.kind else {
            unreachable!("checked above")
        };
        if let Some(a) = args.iter().find(|a| a.name.is_none()) {
            self.report(
                Diagnostic::new("syntax-error", "clone-with arguments must be named")
                    .with_span(a.span)
                    .with_fix("write `clone($obj, prop: value)`"),
            );
        }
        Ok(ExprKind::Clone {
            expr: Box::new(expr),
            with: args,
        })
    }

    /// `fn(params): T => expr` or `fn(params): T { … }`
    fn closure(&mut self) -> PResult<ExprKind> {
        self.expect_kw(Keyword::Fn)?;
        let params = self.params(false)?;
        let ret = self.return_type("a closure")?;
        let ret = ret.unwrap_or_else(|| self.void_type());
        if self.at_kw(Keyword::Use) {
            let span = self.span();
            self.removed(
                span,
                "`use (…)` capture lists are removed: closures capture by value automatically",
                "delete the `use (…)` clause",
            );
            return Err(Fail);
        }
        let body = if self.eat(Punct::FatArrow) {
            ClosureBody::Expr(self.expr()?)
        } else if self.at(Punct::LBrace) {
            ClosureBody::Block(self.block()?)
        } else {
            return Err(self.expected("`=>` or a block"));
        };
        Ok(ExprKind::Closure(Box::new(Closure { params, ret, body })))
    }

    /// `[a, b]` or `[k => v, …]`.
    fn collection_literal(&mut self) -> PResult<ExprKind> {
        self.expect(Punct::LBracket)?;
        if self.eat(Punct::RBracket) {
            return Ok(ExprKind::List(Vec::new()));
        }
        let first = self.expr()?;
        if self.eat(Punct::FatArrow) {
            let mut entries = vec![(first, self.expr()?)];
            while self.eat(Punct::Comma) {
                if self.at(Punct::RBracket) {
                    break;
                }
                let k = self.expr()?;
                self.expect(Punct::FatArrow)?;
                entries.push((k, self.expr()?));
            }
            self.expect(Punct::RBracket)?;
            return Ok(ExprKind::Map(entries));
        }
        let mut items = vec![first];
        while self.eat(Punct::Comma) {
            if self.at(Punct::RBracket) {
                break;
            }
            items.push(self.expr()?);
        }
        self.expect(Punct::RBracket)?;
        Ok(ExprKind::List(items))
    }

    /// `( [Arg { , Arg } [,]] )`, or `(...)` for a first-class callable.
    pub(crate) fn args(&mut self) -> PResult<Args> {
        self.expect(Punct::LParen)?;
        if self.at(Punct::Ellipsis) && self.at_n(1, Punct::RParen) {
            self.bump();
            self.bump();
            return Ok(Args::FirstClass);
        }
        let mut args = Vec::new();
        while !self.at(Punct::RParen) {
            args.push(self.arg()?);
            if !self.eat(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::RParen)?;
        Ok(Args::List(args))
    }

    fn arg(&mut self) -> PResult<Arg> {
        let start = self.span();
        let name = if matches!(self.peek(), TokenKind::Ident(_) | TokenKind::Keyword(_))
            && self.at_n(1, Punct::Colon)
        {
            let n = self.name("an argument name")?;
            self.bump();
            Some(n)
        } else {
            None
        };
        let kind = if self.eat_kw(Keyword::Inout) {
            ArgKind::Inout(self.postfix()?)
        } else if self.eat(Punct::Ellipsis) {
            ArgKind::Spread(self.expr()?)
        } else if self.at(Punct::Question)
            && matches!(
                self.peek_at(1),
                TokenKind::Punct(Punct::Comma | Punct::RParen)
            )
        {
            let span = self.bump().span;
            self.not_in_poc(span, "partial application (`?` arguments)");
            ArgKind::Value(Expr {
                kind: ExprKind::Error,
                span,
            })
        } else {
            self.no_reference();
            ArgKind::Value(self.expr()?)
        };
        Ok(Arg {
            name,
            kind,
            span: self.span_from(start),
        })
    }

    /// Turns a lexed string literal into a plain or interpolated string expression.
    fn string(&mut self, lit: crate::StrLit) -> PResult<ExprKind> {
        if lit.parts.iter().all(|p| matches!(p, StrPart::Text(_))) {
            let mut s = String::new();
            for part in lit.parts {
                if let StrPart::Text(t) = part {
                    s.push_str(&t);
                }
            }
            return Ok(ExprKind::Str(s));
        }
        let mut parts = Vec::new();
        for part in lit.parts {
            parts.push(match part {
                StrPart::Text(t) => InterpPart::Text(t),
                StrPart::Var { name, span } => InterpPart::Expr(Expr {
                    kind: ExprKind::Var(name),
                    span,
                }),
                StrPart::Expr { tokens, .. } => {
                    let mut sub = Parser::new(self.file, tokens);
                    let e = sub.expr();
                    if e.is_ok() && !sub.at_eof() {
                        sub.expected("`}`");
                    }
                    self.diags.append(&mut sub.diags);
                    InterpPart::Expr(e?)
                }
            });
        }
        Ok(ExprKind::Interpolated(parts))
    }
}

pub(crate) enum Args {
    List(Vec<Arg>),
    FirstClass,
}

fn describe_op(tok: &Token) -> String {
    match &tok.kind {
        TokenKind::Punct(p) => p.as_str().to_string(),
        TokenKind::Keyword(k) => k.as_str().to_string(),
        TokenKind::Ident(w) => w.to_string(),
        _ => "operator".into(),
    }
}

/// Syntactically assignable (SPEC "Nested mutation"); the checker applies the rest.
fn is_place(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Var(_)
            | ExprKind::Prop { .. }
            | ExprKind::Index { .. }
            | ExprKind::StaticProp { .. }
    )
}
