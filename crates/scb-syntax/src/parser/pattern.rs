//! `match` and patterns (SPEC "Enums and match", "Destructuring outside match").

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::{Diagnostic, Keyword, Punct, TokenKind};

impl Parser {
    /// `match [ ( subject ) ] { arms }`. In a statement `match`, arms may be blocks.
    pub(crate) fn match_expr(&mut self, is_stmt: bool) -> PResult<Match> {
        let start = self.expect_kw(Keyword::Match)?;
        let subject = if self.eat(Punct::LParen) {
            let e = self.expr()?;
            self.expect(Punct::RParen)?;
            Some(e)
        } else {
            None
        };
        let match_true = matches!(
            subject,
            Some(Expr {
                kind: ExprKind::Bool(true),
                ..
            })
        );
        let mut reported_true = false;
        self.expect(Punct::LBrace)?;
        let mut arms = Vec::new();
        while !self.at(Punct::RBrace) && !self.at_eof() {
            let arm_start = self.span();
            let test = if self.eat_kw(Keyword::Default) {
                ArmTest::Default
            } else if subject.is_none() {
                ArmTest::Conditions(self.conditions()?)
            } else if match_true {
                match self.try_patterns() {
                    Some(ps) => ArmTest::Patterns(ps),
                    None => {
                        if !reported_true {
                            reported_true = true;
                            self.report(
                                Diagnostic::new(
                                    "match-true",
                                    "`match (true)` with condition arms is replaced by the subjectless `match`",
                                )
                                .with_span(start)
                                .with_fix("write `match { cond => …, default => … }`"),
                            );
                        }
                        ArmTest::Conditions(self.conditions()?)
                    }
                }
            } else {
                ArmTest::Patterns(self.patterns()?)
            };
            let guard = if self.eat_kw(Keyword::If) {
                Some(self.expr()?)
            } else {
                None
            };
            if guard.is_none()
                && let ArmTest::Patterns(ps) = &test
            {
                for p in ps {
                    if let PatternKind::Binding(id) = &p.kind {
                        self.report(
                            Diagnostic::new(
                                "bare-binding-pattern",
                                format!(
                                    "`${}` as a pattern matches anything and binds it",
                                    id.name
                                ),
                            )
                            .with_span(p.span)
                            .with_note(
                                "PHP's `match` compared against the variable; SCB patterns bind",
                            )
                            .with_fix(format!(
                                "write `== ${}` to compare, or `default` to match anything",
                                id.name
                            )),
                        );
                    }
                }
            }
            self.expect(Punct::FatArrow)?;
            let body = if self.at(Punct::LBrace) {
                if !is_stmt {
                    let span = self.span();
                    self.report(
                        Diagnostic::new(
                            "syntax-error",
                            "block arms are allowed only in a `match` statement",
                        )
                        .with_span(span)
                        .with_note("a `match` used as a value has expression arms"),
                    );
                }
                ArmBody::Block(self.block()?)
            } else {
                ArmBody::Expr(self.expr()?)
            };
            let block_arm = matches!(body, ArmBody::Block(_));
            arms.push(Arm {
                test,
                guard,
                body,
                span: self.span_from(arm_start),
            });
            if !self.eat(Punct::Comma) && !self.at(Punct::RBrace) && !block_arm {
                return Err(self.expected("`,` or `}`"));
            }
        }
        self.expect(Punct::RBrace)?;
        Ok(Match {
            subject,
            arms,
            span: self.span_from(start),
        })
    }

    fn conditions(&mut self) -> PResult<Vec<Expr>> {
        let mut list = vec![self.expr()?];
        while self.eat(Punct::Comma) {
            list.push(self.expr()?);
        }
        Ok(list)
    }

    fn patterns(&mut self) -> PResult<Vec<Pattern>> {
        let mut list = vec![self.pattern()?];
        while self.eat(Punct::Comma) {
            list.push(self.pattern()?);
        }
        Ok(list)
    }

    /// Patterns for `match (true)`, or `None` (cursor restored) when the arm is not a
    /// pattern list.
    fn try_patterns(&mut self) -> Option<Vec<Pattern>> {
        let (pos, ndiags) = (self.pos, self.diags.len());
        let ok = self
            .patterns()
            .ok()
            .filter(|_| self.at(Punct::FatArrow) || self.at_kw(Keyword::If));
        if ok.is_none() {
            self.pos = pos;
            self.diags.truncate(ndiags);
        }
        ok
    }

