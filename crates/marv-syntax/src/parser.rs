//! Hand-written recursive-descent parser for the M0 subset, with
//! precedence-climbing for binary operators (`spec/02-grammar-and-core-ir.md`
//! §B).
//!
//! The parser is the inverse of [`crate::format_module`] on canonical input. It
//! is also tolerant of well-formed but *non-canonical* drafts (extra spaces,
//! missing parentheses around binary operators, trailing commas, `1_000`-style
//! integers) and normalizes them — that is the point of a single canonical form.
//! Any input it cannot parse yields an [`Err`], which the hybrid
//! [`crate::format`] turns into a whitespace-only fallback.
//!
//! Newlines are significant here: [`Tok::Nl`] separates statements and is *not*
//! skipped while parsing an expression, so the expression parser never crosses a
//! line boundary to grab a following `(`, `.` etc. Structural points that legally
//! span lines (between items, inside blocks) skip `Nl` explicitly.

use crate::ast::*;
use crate::lexer::{lex_spanned, Tok};

/// A parse failure. M0 keeps this deliberately simple (a message); structured,
/// fix-carrying diagnostics are milestone M2 (`spec/03`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
}

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        ParseError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error: {}", self.message)
    }
}

impl std::error::Error for ParseError {}

type PResult<T> = Result<T, ParseError>;

/// Parse a complete module from source text.
pub fn parse(src: &str) -> PResult<Module> {
    Ok(parse_with_spans(src)?.0)
}

/// Parse a complete module, also returning the real source [`ItemSpan`]s of every
/// top-level item (MARV-12). The `Module` itself is identical to [`parse`]'s
/// output — spans live in the side table, so the round-trip property is unaffected.
pub fn parse_with_spans(src: &str) -> PResult<(Module, Vec<ItemSpan>)> {
    let (tokens, spans) = lex_spanned(src).map_err(|e| ParseError::new(e.message))?;
    let mut p = Parser {
        tokens,
        spans,
        pos: 0,
        no_struct: false,
        item_spans: Vec::new(),
        name_hi: 0,
        param_insert: None,
    };
    let module = p.parse_module()?;
    p.expect(Tok::Eof)?;
    Ok((module, p.item_spans))
}

struct Parser {
    tokens: Vec<Tok>,
    /// `(start_byte, end_byte)` of each token, aligned 1:1 with `tokens`.
    spans: Vec<(u32, u32)>,
    pos: usize,
    /// When set, a bare `Name {` is *not* read as a struct literal — the `{`
    /// belongs to an enclosing block. This resolves the classic ambiguity in
    /// `if cond { .. }` / `match scrut { .. }`, where `cond`/`scrut` is an
    /// expression immediately followed by a block brace (`spec/02` §B). The flag
    /// governs only the head expression; it is cleared again inside any
    /// parenthesis, bracket, argument list, or nested block.
    no_struct: bool,
    /// Accumulated per-item source spans (MARV-12), in source order.
    item_spans: Vec<ItemSpan>,
    /// Scratch: end byte of the most recently parsed declaration name, so
    /// `parse_item` can close the header span.
    name_hi: u32,
    /// Scratch: byte offset just inside a `fn`'s parameter `(`, for the
    /// capability-insertion point; reset to `None` per item.
    param_insert: Option<u32>,
}

impl Parser {
    fn peek(&self) -> &Tok {
        // The token stream always ends in `Eof`, so indexing the last element is
        // safe once `pos` runs off the end.
        self.tokens.get(self.pos).unwrap_or(&Tok::Eof)
    }

    /// The byte span of the token at the current position (a zero-width span at
    /// end of input once `pos` runs off the end).
    fn cur_span(&self) -> (u32, u32) {
        self.spans
            .get(self.pos)
            .copied()
            .or_else(|| self.spans.last().copied())
            .unwrap_or((0, 0))
    }

    /// Start byte of the current token.
    fn cur_lo(&self) -> u32 {
        self.cur_span().0
    }

    /// Collect consecutive doc-comment lines (separated by newlines) that precede
    /// the next item, returning their text and consuming them. Leaves the parser
    /// positioned on the first non-doc, non-newline token.
    fn take_docs(&mut self) -> Vec<String> {
        let mut docs = Vec::new();
        loop {
            self.skip_nl();
            if let Tok::Doc(text) = self.peek() {
                docs.push(text.clone());
                self.bump();
            } else {
                break;
            }
        }
        docs
    }

