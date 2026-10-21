//! Types (SPEC "Grammar sketch → Types"). Always postfix: `$x: T`, `): T`.
//!
//! `Type = Intersection { "|" Intersection }`, `Intersection = Primary { "&" Primary }`.
//! Intersections are out of POC scope: they are parsed, reported and reduced to their
//! first member. Generic arguments use `<…>`; a closing `>>` is split by
//! [`Parser::eat_close_angle`].

use super::{PResult, Parser};
use crate::ast::*;
use crate::{Diagnostic, Keyword, Punct, TokenKind};

/// PHP type names with no SCB counterpart (SPEC "Type system", "Removed from PHP").
fn banned_type(name: &str) -> Option<&'static str> {
    match name {
        "mixed" => Some("a union or a tagged enum (e.g. `\\Json\\Value`)"),
        "callable" => Some("a function type such as `fn(int): bool`"),
        "iterable" => Some("`Iterable<T>`"),
        "object" => Some("a class or interface type"),
        "array" => Some("`list<T>`, `map<K, V>`, `set<T>`, `tuple<…>` or a struct"),
        _ => None,
    }
}

impl Parser {
    /// `Type = Intersection { "|" Intersection }`
    pub(crate) fn ty(&mut self) -> PResult<Type> {
        let start = self.span();
        let first = self.intersection()?;
        if !self.at(Punct::Pipe) {
            return Ok(first);
        }
        let mut members = vec![first];
        while self.eat(Punct::Pipe) {
            members.push(self.intersection()?);
        }
        Ok(Type {
            kind: TypeKind::Union(members),
            span: self.span_from(start),
        })
    }

    /// `A & B`; the `&` check skips `&$x` so PHP's by-reference parameter syntax is
    /// left for the parameter parser to report.
    fn intersection(&mut self) -> PResult<Type> {
        let start = self.span();
        let first = self.primary_type()?;
        if self.at(Punct::Amp) && !matches!(self.peek_at(1), TokenKind::Variable(_)) {
            while self.eat(Punct::Amp) {
                self.primary_type()?;
            }
            let span = self.span_from(start);
            self.not_in_poc(span, "intersection types");
        }
        Ok(first)
    }

    /// `?T`, `(T)`, a keyword type, `fn(…): T` or a named type. Recursion (nested `?`,
    /// parentheses, generic and function arguments) passes through here, so this
    /// counts depth.
    fn primary_type(&mut self) -> PResult<Type> {
        self.nested(Self::primary_type_inner)
    }

    fn primary_type_inner(&mut self) -> PResult<Type> {
        let start = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Punct(Punct::Question) => {
                self.bump();
                TypeKind::Nullable(Box::new(self.primary_type()?))
            }
            TokenKind::Punct(Punct::LParen) => {
                self.bump();
                let inner = self.ty()?;
                self.expect(Punct::RParen)?;
                return Ok(inner);
            }
            TokenKind::Keyword(Keyword::Void) => {
                self.bump();
                TypeKind::Void
            }
            TokenKind::Keyword(Keyword::Never) => {
                self.bump();
                TypeKind::Never
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.bump();
                TypeKind::Null
            }
            TokenKind::Keyword(Keyword::SelfKw) => {
                self.bump();
                TypeKind::SelfType
            }
            TokenKind::Keyword(Keyword::Static) => {
                let span = self.bump();
                self.not_in_poc(span, "late static binding (`static` return type)");
                TypeKind::SelfType
            }
            TokenKind::Keyword(Keyword::Fn) => self.fn_type()?,
            TokenKind::Ident(_) | TokenKind::Punct(Punct::Backslash) => {
                TypeKind::Named(self.named_type()?)
            }
            _ => return Err(self.expected("a type")),
        };
        Ok(Type {
            kind,
            span: self.span_from(start),
        })
    }

    /// `fn( [inout] T, … ): R`
    fn fn_type(&mut self) -> PResult<TypeKind> {
        self.expect_kw(Keyword::Fn)?;
        self.expect(Punct::LParen)?;
        let mut params = Vec::new();
        while !self.at(Punct::RParen) {
            let inout = self.eat_kw(Keyword::Inout);
            params.push(FnTypeParam {
                inout,
                ty: self.ty()?,
            });
            if !self.eat(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::RParen)?;
        if !self.at(Punct::Colon) {
            let span = self.span();
            self.report(
                Diagnostic::new("missing-return-type", "a function type needs a return type")
                    .with_span(span)
                    .with_fix("write `fn(…): T`"),
            );
            return Ok(TypeKind::Fn {
                params,
                ret: Box::new(self.void_type()),
            });
        }
        self.bump();
        let ret = Box::new(self.ty()?);
        Ok(TypeKind::Fn { params, ret })
    }

    /// `QualifiedName [ "<" TypeList ">" ]`
    pub(crate) fn named_type(&mut self) -> PResult<NamedType> {
        let start = self.span();
        let name = self.qualified_name(true)?;
        if let [only] = name.parts.as_slice()
            && !name.absolute
            && let Some(fix) = banned_type(&only.name)
        {
            self.report(
                Diagnostic::new(
                    "removed-syntax",
                    format!("the `{}` type is removed", only.name),
                )
                .with_span(only.span)
                .with_fix(format!("use {fix}")),
            );
        }
        let mut args = Vec::new();
        if self.eat(Punct::Lt) {
            loop {
                args.push(self.ty()?);
                if !self.eat(Punct::Comma) {
                    break;
                }
            }
            if !self.eat_close_angle() {
                return Err(self.expected("`>`"));
            }
        }
        Ok(NamedType {
            name,
            args,
            span: self.span_from(start),
        })
    }
}
