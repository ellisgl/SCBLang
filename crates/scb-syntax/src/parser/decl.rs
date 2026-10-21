//! Files, `use`, declarations, class bodies and parameters.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::{Diagnostic, Keyword, Punct, TokenKind};

pub(super) fn file(p: &mut Parser) -> File {
    if p.at(Punct::Lt) && p.at_n(1, Punct::Question) && p.at_word_n(2, "php") {
        let start = p.span();
        p.bump();
        p.bump();
        p.bump();
        let span = p.span_from(start);
        p.removed(
            span,
            "`<?php` is removed: files contain only code",
            "delete the opening tag",
        );
    }

    let namespace = if p.at_kw(Keyword::Namespace) {
        p.bump();
        match p.qualified_name(false) {
            Ok(name) => {
                if p.expect(Punct::Semi).is_err() {
                    p.sync_decl();
                }
                name
            }
            Err(Fail) => {
                p.sync_decl();
                placeholder_name(p)
            }
        }
    } else {
        let span = p.span();
        p.report(
            Diagnostic::new(
                "missing-namespace",
                "a file must start with a `namespace` declaration",
            )
            .with_span(span)
            .with_fix("add `namespace App;` (single-file mode accepts any non-reserved namespace)"),
        );
        placeholder_name(p)
    };

    let mut uses = Vec::new();
    let mut decls = Vec::new();
    while !p.at_eof() {
        if p.at_kw(Keyword::Use) {
            if !decls.is_empty() {
                let span = p.span();
                p.report(
                    Diagnostic::new(
                        "syntax-error",
                        "`use` declarations must come before other declarations",
                    )
                    .with_span(span),
                );
            }
            if use_decl(p, &mut uses).is_err() {
                p.sync_decl();
            }
            continue;
        }
        if p.at_kw(Keyword::Namespace) {
            let span = p.span();
            p.report(
                Diagnostic::new(
                    "syntax-error",
                    "a file has exactly one `namespace` declaration",
                )
                .with_span(span),
            );
            p.sync_decl();
            continue;
        }
        match decl(p) {
            Ok(Some(d)) => decls.push(d),
            Ok(None) => {}
            Err(Fail) => p.sync_decl(),
        }
    }
    File {
        namespace,
        uses,
        decls,
    }
}

fn placeholder_name(p: &Parser) -> QualifiedName {
    let span = p.span();
    QualifiedName {
        absolute: false,
        parts: Vec::new(),
        span,
    }
}

/// `use [function|const] A\B [as C];` or `use A\{B, C as D};`
fn use_decl(p: &mut Parser, out: &mut Vec<UseDecl>) -> PResult<()> {
    let start = p.expect_kw(Keyword::Use)?;
    let kind = if p.eat_kw(Keyword::Function) {
        UseKind::Function
    } else if p.eat_kw(Keyword::Const) {
        UseKind::Const
    } else {
        UseKind::Type
    };
    let prefix = p.qualified_name(false)?;
    if p.at(Punct::Backslash) && p.at_n(1, Punct::LBrace) {
        p.bump();
        p.bump();
        loop {
            if p.at(Punct::RBrace) {
                break;
            }
            let item_start = p.span();
            let item = p.qualified_name(false)?;
            let alias = if p.eat_kw(Keyword::As) {
                Some(p.ident("an alias")?)
            } else {
                None
            };
            let mut name = prefix.clone();
            name.parts.extend(item.parts);
            name.span = p.span_from(prefix.span);
            out.push(UseDecl {
                kind,
                name,
                alias,
                span: p.span_from(item_start),
            });
            if !p.eat(Punct::Comma) {
                break;
            }
        }
        p.expect(Punct::RBrace)?;
    } else {
        let alias = if p.eat_kw(Keyword::As) {
            Some(p.ident("an alias")?)
        } else {
            None
        };
        out.push(UseDecl {
            kind,
            name: prefix,
            alias,
            span: p.span_from(start),
        });
    }
    p.expect(Punct::Semi)?;
    Ok(())
}