    fn bump(&mut self) -> Tok {
        let tok = self.peek().clone();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == tok {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: Tok) -> PResult<()> {
        if self.peek() == &tok {
            self.pos += 1;
            Ok(())
        } else {
            Err(ParseError::new(format!(
                "expected {:?}, found {:?}",
                tok,
                self.peek()
            )))
        }
    }

    /// Skip a run of significant-but-structural newlines.
    fn skip_nl(&mut self) {
        while self.peek() == &Tok::Nl {
            self.pos += 1;
        }
    }

    fn ident(&mut self) -> PResult<String> {
        match self.bump() {
            Tok::Ident(name) => Ok(name),
            other => Err(ParseError::new(format!(
                "expected identifier, found {other:?}"
            ))),
        }
    }

    // ---- module / items -------------------------------------------------

    fn parse_module(&mut self) -> PResult<Module> {
        self.skip_nl();
        self.expect(Tok::Mod)?;
        let name = self.parse_path()?;

        let mut imports = Vec::new();
        loop {
            self.skip_nl();
            if self.peek() == &Tok::Import {
                imports.push(self.parse_import()?);
            } else {
                break;
            }
        }

        let mut items = Vec::new();
        loop {
            // Doc comments (`///`) bind to the item that follows them.
            let docs = self.take_docs();
            if self.peek() == &Tok::Eof {
                break; // trailing doc comments with no item: tolerated, dropped
            }
            // The header starts at this item's first token (keyword/modifier).
            let header_lo = self.cur_lo();
            self.param_insert = None;
            let item = self.parse_item(docs)?;
            self.item_spans.push(ItemSpan {
                name: item.name().to_string(),
                header: (header_lo, self.name_hi),
                param_insert: self.param_insert,
            });
            items.push(item);
        }

        Ok(Module {
            name,
            imports,
            items,
        })
    }

    /// A dotted path: `a` or `a.b.c`. Stops at the first non-`.` token (a `Nl`
    /// ends the path, since paths never span lines in canonical form).
    fn parse_path(&mut self) -> PResult<Path> {
        let mut parts = vec![self.ident()?];
        while self.peek() == &Tok::Dot {
            self.bump();
            parts.push(self.ident()?);
        }
        Ok(parts)
    }

    fn parse_import(&mut self) -> PResult<Import> {
        self.expect(Tok::Import)?;
        let path = self.parse_path()?;
        let names = if self.eat(&Tok::LParen) {
            let mut names = vec![self.ident()?];
            while self.eat(&Tok::Comma) {
                self.skip_nl();
                if self.peek() == &Tok::RParen {
                    break; // tolerate a trailing comma
                }
                names.push(self.ident()?);
            }
            self.expect(Tok::RParen)?;
            Some(names)
        } else {
            None
        };
        Ok(Import { path, names })
    }

    fn parse_item(&mut self, docs: Vec<String>) -> PResult<Item> {
        match self.peek() {
            Tok::Pure => {
                self.bump();
                Ok(Item::Fn(self.parse_fn(true, docs)?))
            }
            Tok::Fn => Ok(Item::Fn(self.parse_fn(false, docs)?)),
            Tok::Linear => {
                self.bump();
                Ok(Item::Struct(self.parse_struct(true, docs)?))
            }
            Tok::Struct => Ok(Item::Struct(self.parse_struct(false, docs)?)),
            Tok::Enum => Ok(Item::Enum(self.parse_enum(docs)?)),
            Tok::Error => Ok(Item::Error(self.parse_error_decl(docs)?)),
            Tok::Interface => Ok(Item::Interface(self.parse_interface(docs)?)),
            Tok::Impl => Ok(Item::Impl(self.parse_impl(docs)?)),
            other => Err(ParseError::new(format!(
                "expected an item (`fn`, `pure fn`, `struct`, `linear struct`, `enum`, `error`, \
                 `interface`, `impl`), found {other:?}"
            ))),
        }
    }

    /// Parse a generic parameter list `[A, B: Bound, ...]`, or `Vec::new()` when
    /// none is present. The opening `[` must be the very next token. Each entry is
    /// a name with an optional interface bound (`generic = ident , [ ":" , bound
    /// ]`, `spec/02` §B).
    fn parse_generics(&mut self) -> PResult<Vec<Generic>> {
        if !self.eat(&Tok::LBracket) {
            return Ok(Vec::new());
        }
        let mut generics = vec![self.parse_generic()?];
        while self.eat(&Tok::Comma) {
            if self.peek() == &Tok::RBracket {
                break; // tolerate a trailing comma
            }
            generics.push(self.parse_generic()?);
        }
        self.expect(Tok::RBracket)?;
        Ok(generics)
    }

    /// Parse a single generic parameter `ident [ ":" bound ]` (`spec/02` §B
    /// `generic`).
    fn parse_generic(&mut self) -> PResult<Generic> {
        let name = self.ident()?;
        let bound = if self.eat(&Tok::Colon) {
            Some(self.parse_bound()?)
        } else {
            None
        };
        Ok(Generic { name, bound })
    }

    /// Parse an interface bound `path [ "[" type { "," type } "]" ]` (`spec/02`
    /// §B `bound`). The optional bracketed list carries any *extra* type arguments
    /// beyond the constrained parameter.
    fn parse_bound(&mut self) -> PResult<Bound> {
        let path = self.parse_path()?;
        let mut args = Vec::new();
        if self.eat(&Tok::LBracket) {
            args.push(self.parse_type()?);
            while self.eat(&Tok::Comma) {
                if self.peek() == &Tok::RBracket {
                    break; // trailing comma
                }
                args.push(self.parse_type()?);
            }
            self.expect(Tok::RBracket)?;
        }
        Ok(Bound { path, args })
    }

    /// Parse `interface Name[generics] { fn_sig* }` (`spec/02` §B
    /// `interface_decl`). Method signatures have no body and no contracts; each
    /// sits on its own line.
    fn parse_interface(&mut self, docs: Vec<String>) -> PResult<InterfaceDecl> {
        self.expect(Tok::Interface)?;
        let name_hi = self.cur_span().1;
        let name = self.ident()?;
        self.name_hi = name_hi;
        let generics = self.parse_generics()?;
        self.expect(Tok::LBrace)?;

        let mut methods = Vec::new();
        loop {
            let mdocs = self.take_docs();
            if self.peek() == &Tok::RBrace {
                break;
            }
            methods.push(self.parse_fn_sig(mdocs)?);
        }
        self.skip_nl();
        self.expect(Tok::RBrace)?;
        Ok(InterfaceDecl {
            docs,
            name,
            generics,
            methods,
        })
    }

    /// Parse one abstract method signature inside an interface (`spec/02` §B
    /// `fn_sig = "fn" ident [generics] "(" [params] ")" ["->" type]`). No body, no
    /// contracts.
    fn parse_fn_sig(&mut self, docs: Vec<String>) -> PResult<FnSig> {
        self.expect(Tok::Fn)?;
        let name = self.ident()?;
        let generics = self.parse_generics()?;
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        if self.peek() != &Tok::RParen {
            params.push(self.parse_param()?);
            while self.eat(&Tok::Comma) {
                if self.peek() == &Tok::RParen {
                    break; // trailing comma
                }
                params.push(self.parse_param()?);
            }
        }
        self.expect(Tok::RParen)?;
        let ret = if self.eat(&Tok::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        Ok(FnSig {
            docs,
            name,
            generics,
            params,
            ret,
        })
    }

    /// Parse `impl Path[type, ...] { fn_decl* }` (`spec/02` §B `impl_decl`). The
    /// bracketed type list is mandatory (the concrete type(s) implemented for);
    /// methods are full function declarations.
    fn parse_impl(&mut self, docs: Vec<String>) -> PResult<ImplDecl> {
        self.expect(Tok::Impl)?;
        let interface = self.parse_path()?;
        self.name_hi = self.cur_span().0;
        self.expect(Tok::LBracket)?;
        let mut args = vec![self.parse_type()?];
        while self.eat(&Tok::Comma) {
            if self.peek() == &Tok::RBracket {
                break; // trailing comma
            }
            args.push(self.parse_type()?);
        }
        self.expect(Tok::RBracket)?;
        self.expect(Tok::LBrace)?;

        let mut methods = Vec::new();
        loop {
            let mdocs = self.take_docs();
            match self.peek() {
                Tok::RBrace => break,
                Tok::Pure => {
                    self.bump();
                    methods.push(self.parse_fn(true, mdocs)?);
                }
                Tok::Fn => methods.push(self.parse_fn(false, mdocs)?),
                other => {
                    return Err(ParseError::new(format!(
                        "expected a method (`fn` or `pure fn`) or `}}` in `impl`, found {other:?}"
                    )))
                }
            }
        }
        self.skip_nl();
        self.expect(Tok::RBrace)?;
        Ok(ImplDecl {
            docs,
            interface,
            args,
            methods,
        })
    }

    fn parse_enum(&mut self, docs: Vec<String>) -> PResult<EnumDecl> {
        self.expect(Tok::Enum)?;
        let name_hi = self.cur_span().1;
        let name = self.ident()?;
        self.name_hi = name_hi;
        let generics = self.parse_generics()?;
        self.expect(Tok::LBrace)?;
        self.skip_nl();

        let mut variants = Vec::new();
        if self.peek() != &Tok::RBrace {
            variants.push(self.parse_variant()?);
            while self.eat(&Tok::Comma) {
                self.skip_nl();
                if self.peek() == &Tok::RBrace {
                    break; // trailing comma
                }
                variants.push(self.parse_variant()?);
            }
        }
        self.skip_nl();
        self.expect(Tok::RBrace)?;
        Ok(EnumDecl {
            docs,
            name,
            generics,
            variants,
        })
    }

    /// Parse `error Name { Variant, Variant, ... }` (`spec/02` §B `error_decl`).
    /// Variants are bare identifiers (no payload); a trailing comma is tolerated.
    fn parse_error_decl(&mut self, docs: Vec<String>) -> PResult<ErrorDecl> {
        self.expect(Tok::Error)?;
        let name_hi = self.cur_span().1;
        let name = self.ident()?;
        self.name_hi = name_hi;
        self.expect(Tok::LBrace)?;
        self.skip_nl();

        let mut variants = Vec::new();
        if self.peek() != &Tok::RBrace {
            variants.push(self.ident()?);
            while self.eat(&Tok::Comma) {
                self.skip_nl();
                if self.peek() == &Tok::RBrace {
                    break; // trailing comma
                }
                variants.push(self.ident()?);
            }
        }
        self.skip_nl();
        self.expect(Tok::RBrace)?;
        Ok(ErrorDecl {
            docs,
            name,
            variants,
        })
    }

    fn parse_variant(&mut self) -> PResult<Variant> {
        let name = self.ident()?;
        let mut fields = Vec::new();
        if self.eat(&Tok::LParen) {
            fields.push(self.parse_type()?);
            while self.eat(&Tok::Comma) {
                if self.peek() == &Tok::RParen {
                    break; // trailing comma
                }
                fields.push(self.parse_type()?);
            }
            self.expect(Tok::RParen)?;
        }
        Ok(Variant { name, fields })
    }

    fn parse_struct(&mut self, linear: bool, docs: Vec<String>) -> PResult<StructDecl> {
        self.expect(Tok::Struct)?;
        let name_hi = self.cur_span().1;
        let name = self.ident()?;
        self.name_hi = name_hi;
        let generics = self.parse_generics()?;
        self.expect(Tok::LBrace)?;
        self.skip_nl();

        let mut fields = Vec::new();
        if self.peek() != &Tok::RBrace {
            fields.push(self.parse_field()?);
            while self.eat(&Tok::Comma) {
                self.skip_nl();
                if self.peek() == &Tok::RBrace {
                    break; // trailing comma
                }
                fields.push(self.parse_field()?);
            }
        }
        self.skip_nl();
        self.expect(Tok::RBrace)?;
        Ok(StructDecl {
            docs,
            linear,
            name,
            generics,
            fields,
        })
    }

    fn parse_field(&mut self) -> PResult<Field> {
        let name = self.ident()?;
        self.expect(Tok::Colon)?;
        let ty = self.parse_type()?;
        Ok(Field { name, ty })
    }

    fn parse_fn(&mut self, is_pure: bool, docs: Vec<String>) -> PResult<FnDecl> {
        self.expect(Tok::Fn)?;
        let name_hi = self.cur_span().1;
        let name = self.ident()?;
        self.name_hi = name_hi;
        let generics = self.parse_generics()?;
        // Record the byte just inside `(` as the capability-insertion point: a new
        // leading parameter (`fs: Fs, `) lands here for the `MissingCapability` fix.
        self.param_insert = Some(self.cur_span().1);
        self.expect(Tok::LParen)?;

        let mut params = Vec::new();
        if self.peek() != &Tok::RParen {
            params.push(self.parse_param()?);
            while self.eat(&Tok::Comma) {
                if self.peek() == &Tok::RParen {
                    break; // trailing comma
                }
                params.push(self.parse_param()?);
            }
        }
        self.expect(Tok::RParen)?;

        let ret = if self.eat(&Tok::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };

        let (requires, ensures) = self.parse_contracts()?;

        let body = self.parse_block()?;
        Ok(FnDecl {
            docs,
            is_pure,
            name,
            generics,
            params,
            ret,
            requires,
            ensures,
            body,
        })
    }

    /// Parse zero or more `requires`/`ensures` contract clauses (`spec/01` §7)
    /// that sit on their own lines between the signature and the body block.
    /// `requires`/`ensures` are contextual keywords (ordinary identifiers
    /// elsewhere), so a clause is recognized only here, after the signature.
    fn parse_contracts(&mut self) -> PResult<(Vec<Expr>, Vec<Expr>)> {
        let mut requires = Vec::new();
        let mut ensures = Vec::new();
        loop {
            // A clause sits on the next line; peek past the newline without
            // committing unless the line actually opens with a clause keyword.
            let save = self.pos;
            self.skip_nl();
            let is_req = matches!(self.peek(), Tok::Ident(k) if k == "requires");
            let is_ens = matches!(self.peek(), Tok::Ident(k) if k == "ensures");
            if !is_req && !is_ens {
                self.pos = save;
                break;
            }
            self.bump(); // the clause keyword
                         // A contract clause is a head expression: the function body's `{`
                         // follows it, so (like an `if`/`match` head) a bare `Name {` here is
                         // not a struct literal.
            let expr = self.parse_expr_no_struct()?;
            if is_req {
                requires.push(expr);
            } else {
                ensures.push(expr);
            }
        }
        // When contracts are present the body's `{` is on its own next line;
        // consume the separating newline so `parse_block` sees the brace.
        if !requires.is_empty() || !ensures.is_empty() {
            self.skip_nl();
        }
        Ok((requires, ensures))
    }

    fn parse_param(&mut self) -> PResult<Param> {
        let name = self.ident()?;
        self.expect(Tok::Colon)?;
        let ty = self.parse_type()?;
        Ok(Param { name, ty })
    }

    // ---- types ----------------------------------------------------------

    /// Whether the next token can begin a `type` (`spec/02` §B). Used to decide
    /// whether a `!` is `!T` (payload follows) or the bare `!` (`!()`) form.
    fn starts_type(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Amp | Tok::Bang | Tok::Question | Tok::LParen | Tok::LBracket | Tok::Ident(_)
        )
    }

    fn parse_type(&mut self) -> PResult<Type> {
        if self.eat(&Tok::Amp) {
            let mutable = self.eat(&Tok::Mut);
            let inner = self.parse_type_base()?;
            return Ok(Type::Ref {
                mutable,
                inner: Box::new(inner),
            });
        }
        self.parse_type_base()
    }

    fn parse_type_base(&mut self) -> PResult<Type> {
        match self.peek() {
            // `!T` / bare `!` — error union (`spec/02` §B `base_type`). The
            // payload is optional; a `!` with no following type is `!()`.
            Tok::Bang => {
                self.bump();
                let payload = if self.starts_type() {
                    Some(self.parse_type_base()?)
                } else {
                    None
                };
                // `!()` and bare `!` denote the same union-over-unit; canonicalize
                // the explicit-unit spelling to the bare form.
                let payload = match payload {
                    Some(Type::Unit) | None => None,
                    Some(t) => Some(Box::new(t)),
                };
                Ok(Type::ErrorUnion(payload))
            }
            // `?T` — optional sugar (`spec/02` §B `base_type`).
            Tok::Question => {
                self.bump();
                Ok(Type::Optional(Box::new(self.parse_type_base()?)))
            }
            Tok::LParen => {
                self.bump();
                self.expect(Tok::RParen)?;
                Ok(Type::Unit)
            }
            // `[]T` (slice) or `[N]T` (fixed array) — disambiguated on whether an
            // integer length precedes the closing bracket (`spec/02` §B
            // `base_type`).
            Tok::LBracket => {
                self.bump();
                if let Tok::Int(n) = self.peek().clone() {
                    self.bump();
                    if n < 0 {
                        return Err(ParseError::new("array length must be non-negative"));
                    }
                    self.expect(Tok::RBracket)?;
                    let elem = self.parse_type()?;
                    Ok(Type::Array {
                        len: n as u64,
                        elem: Box::new(elem),
                    })
                } else {
                    self.expect(Tok::RBracket)?;
                    let elem = self.parse_type()?;
                    Ok(Type::Slice(Box::new(elem)))
                }
            }
            Tok::Ident(_) => {
                let path = self.parse_path()?;
                // An optional `[T, ...]` makes this a generic application.
                if self.eat(&Tok::LBracket) {
                    let mut args = vec![self.parse_type()?];
                    while self.eat(&Tok::Comma) {
                        if self.peek() == &Tok::RBracket {
                            break; // trailing comma
                        }
                        args.push(self.parse_type()?);
                    }
                    self.expect(Tok::RBracket)?;
                    Ok(Type::Generic { path, args })
                } else {
                    Ok(Type::Named(path))
                }
            }
            other => Err(ParseError::new(format!("expected a type, found {other:?}"))),
        }
    }

    // ---- blocks & statements -------------------------------------------

    fn parse_block(&mut self) -> PResult<Block> {
        self.expect(Tok::LBrace)?;
        // Inside a block the struct-literal/block-brace ambiguity is gone (the
        // brace here can only delimit a struct literal), so re-enable them even
        // when this block is the body of an `if`/`match` whose head suppressed
        // them.
        let saved_no_struct = self.no_struct;
        self.no_struct = false;

        let mut stmts = Vec::new();
        let mut tail = None;

        loop {
            self.skip_nl();
            match self.peek() {
                Tok::RBrace => break,
                Tok::Let => stmts.push(self.parse_let(false)?),
                Tok::Var => stmts.push(self.parse_let(true)?),
                // A loop is a statement (it has no value), so it does not end the
                // block: parsing continues with whatever follows it.
                Tok::While => stmts.push(self.parse_while()?),
                Tok::For => stmts.push(self.parse_for()?),
                Tok::Return => {
                    tail = Some(self.parse_return_tail()?);
                    break;
                }
                Tok::If => {
                    tail = Some(Tail::If(Box::new(self.parse_if()?)));
                    break;
                }
                Tok::Match => {
                    tail = Some(Tail::Match(Box::new(self.parse_match()?)));
                    break;
                }
                _ => {
                    // The line is either an assignment statement (`lvalue = expr`)
                    // or the block's tail expression. They share a leading
                    // expression, so parse it, then disambiguate on a following
                    // `=`: an assignment is a statement (the loop continues); a
                    // bare expression is the tail (the block ends).
                    let expr = self.parse_expr()?;
                    if self.eat(&Tok::Eq) {
                        let value = self.parse_expr()?;
                        let target = expr_to_lvalue(expr)?;
                        stmts.push(Stmt::Assign { target, value });
                    } else {
                        tail = Some(Tail::Expr(expr));
                        break;
                    }
                }
            }
        }

        self.skip_nl();
        self.expect(Tok::RBrace)?;
        self.no_struct = saved_no_struct;
        Ok(Block { stmts, tail })
    }

    fn parse_let(&mut self, is_var: bool) -> PResult<Stmt> {
        self.bump(); // `let` or `var`
        let name = self.ident()?;
        let ty = if self.eat(&Tok::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(Tok::Eq)?;
        let value = self.parse_expr()?;
        Ok(if is_var {
            Stmt::Var { name, ty, value }
        } else {
            Stmt::Let { name, ty, value }
        })
    }

    fn parse_return_tail(&mut self) -> PResult<Tail> {
        self.expect(Tok::Return)?;
        // A bare `return` is followed by the statement separator or the block
        // close; anything else is the returned expression.
        let value = match self.peek() {
            Tok::Nl | Tok::RBrace => None,
            _ => Some(self.parse_expr()?),
        };
        Ok(Tail::Return(value))
    }

    fn parse_if(&mut self) -> PResult<IfExpr> {
        self.expect(Tok::If)?;
        let cond = self.parse_expr_no_struct()?;
        let then = self.parse_block()?;
        // In canonical form `} else` shares a line, so no `Nl` is skipped here.
        let els = if self.eat(&Tok::Else) {
            if self.peek() == &Tok::If {
                Some(Else::If(Box::new(self.parse_if()?)))
            } else {
                Some(Else::Block(self.parse_block()?))
            }
        } else {
            None
        };
        Ok(IfExpr { cond, then, els })
    }

    /// Parse `while cond { invariant expr }* block` (`spec/02` §B `while_stmt`).
    /// The condition and every `invariant` clause are head expressions followed by
    /// a `{` (another clause's expr, or the body brace), so — like an `if` head —
    /// a bare `Name {` in them is not a struct literal. `invariant` is a
    /// contextual keyword (an ordinary identifier elsewhere), recognized only at
    /// the start of a clause line here.
    fn parse_while(&mut self) -> PResult<Stmt> {
        self.expect(Tok::While)?;
        let cond = self.parse_expr_no_struct()?;
        let mut invariants = Vec::new();
        loop {
            // A clause sits on the next line; peek past the newline without
            // committing unless the line actually opens with `invariant`.
            let save = self.pos;
            self.skip_nl();
            if matches!(self.peek(), Tok::Ident(k) if k == "invariant") {
                self.bump();
                invariants.push(self.parse_expr_no_struct()?);
            } else {
                self.pos = save;
                break;
            }
        }
        // With invariants present the body's `{` is on its own next line; consume
        // the separating newline so `parse_block` sees the brace.
        if !invariants.is_empty() {
            self.skip_nl();
        }
        let body = self.parse_block()?;
        Ok(Stmt::While {
            cond,
            invariants,
            body,
        })
    }

    /// Parse `for binder in iter block` (`spec/02` §B `for_stmt`). The iterator
    /// expression is a head expression (the body brace follows it), so struct
    /// literals are suppressed in it like an `if` condition.
    fn parse_for(&mut self) -> PResult<Stmt> {
        self.expect(Tok::For)?;
        let binder = self.ident()?;
        self.expect(Tok::In)?;
        let iter = self.parse_expr_no_struct()?;
        let body = self.parse_block()?;
        Ok(Stmt::For { binder, iter, body })
    }

    fn parse_match(&mut self) -> PResult<MatchExpr> {
        self.expect(Tok::Match)?;
        let scrutinee = self.parse_expr_no_struct()?;
        self.expect(Tok::LBrace)?;

        let mut arms = Vec::new();
        loop {
            self.skip_nl();
            if self.peek() == &Tok::RBrace {
                break;
            }
            arms.push(self.parse_arm()?);
        }
        self.skip_nl();
        self.expect(Tok::RBrace)?;
        Ok(MatchExpr { scrutinee, arms })
    }

    /// Parse one `pattern => (expr | block) ,` arm. The trailing comma is
    /// mandatory in canonical form (`spec/02` §B `arm`), but tolerated absent
    /// before the closing brace so agent drafts still parse.
    fn parse_arm(&mut self) -> PResult<Arm> {
        let pat = self.parse_pattern()?;
        self.expect(Tok::FatArrow)?;
        let body = if self.peek() == &Tok::LBrace {
            ArmBody::Block(self.parse_block()?)
        } else {
            ArmBody::Expr(self.parse_expr()?)
        };
        // Consume the arm separator if present (a `Nl` may sit before it).
        self.skip_nl();
        self.eat(&Tok::Comma);
        Ok(Arm { pat, body })
    }

    fn parse_pattern(&mut self) -> PResult<Pattern> {
        // `_` is a wildcard (it lexes as an ordinary identifier).
        if matches!(self.peek(), Tok::Ident(name) if name == "_") {
            self.bump();
            return Ok(Pattern::Wildcard);
        }
        let path = self.parse_path()?;
        let mut fields = Vec::new();
        if self.eat(&Tok::LParen) {
            fields.push(self.parse_field_pat()?);
            while self.eat(&Tok::Comma) {
                if self.peek() == &Tok::RParen {
                    break; // trailing comma
                }
                fields.push(self.parse_field_pat()?);
            }
            self.expect(Tok::RParen)?;
        }
        Ok(Pattern::Ctor { path, fields })
    }

    fn parse_field_pat(&mut self) -> PResult<FieldPat> {
        let name = self.ident()?;
        if name == "_" {
            Ok(FieldPat::Wildcard)
        } else {
            Ok(FieldPat::Bind(name))
        }
    }

    // ---- expressions (precedence climbing) -----------------------------

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_bin(0)
    }

    /// Parse an expression with struct literals suppressed (the head of an `if`
    /// condition or `match` scrutinee, where a following `{` opens a block).
    fn parse_expr_no_struct(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = true;
        let r = self.parse_expr();
        self.no_struct = saved;
        r
    }

    /// Parse an expression with struct literals re-enabled (inside a paren,
    /// bracket, argument, or field-initializer, the brace ambiguity is gone).
    fn parse_expr_allow_struct(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = false;
        let r = self.parse_expr();
        self.no_struct = saved;
        r
    }

    /// Precedence-climbing binary-operator parser. `min_prec` is the lowest
    /// binding power this call will accept; operators are left-associative.
    fn parse_bin(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.parse_unary()?;
        while let Some(op) = self.peek_binop() {
            let prec = op.precedence();
            if prec < min_prec {
                break;
            }
            self.bump(); // operator
            let rhs = self.parse_bin(prec + 1)?;
            lhs = Expr::Binary(Box::new(lhs), op, Box::new(rhs));
        }
        Ok(lhs)
    }

    /// Parse a prefix `unary` (`spec/02` §B `unary = [ "not" | "-" | "&" | "&mut"
    /// ] , postfix`). Unary binds tighter than every binary operator. The grammar
    /// admits a single optional prefix; the parser is right-recursive (the operand
    /// is itself a `unary`), so a stacked draft like `not not p` or `- -x` still
    /// parses — and the canonical formatter re-emits it bijectively. `&`/`&mut`
    /// here are the expression reference-of operators; the type prefixes `&T`/`&mut
    /// T` are a different position handled by [`Self::parse_type`].
    fn parse_unary(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Not => {
                self.bump();
                Some(UnOp::Not)
            }
            Tok::Minus => {
                self.bump();
                Some(UnOp::Neg)
            }
            Tok::Amp => {
                self.bump();
                if self.eat(&Tok::Mut) {
                    Some(UnOp::RefMut)
                } else {
                    Some(UnOp::Ref)
                }
            }
            _ => None,
        };
        match op {
            Some(op) => {
                let operand = self.parse_unary()?;
                Ok(Expr::Unary(op, Box::new(operand)))
            }
            None => self.parse_postfix(),
        }
    }