    pub(crate) fn pattern(&mut self) -> PResult<Pattern> {
        let start = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Underscore => {
                self.bump();
                if self.eat(Punct::Colon) {
                    PatternKind::Typed {
                        name: None,
                        ty: self.ty()?,
                    }
                } else {
                    PatternKind::Wildcard
                }
            }
            TokenKind::Variable(_) => {
                let name = self.variable("a variable")?;
                if self.eat(Punct::Colon) {
                    PatternKind::Typed {
                        name: Some(name),
                        ty: self.ty()?,
                    }
                } else {
                    PatternKind::Binding(name)
                }
            }
            TokenKind::Punct(Punct::EqEq) => {
                self.bump();
                PatternKind::Equals(self.binary(0)?)
            }
            TokenKind::Punct(Punct::Minus)
                if matches!(self.peek_at(1), TokenKind::Int(_) | TokenKind::Float(_)) =>
            {
                self.number_pattern()?
            }
            TokenKind::Int(_) | TokenKind::Float(_) => self.number_pattern()?,
            TokenKind::Str(_)
            | TokenKind::Bytes(_)
            | TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::Null) => {
                PatternKind::Literal(self.postfix()?)
            }
            TokenKind::Punct(Punct::LBracket) => {
                self.bump();
                let mut items = Vec::new();
                while !self.at(Punct::RBracket) {
                    items.push(self.pattern()?);
                    if !self.eat(Punct::Comma) {
                        break;
                    }
                }
                self.expect(Punct::RBracket)?;
                PatternKind::List(items)
            }
            TokenKind::Ident(_)
            | TokenKind::Punct(Punct::Backslash)
            | TokenKind::Keyword(Keyword::SelfKw) => self.path_pattern()?,
            _ => return Err(self.expected("a pattern")),
        };
        Ok(Pattern {
            kind,
            span: self.span_from(start),
        })
    }

    /// `[-]N`, or an integer range `[-]A..=[-]B` / `[-]A..<[-]B`.
    fn number_pattern(&mut self) -> PResult<PatternKind> {
        let start = self.number_literal()?;
        let inclusive = match self.peek() {
            TokenKind::Punct(Punct::DotDotEq) => true,
            TokenKind::Punct(Punct::DotDotLt) => false,
            _ => return Ok(PatternKind::Literal(start)),
        };
        self.bump();
        let end = self.number_literal()?;
        for e in [&start, &end] {
            if !is_int_literal(e) {
                self.report(
                    Diagnostic::new("syntax-error", "range patterns take integer literals")
                        .with_span(e.span),
                );
            }
        }
        Ok(PatternKind::Range {
            start,
            end,
            inclusive,
        })
    }

    fn number_literal(&mut self) -> PResult<Expr> {
        let start = self.span();
        let neg = self.eat(Punct::Minus);
        let kind = match self.peek().clone() {
            TokenKind::Int(n) => ExprKind::Int(n),
            TokenKind::Float(f) => ExprKind::Float(f),
            _ => return Err(self.expected("a number")),
        };
        let lit_span = self.bump().span;
        let lit = Expr {
            kind,
            span: lit_span,
        };
        if !neg {
            return Ok(lit);
        }
        Ok(Expr {
            kind: ExprKind::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(lit),
            },
            span: self.span_from(start),
        })
    }

    /// `Name`, `Name::CASE`, `Name::CASE(p, …)`, `Name { f: p }`, `Name<T>` (a type).
    fn path_pattern(&mut self) -> PResult<PatternKind> {
        let name = if self.at_kw(Keyword::SelfKw) {
            let span = self.bump().span;
            QualifiedName {
                absolute: false,
                parts: vec![Ident {
                    name: "self".into(),
                    span,
                }],
                span,
            }
        } else {
            self.qualified_name(true)?
        };
        if self.at(Punct::Lt) {
            // A generic type in `instanceof`: re-parse as a type.
            let start = name.span;
            self.eat(Punct::Lt);
            let mut args = Vec::new();
            loop {
                args.push(self.ty()?);
                if !self.eat(Punct::Comma) {
                    break;
                }
            }
            if !self.eat_close_angle() {
                return Err(self.expected("`>`"));
            }
            let span = self.span_from(start);
            return Ok(PatternKind::Type(Type {
                kind: TypeKind::Named(NamedType { name, args, span }),
                span,
            }));
        }
        if self.at(Punct::LBrace) {
            let ty = NamedType {
                span: name.span,
                name,
                args: Vec::new(),
            };
            self.bump();
            let mut fields = Vec::new();
            while !self.at(Punct::RBrace) {
                let field = self.name("a field name")?;
                self.expect(Punct::Colon)?;
                let p = self.pattern()?;
                fields.push((field, p));
                if !self.eat(Punct::Comma) {
                    break;
                }
            }
            self.expect(Punct::RBrace)?;
            return Ok(PatternKind::Struct { ty, fields });
        }
        let member = if self.eat(Punct::ColonColon) {
            Some(self.name("a case or constant name")?)
        } else {
            None
        };
        let payload = if self.at(Punct::LParen) {
            self.bump();
            let mut items = Vec::new();
            while !self.at(Punct::RParen) {
                items.push(self.pattern()?);
                if !self.eat(Punct::Comma) {
                    break;
                }
            }
            self.expect(Punct::RParen)?;
            Some(items)
        } else {
            None
        };
        if member.is_none() && payload.is_some() {
            self.report(
                Diagnostic::new(
                    "syntax-error",
                    "a payload pattern needs an enum case `Enum::CASE(…)`",
                )
                .with_span(name.span),
            );
            return Err(Fail);
        }
        Ok(PatternKind::Path {
            name,
            member,
            payload,
        })
    }
}

fn is_int_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) => true,
        ExprKind::Unary {
            op: UnaryOp::Neg,
            expr,
        } => matches!(expr.kind, ExprKind::Int(_)),
        _ => false,
    }
}