impl Parser {
    pub(crate) fn at_decl_start(&self) -> bool {
        match self.peek() {
            TokenKind::Keyword(
                Keyword::Function
                | Keyword::Class
                | Keyword::Struct
                | Keyword::Interface
                | Keyword::Enum
                | Keyword::Const
                | Keyword::Abstract
                | Keyword::Extendable
                | Keyword::Readonly
                | Keyword::Final
                | Keyword::Use,
            ) => true,
            TokenKind::AttrOpen => true,
            TokenKind::Ident(w) if &**w == "type" => {
                matches!(self.peek_at(1), TokenKind::Ident(_))
            }
            _ => false,
        }
    }

    /// Skips `#[…]` attributes (out of POC scope), reporting each.
    pub(crate) fn attributes(&mut self) {
        while matches!(self.peek(), TokenKind::AttrOpen) {
            let start = self.span();
            self.bump();
            let mut depth = 1usize;
            while depth > 0 && !self.at_eof() {
                match self.peek() {
                    TokenKind::Punct(Punct::LBracket) | TokenKind::AttrOpen => depth += 1,
                    TokenKind::Punct(Punct::RBracket) => depth -= 1,
                    _ => {}
                }
                self.bump();
            }
            let span = self.span_from(start);
            self.not_in_poc(span, "attributes (`#[…]`)");
        }
    }

    /// Skips a balanced `{ … }` group starting at the current `{`.
    pub(crate) fn skip_braces(&mut self) {
        self.skip_group(Punct::LBrace, Punct::RBrace);
    }

    /// Skips a balanced `( … )` group starting at the current `(`.
    pub(crate) fn skip_parens(&mut self) {
        self.skip_group(Punct::LParen, Punct::RParen);
    }