    /// Peek a binary operator without skipping `Nl` — a newline ends the
    /// expression rather than continuing it onto the next line.
    fn peek_binop(&self) -> Option<BinOp> {
        Some(match self.peek() {
            Tok::Plus => BinOp::Add,
            Tok::Minus => BinOp::Sub,
            Tok::Star => BinOp::Mul,
            Tok::Slash => BinOp::Div,
            Tok::Percent => BinOp::Rem,
            Tok::EqEq => BinOp::Eq,
            Tok::BangEq => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            Tok::And => BinOp::And,
            Tok::Or => BinOp::Or,
            _ => return None,
        })
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                Tok::Dot => {
                    self.bump();
                    let name = self.ident()?;
                    expr = Expr::Field(Box::new(expr), name);
                }
                Tok::LParen => {
                    self.bump();
                    let args = self.parse_args()?;
                    self.expect(Tok::RParen)?;
                    expr = Expr::Call(Box::new(expr), args);
                }
                Tok::LBracket => {
                    self.bump();
                    // Inside the brackets struct literals are unambiguous again.
                    let index = self.parse_expr_allow_struct()?;
                    self.expect(Tok::RBracket)?;
                    expr = Expr::Index(Box::new(expr), Box::new(index));
                }
                // Postfix `?` — error propagation (`spec/02` §B `postfix`).
                Tok::Question => {
                    self.bump();
                    expr = Expr::Try(Box::new(expr));
                }
                // Postfix `as Type` — explicit scalar cast (`spec/02` §B
                // `postfix`). The cast target is a `base_type`, so it never
                // crosses into a following block brace.
                Tok::As => {
                    self.bump();
                    let ty = self.parse_type()?;
                    expr = Expr::Cast(Box::new(expr), ty);
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_args(&mut self) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        if self.peek() == &Tok::RParen {
            return Ok(args);
        }
        // Argument expressions sit inside parentheses, so struct literals are
        // unambiguous here even when the call is the head of an `if`/`match`.
        args.push(self.parse_expr_allow_struct()?);
        while self.eat(&Tok::Comma) {
            if self.peek() == &Tok::RParen {
                break; // trailing comma
            }
            args.push(self.parse_expr_allow_struct()?);
        }
        Ok(args)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                Ok(Expr::Int(n))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Expr::Str(s))
            }
            Tok::Char(c) => {
                self.bump();
                Ok(Expr::Char(c))
            }
            Tok::True => {
                self.bump();
                Ok(Expr::Bool(true))
            }
            Tok::False => {
                self.bump();
                Ok(Expr::Bool(false))
            }
            Tok::Ident(name) => {
                self.bump();
                // `Name { field: expr, ... }` is a struct literal — but only when
                // struct literals are not suppressed (an `if`/`match` head) and a
                // `{` immediately follows on the same line.
                if !self.no_struct && self.peek() == &Tok::LBrace {
                    self.parse_struct_literal(vec![name])
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Tok::LParen => {
                self.bump();
                if self.eat(&Tok::RParen) {
                    Ok(Expr::Unit)
                } else {
                    // Inside parentheses the brace ambiguity is gone.
                    let inner = self.parse_expr_allow_struct()?;
                    self.expect(Tok::RParen)?;
                    Ok(inner)
                }
            }
            // `[e0, e1, ...]` — an array literal (`spec/02` §B `primary`). A `[`
            // at the *start* of a `primary` is unambiguously a literal; a trailing
            // `[` (an index) is handled in `parse_postfix`. The element exprs sit
            // inside brackets, so struct literals are unambiguous again.
            Tok::LBracket => {
                self.bump();
                let mut elems = Vec::new();
                if self.peek() != &Tok::RBracket {
                    elems.push(self.parse_expr_allow_struct()?);
                    while self.eat(&Tok::Comma) {
                        if self.peek() == &Tok::RBracket {
                            break; // trailing comma
                        }
                        elems.push(self.parse_expr_allow_struct()?);
                    }
                }
                self.expect(Tok::RBracket)?;
                Ok(Expr::Array(elems))
            }
            other => Err(ParseError::new(format!(
                "expected an expression, found {other:?}"
            ))),
        }
    }

