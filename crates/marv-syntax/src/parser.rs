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
use crate::lexer::{lex, Tok};

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
    let tokens = lex(src).map_err(|e| ParseError::new(e.message))?;
    let mut p = Parser { tokens, pos: 0 };
    let module = p.parse_module()?;
    p.expect(Tok::Eof)?;
    Ok(module)
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        // The token stream always ends in `Eof`, so indexing the last element is
        // safe once `pos` runs off the end.
        self.tokens.get(self.pos).unwrap_or(&Tok::Eof)
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
            self.skip_nl();
            if self.peek() == &Tok::Eof {
                break;
            }
            items.push(self.parse_item()?);
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

    fn parse_item(&mut self) -> PResult<Item> {
        match self.peek() {
            Tok::Pure => {
                self.bump();
                Ok(Item::Fn(self.parse_fn(true)?))
            }
            Tok::Fn => Ok(Item::Fn(self.parse_fn(false)?)),
            Tok::Linear => {
                self.bump();
                Ok(Item::Struct(self.parse_struct(true)?))
            }
            Tok::Struct => Ok(Item::Struct(self.parse_struct(false)?)),
            other => Err(ParseError::new(format!(
                "expected an item (`fn`, `pure fn`, `struct`, `linear struct`), found {other:?}"
            ))),
        }
    }

    fn parse_struct(&mut self, linear: bool) -> PResult<StructDecl> {
        self.expect(Tok::Struct)?;
        let name = self.ident()?;
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
            linear,
            name,
            fields,
        })
    }

    fn parse_field(&mut self) -> PResult<Field> {
        let name = self.ident()?;
        self.expect(Tok::Colon)?;
        let ty = self.parse_type()?;
        Ok(Field { name, ty })
    }

    fn parse_fn(&mut self, is_pure: bool) -> PResult<FnDecl> {
        self.expect(Tok::Fn)?;
        let name = self.ident()?;
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
            is_pure,
            name,
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
            let expr = self.parse_expr()?;
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
            Tok::LParen => {
                self.bump();
                self.expect(Tok::RParen)?;
                Ok(Type::Unit)
            }
            Tok::LBracket => {
                self.bump();
                self.expect(Tok::RBracket)?;
                let elem = self.parse_type()?;
                Ok(Type::Slice(Box::new(elem)))
            }
            Tok::Ident(_) => Ok(Type::Named(self.parse_path()?)),
            other => Err(ParseError::new(format!("expected a type, found {other:?}"))),
        }
    }

    // ---- blocks & statements -------------------------------------------

    fn parse_block(&mut self) -> PResult<Block> {
        self.expect(Tok::LBrace)?;
        let mut stmts = Vec::new();
        let mut tail = None;

        loop {
            self.skip_nl();
            match self.peek() {
                Tok::RBrace => break,
                Tok::Let => stmts.push(self.parse_let(false)?),
                Tok::Var => stmts.push(self.parse_let(true)?),
                Tok::Return => {
                    tail = Some(self.parse_return_tail()?);
                    break;
                }
                Tok::If => {
                    tail = Some(Tail::If(Box::new(self.parse_if()?)));
                    break;
                }
                _ => {
                    tail = Some(Tail::Expr(self.parse_expr()?));
                    break;
                }
            }
        }

        self.skip_nl();
        self.expect(Tok::RBrace)?;
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
        let cond = self.parse_expr()?;
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

    // ---- expressions (precedence climbing) -----------------------------

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_bin(0)
    }

    /// Precedence-climbing binary-operator parser. `min_prec` is the lowest
    /// binding power this call will accept; operators are left-associative.
    fn parse_bin(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.parse_postfix()?;
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
        args.push(self.parse_expr()?);
        while self.eat(&Tok::Comma) {
            if self.peek() == &Tok::RParen {
                break; // trailing comma
            }
            args.push(self.parse_expr()?);
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
                Ok(Expr::Var(name))
            }
            Tok::LParen => {
                self.bump();
                if self.eat(&Tok::RParen) {
                    Ok(Expr::Unit)
                } else {
                    let inner = self.parse_expr()?;
                    self.expect(Tok::RParen)?;
                    Ok(inner)
                }
            }
            other => Err(ParseError::new(format!(
                "expected an expression, found {other:?}"
            ))),
        }
    }
}