    fn skip_group(&mut self, open: Punct, close: Punct) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                TokenKind::Eof => return,
                TokenKind::Punct(p) if *p == open => depth += 1,
                TokenKind::Punct(p) if *p == close => {
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// `<T, U of Bound = Default>` on a declaration: user generics are out of POC scope.
    fn type_params(&mut self) -> PResult<()> {
        if !self.at(Punct::Lt) {
            return Ok(());
        }
        let start = self.bump().span;
        loop {
            if self.at_word("in") || self.at_word("out") {
                self.bump();
            }
            self.ident("a type parameter")?;
            if self.at_word("of") {
                self.bump();
                self.ty()?;
            }
            if self.eat(Punct::Eq) {
                self.ty()?;
            }
            if !self.eat(Punct::Comma) {
                break;
            }
        }
        if !self.eat_close_angle() {
            return Err(self.expected("`>`"));
        }
        let span = self.span_from(start);
        self.not_in_poc(span, "user-defined generics");
        Ok(())
    }

    /// `: T`, or a `missing-return-type` diagnostic (SPEC "Grammar sketch → Notes").
    pub(crate) fn return_type(&mut self, what: &str) -> PResult<Option<Type>> {
        if self.eat(Punct::Colon) {
            return Ok(Some(self.ty()?));
        }
        let span = self.span();
        self.report(
            Diagnostic::new("missing-return-type", format!("{what} needs a return type"))
                .with_span(span)
                .with_fix("add `: T` after the parameter list (`: void` if it returns nothing)"),
        );
        Ok(None)
    }

    pub(crate) fn void_type(&self) -> Type {
        Type {
            kind: TypeKind::Void,
            span: self.prev_span(),
        }
    }

    /// `( [Param { , Param } [,]] )`
    pub(crate) fn params(&mut self, promotable: bool) -> PResult<Vec<Param>> {
        self.expect(Punct::LParen)?;
        let mut params = Vec::new();
        while !self.at(Punct::RParen) {
            params.push(self.param(promotable)?);
            if !self.eat(Punct::Comma) {
                break;
            }
        }
        self.expect(Punct::RParen)?;
        Ok(params)
    }

    fn param(&mut self, promotable: bool) -> PResult<Param> {
        self.attributes();
        let start = self.span();
        let mut promote = None;
        if let Some(visibility) = self.visibility() {
            let vis_span = self.bump().span;
            self.set_visibility()?;
            let readonly = self.eat_kw(Keyword::Readonly);
            if !promotable {
                self.report(
                    Diagnostic::new(
                        "syntax-error",
                        "only constructor parameters can be promoted to properties",
                    )
                    .with_span(vis_span),
                );
            }
            promote = Some(Promotion {
                visibility,
                readonly,
            });
        } else if self.at_kw(Keyword::Readonly) {
            let span = self.bump().span;
            self.report(
                Diagnostic::new(
                    "syntax-error",
                    "a promoted parameter needs a visibility before `readonly`",
                )
                .with_span(span),
            );
        }
        let inout = self.eat_kw(Keyword::Inout);
        if self.at(Punct::Amp) {
            let span = self.bump().span;
            self.removed(
                span,
                "`&` references are removed",
                "use an `inout` parameter",
            );
        }
        let variadic = self.eat(Punct::Ellipsis);
        let mut prefix_type = None;
        if matches!(
            self.peek(),
            TokenKind::Ident(_) | TokenKind::Punct(Punct::Question)
        ) && !matches!(self.peek_at(1), TokenKind::Punct(Punct::Colon))
        {
            // PHP's prefix type: `int $x`.
            let ty = self.ty()?;
            self.report(
                Diagnostic::new("syntax-error", "types are written after the name")
                    .with_span(ty.span)
                    .with_fix("write `$name: Type`"),
            );
            prefix_type = Some(ty);
        }
        let name = self.variable("a parameter (`$name: Type`)")?;
        let ty = match prefix_type {
            Some(ty) if !self.at(Punct::Colon) => ty,
            _ => self.required_type(&format!("parameter `${}`", name.name))?,
        };
        let default = if self.eat(Punct::Eq) {
            Some(self.expr()?)
        } else {
            None
        };
        if self.at(Punct::LBrace) {
            let hooks = self.span();
            self.skip_braces();
            let span = self.span_from(hooks);
            self.not_in_poc(span, "property hooks");
        }
        Ok(Param {
            promote,
            inout,
            variadic,
            name,
            ty,
            default,
            span: self.span_from(start),
        })
    }

    /// `: T` where a type is mandatory (parameters, properties, constants). A missing
    /// type is reported and replaced by a placeholder so parsing continues.
    pub(crate) fn required_type(&mut self, what: &str) -> PResult<Type> {
        if self.eat(Punct::Colon) {
            return self.ty();
        }
        let span = self.span();
        self.report(
            Diagnostic::new("missing-type", format!("{what} needs a type"))
                .with_span(span)
                .with_fix("add `: Type` after the name"),
        );
        Ok(Type {
            kind: TypeKind::Error,
            span,
        })
    }

    fn visibility(&self) -> Option<Visibility> {
        match self.peek() {
            TokenKind::Keyword(Keyword::Public) => Some(Visibility::Public),
            TokenKind::Keyword(Keyword::Protected) => Some(Visibility::Protected),
            TokenKind::Keyword(Keyword::Private) => Some(Visibility::Private),
            TokenKind::Keyword(Keyword::Internal) => Some(Visibility::Internal),
            _ => None,
        }
    }

    /// `private(set)` after a visibility: asymmetric visibility is out of POC scope.
    fn set_visibility(&mut self) -> PResult<()> {
        if self.at(Punct::LParen) && self.at_word_n(1, "set") && self.at_n(2, Punct::RParen) {
            let start = self.span();
            self.bump();
            self.bump();
            self.bump();
            let span = self.span_from(start);
            self.not_in_poc(span, "asymmetric visibility");
        } else if self.visibility().is_some()
            && self.at_n(1, Punct::LParen)
            && self.at_word_n(2, "set")
        {
            // `public private(set)`: the second visibility is the set visibility.
            let start = self.bump().span;
            self.expect(Punct::LParen)?;
            self.bump();
            self.expect(Punct::RParen)?;
            let span = self.span_from(start);
            self.not_in_poc(span, "asymmetric visibility");
        }
        Ok(())
    }
}

/// One top-level declaration. `Ok(None)` for syntax that was diagnosed and dropped.
fn decl(p: &mut Parser) -> PResult<Option<Decl>> {
    p.attributes();
    let start = p.span();
    if p.at_word("type") && matches!(p.peek_at(1), TokenKind::Ident(_)) {
        while !p.at(Punct::Semi) && !p.at_eof() {
            p.bump();
        }
        p.bump();
        let span = p.span_from(start);
        p.not_in_poc(span, "type aliases");
        return Ok(None);
    }
    if p.at_word("declare") && p.at_n(1, Punct::LParen) {
        p.bump();
        p.skip_parens();
        p.eat(Punct::Semi);
        let span = p.span_from(start);
        p.removed(
            span,
            "`declare(…)` is removed: SCB is always strictly typed",
            "delete the declaration",
        );
        return Ok(None);
    }
    match p.peek() {
        TokenKind::Keyword(Keyword::Function) => Ok(Some(Decl::Function(function_decl(p)?))),
        TokenKind::Keyword(Keyword::Const) => {
            p.bump();
            Ok(Some(Decl::Const(const_tail(p, start)?)))
        }
        TokenKind::Keyword(Keyword::Interface) => Ok(Some(Decl::Interface(interface_decl(p)?))),
        TokenKind::Keyword(Keyword::Enum) => Ok(Some(Decl::Enum(enum_decl(p)?))),
        TokenKind::Keyword(
            Keyword::Class
            | Keyword::Struct
            | Keyword::Abstract
            | Keyword::Extendable
            | Keyword::Readonly
            | Keyword::Final,
        ) => class_or_struct(p),
        _ if p.at_stmt_start() => {
            let span = p.span();
            p.report(
                Diagnostic::new(
                    "top-level-statement",
                    "files contain declarations only; statements belong in a function",
                )
                .with_span(span)
                .with_fix("move this into `function main(): void { … }`"),
            );
            Err(Fail)
        }
        _ => Err(p.expected("a declaration")),
    }
}

fn function_decl(p: &mut Parser) -> PResult<FunctionDecl> {
    let start = p.expect_kw(Keyword::Function)?;
    if p.at(Punct::Amp) {
        let span = p.bump().span;
        p.removed(span, "`&` references are removed", "return a value");
    }
    let name = p.ident("a function name")?;
    p.type_params()?;
    let params = p.params(false)?;
    let ret = p.return_type(&format!("function `{}`", name.name))?;
    let ret = ret.unwrap_or_else(|| p.void_type());
    let body = p.block()?;
    Ok(FunctionDecl {
        name,
        params,
        ret,
        body,
        span: p.span_from(start),
    })
}

/// After `const`: `NAME: T = expr;`
pub(crate) fn const_tail(p: &mut Parser, start: crate::Span) -> PResult<ConstDecl> {
    let name = p.name("a constant name")?;
    let ty = p.required_type(&format!("constant `{}`", name.name))?;
    p.expect(Punct::Eq)?;
    let value = p.expr()?;
    p.expect(Punct::Semi)?;
    Ok(ConstDecl {
        name,
        ty,
        value,
        span: p.span_from(start),
    })
}

fn class_or_struct(p: &mut Parser) -> PResult<Option<Decl>> {
    let start = p.span();
    let mut kind = ClassKind::Plain;
    let mut readonly = false;
    loop {
        match p.peek() {
            TokenKind::Keyword(Keyword::Abstract) if kind == ClassKind::Plain => {
                p.bump();
                kind = ClassKind::Abstract;
            }
            TokenKind::Keyword(Keyword::Extendable) if kind == ClassKind::Plain => {
                p.bump();
                kind = ClassKind::Extendable;
            }
            TokenKind::Keyword(Keyword::Readonly) if !readonly => {
                p.bump();
                readonly = true;
            }
            TokenKind::Keyword(Keyword::Final) => {
                let span = p.bump().span;
                p.report(
                    Diagnostic::new(
                        "redundant-final",
                        "`final` is not a class modifier: classes are final by default",
                    )
                    .with_span(span)
                    .with_fix("remove `final` (use `extendable` to allow subclasses)"),
                );
            }
            _ => break,
        }
    }
    if p.eat_kw(Keyword::Struct) {
        if kind != ClassKind::Plain {
            p.report(
                Diagnostic::new(
                    "syntax-error",
                    "structs have no inheritance: `abstract` and `extendable` apply to classes",
                )
                .with_span(start),
            );
        }
        let name = p.ident("a struct name")?;
        p.type_params()?;
        if p.at_kw(Keyword::Extends) {
            let span = p.span();
            p.report(
                Diagnostic::new("syntax-error", "structs cannot extend other types")
                    .with_span(span)
                    .with_note("use an interface, or make it a class"),
            );
            return Err(Fail);
        }
        let implements = implements(p)?;
        let (members, _) = body(p, BodyKind::Struct)?;
        return Ok(Some(Decl::Struct(StructDecl {
            readonly,
            name,
            implements,
            members,
            span: p.span_from(start),
        })));
    }
    p.expect_kw(Keyword::Class)?;
    let name = p.ident("a class name")?;
    p.type_params()?;
    let extends = if p.eat_kw(Keyword::Extends) {
        Some(p.named_type()?)
    } else {
        None
    };
    let implements = implements(p)?;
    let (members, _) = body(p, BodyKind::Class)?;
    Ok(Some(Decl::Class(ClassDecl {
        kind,
        readonly,
        name,
        extends,
        implements,
        members,
        span: p.span_from(start),
    })))
}

fn implements(p: &mut Parser) -> PResult<Vec<NamedType>> {
    if p.eat_kw(Keyword::Implements) {
        type_list(p)
    } else {
        Ok(Vec::new())
    }
}

fn type_list(p: &mut Parser) -> PResult<Vec<NamedType>> {
    let mut list = vec![p.named_type()?];
    while p.eat(Punct::Comma) {
        list.push(p.named_type()?);
    }
    Ok(list)
}

fn interface_decl(p: &mut Parser) -> PResult<InterfaceDecl> {
    let start = p.expect_kw(Keyword::Interface)?;
    let name = p.ident("an interface name")?;
    p.type_params()?;
    let extends = if p.eat_kw(Keyword::Extends) {
        type_list(p)?
    } else {
        Vec::new()
    };
    let (members, _) = body(p, BodyKind::Interface)?;
    Ok(InterfaceDecl {
        name,
        extends,
        members,
        span: p.span_from(start),
    })
}

fn enum_decl(p: &mut Parser) -> PResult<EnumDecl> {
    let start = p.expect_kw(Keyword::Enum)?;
    let name = p.ident("an enum name")?;
    p.type_params()?;
    let backing = if p.eat(Punct::Colon) {
        Some(p.ty()?)
    } else {
        None
    };
    let implements = implements(p)?;
    let (members, cases) = body(p, BodyKind::Enum)?;
    Ok(EnumDecl {
        name,
        backing,
        implements,
        cases,
        members,
        span: p.span_from(start),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    Class,
    Struct,
    Interface,
    Enum,
}

/// `{ { Member | EnumCase } }`
fn body(p: &mut Parser, kind: BodyKind) -> PResult<(Vec<Member>, Vec<EnumCase>)> {
    p.expect(Punct::LBrace)?;
    let mut members = Vec::new();
    let mut cases = Vec::new();
    while !p.at(Punct::RBrace) && !p.at_eof() {
        p.attributes();
        if p.at_kw(Keyword::Case) {
            match enum_case(p, kind) {
                Ok(c) => cases.push(c),
                Err(Fail) => sync_member(p),
            }
            continue;
        }
        match member(p) {
            Ok(Some(m)) => members.push(m),
            Ok(None) => {}
            Err(Fail) => sync_member(p),
        }
    }
    p.expect(Punct::RBrace)?;
    Ok((members, cases))
}

/// Skips to the end of a broken member: past a `;` or a `{ … }` body, or up to the
/// closing `}` of the class.
fn sync_member(p: &mut Parser) {
    let start = p.pos;
    loop {
        match p.peek() {
            TokenKind::Eof | TokenKind::Punct(Punct::RBrace) => return,
            TokenKind::Punct(Punct::Semi) => {
                p.bump();
                return;
            }
            TokenKind::Punct(Punct::LBrace) => {
                p.skip_braces();
                return;
            }
            TokenKind::Keyword(
                Keyword::Public
                | Keyword::Protected
                | Keyword::Private
                | Keyword::Internal
                | Keyword::Case
                | Keyword::Abstract
                | Keyword::Final,
            ) if p.pos != start => return,
            _ => {
                p.bump();
            }
        }
    }
}

fn enum_case(p: &mut Parser, kind: BodyKind) -> PResult<EnumCase> {
    let start = p.expect_kw(Keyword::Case)?;
    if kind != BodyKind::Enum {
        p.report(
            Diagnostic::new("syntax-error", "`case` declarations belong in an enum")
                .with_span(start)
                .with_note("`switch` is removed; use `match`"),
        );
        return Err(Fail);
    }
    let name = p.name("a case name")?;
    let body = if p.at(Punct::LParen) {
        CaseBody::Data(p.params(false)?)
    } else if p.eat(Punct::Eq) {
        CaseBody::Value(p.expr()?)
    } else {
        CaseBody::Unit
    };
    p.expect(Punct::Semi)?;
    Ok(EnumCase {
        name,
        body,
        span: p.span_from(start),
    })
}

/// Modifier ranks in the required order (SPEC "Style and naming").
fn modifier_rank(k: Keyword) -> Option<u8> {
    match k {
        Keyword::Abstract | Keyword::Final => Some(0),
        Keyword::Public | Keyword::Protected | Keyword::Private | Keyword::Internal => Some(1),
        Keyword::Static => Some(2),
        Keyword::Readonly => Some(3),
        Keyword::Mutating => Some(4),
        _ => None,
    }
}

fn member(p: &mut Parser) -> PResult<Option<Member>> {
    let start = p.span();
    let mut visibility = None;
    let mut kind = MethodKind::Plain;
    let mut is_static = false;
    let mut readonly = false;
    let mut mutating = false;
    let mut last_rank = None;
    loop {
        if p.at_word("var") && matches!(p.peek_at(1), TokenKind::Variable(_)) {
            let span = p.bump().span;
            p.removed(
                span,
                "`var` is removed: properties declare a visibility",
                "write `public` (or `protected`, `private`, `internal`)",
            );
            visibility = Some(Visibility::Public);
            continue;
        }
        let TokenKind::Keyword(k) = *p.peek() else {
            break;
        };
        let Some(rank) = modifier_rank(k) else {
            break;
        };
        let span = p.span();
        // SPEC "Style and naming" (Q29): out-of-order modifiers are a compile error.
        if last_rank.is_some_and(|r| r >= rank) {
            p.report(
                Diagnostic::new(
                    "syntax-error",
                    format!("modifier `{}` is out of order or repeated", k.as_str()),
                )
                .with_span(span)
                .with_note(
                    "modifier order: `abstract` / `final`, visibility, `static`, `readonly`, `mutating`",
                ),
            );
        }
        last_rank = Some(rank);
        if rank == 1 {
            visibility = p.visibility();
            p.bump();
            p.set_visibility()?;
            continue;
        }
        p.bump();
        match k {
            Keyword::Abstract => kind = MethodKind::Abstract,
            Keyword::Final => kind = MethodKind::Final,
            Keyword::Static => is_static = true,
            Keyword::Readonly => readonly = true,
            _ => mutating = true,
        }
    }

    let needs_visibility = |p: &mut Parser, what: &str| {
        if visibility.is_none() {
            p.report(
                Diagnostic::new(
                    "missing-visibility",
                    format!("{what} needs an explicit visibility"),
                )
                .with_span(start)
                .with_fix("add `public`, `protected`, `private` or `internal`"),
            );
        }
    };

    match p.peek().clone() {
        TokenKind::Keyword(Keyword::Function) => {
            p.bump();
            if p.at(Punct::Amp) {
                let span = p.bump().span;
                p.removed(span, "`&` references are removed", "return a value");
            }
            let name = p.name("a method name")?;
            needs_visibility(p, &format!("method `{}`", name.name));
            if readonly {
                p.report(
                    Diagnostic::new(
                        "syntax-error",
                        "`readonly` applies to properties, not methods",
                    )
                    .with_span(start),
                );
            }
            p.type_params()?;
            let special = matches!(&*name.name, "__construct" | "__destruct");
            let params = p.params(&*name.name == "__construct")?;
            let ret = if special {
                if p.at(Punct::Colon) {
                    let span = p.span();
                    p.report(
                        Diagnostic::new(
                            "syntax-error",
                            format!("`{}` has no return type", name.name),
                        )
                        .with_span(span)
                        .with_fix("remove the return type"),
                    );
                    p.bump();
                    p.ty()?;
                }
                None
            } else {
                let what = format!("method `{}`", name.name);
                Some(p.return_type(&what)?.unwrap_or_else(|| p.void_type()))
            };
            if p.at_word("where") {
                let where_start = p.bump().span;
                while !p.at(Punct::LBrace) && !p.at(Punct::Semi) && !p.at_eof() {
                    p.bump();
                }
                let span = p.span_from(where_start);
                p.not_in_poc(span, "conditional methods (`where`)");
            }
            let body = if p.eat(Punct::Semi) {
                None
            } else {
                Some(p.block()?)
            };
            Ok(Some(Member::Method(Method {
                kind,
                visibility: visibility.unwrap_or(Visibility::Public),
                is_static,
                mutating,
                name,
                params,
                ret,
                body,
                span: p.span_from(start),
            })))
        }
        TokenKind::Keyword(Keyword::Const) => {
            p.bump();
            let c = const_tail(p, start)?;
            needs_visibility(p, &format!("constant `{}`", c.name.name));
            if kind != MethodKind::Plain || is_static || readonly || mutating {
                p.report(
                    Diagnostic::new("syntax-error", "constants take only a visibility")
                        .with_span(start),
                );
            }
            Ok(Some(Member::Const(ClassConst {
                visibility: visibility.unwrap_or(Visibility::Public),
                name: c.name,
                ty: c.ty,
                value: c.value,
                span: c.span,
            })))
        }
        TokenKind::Variable(_) => {
            let name = p.variable("a property name")?;
            needs_visibility(p, &format!("property `${}`", name.name));
            if kind != MethodKind::Plain || mutating {
                p.report(
                    Diagnostic::new(
                        "syntax-error",
                        "`abstract`, `final` and `mutating` apply to methods, not properties",
                    )
                    .with_span(start),
                );
            }
            let ty = p.required_type(&format!("property `${}`", name.name))?;
            let default = if p.eat(Punct::Eq) {
                Some(p.expr()?)
            } else {
                None
            };
            if p.at(Punct::LBrace) {
                let hooks = p.span();
                p.skip_braces();
                let span = p.span_from(hooks);
                p.not_in_poc(span, "property hooks");
            } else {
                p.expect(Punct::Semi)?;
            }
            Ok(Some(Member::Property(Property {
                visibility: visibility.unwrap_or(Visibility::Public),
                is_static,
                readonly,
                name,
                ty,
                default,
                span: p.span_from(start),
            })))
        }
        TokenKind::Keyword(Keyword::Use) => {
            let span = p.span();
            p.report(
                Diagnostic::new("removed-syntax", "traits are removed")
                    .with_span(span)
                    .with_fix("use interface default methods"),
            );
            Err(Fail)
        }
        _ => Err(p.expected("a property, method or constant")),
    }
}