    /// Parse a struct literal body `{ field: expr, ... }`, given the already-read
    /// type `path`. The opening `{` is the next token (`spec/02` §B `primary`
    /// struct-literal form). Field initializers may be written in any order;
    /// trailing commas are tolerated, and the empty form `Name {}` is allowed.
    fn parse_struct_literal(&mut self, path: Path) -> PResult<Expr> {
        self.expect(Tok::LBrace)?;
        self.skip_nl();
        let mut fields = Vec::new();
        if self.peek() != &Tok::RBrace {
            fields.push(self.parse_field_init()?);
            while self.eat(&Tok::Comma) {
                self.skip_nl();
                if self.peek() == &Tok::RBrace {
                    break; // trailing comma
                }
                fields.push(self.parse_field_init()?);
            }
        }
        self.skip_nl();
        self.expect(Tok::RBrace)?;
        Ok(Expr::Struct { path, fields })
    }

    fn parse_field_init(&mut self) -> PResult<FieldInit> {
        let name = self.ident()?;
        self.expect(Tok::Colon)?;
        // A field value sits inside the literal's braces, so struct literals are
        // unambiguous here.
        let value = self.parse_expr_allow_struct()?;
        Ok(FieldInit { name, value })
    }
}

/// Convert a parsed expression into an assignment target, or fail if it is not a
/// valid `lvalue` (`spec/02` §B: a root identifier followed by field and index
/// accesses). This reuses the postfix expression parser to read the target, then
/// validates its shape here.
fn expr_to_lvalue(e: Expr) -> PResult<LValue> {
    match e {
        Expr::Var(name) => Ok(LValue::Var(name)),
        Expr::Field(base, field) => Ok(LValue::Field(Box::new(expr_to_lvalue(*base)?), field)),
        Expr::Index(base, index) => Ok(LValue::Index(Box::new(expr_to_lvalue(*base)?), index)),
        _ => Err(ParseError::new(
            "invalid assignment target: an `lvalue` is a name optionally followed by `.field` \
             and `[index]` accesses",
        )),
    }
}
