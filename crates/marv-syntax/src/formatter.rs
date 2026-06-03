//! The canonical formatter: AST → text (`spec/02-grammar-and-core-ir.md` §B,
//! invariant #1 "one canonical form").
//!
//! It is the inverse of [`crate::parse`]. Canonical rules (M0):
//!
//! - 4-space indentation, one statement per line, no trailing whitespace.
//! - Every binary node is fully parenthesized as `(a op b)`.
//! - `, ` separates list elements; no trailing commas; no semicolons.
//! - `mod` then imports (no blank lines between them), then one blank line,
//!   then items separated by single blank lines. Exactly one trailing newline.

use crate::ast::*;

const INDENT: &str = "    ";

fn indent(level: usize) -> String {
    INDENT.repeat(level)
}

/// Format a whole module into its single canonical textual form.
pub fn format_module(module: &Module) -> String {
    let mut out = String::new();

    out.push_str("mod ");
    out.push_str(&module.name.join("."));
    out.push('\n');

    for import in &module.imports {
        out.push_str(&format_import(import));
        out.push('\n');
    }

    for item in &module.items {
        out.push('\n'); // blank line before each item
        out.push_str(&format_item(item));
        out.push('\n');
    }

    out
}

fn format_import(import: &Import) -> String {
    let mut s = format!("import {}", import.path.join("."));
    if let Some(names) = &import.names {
        s.push_str(" (");
        s.push_str(&names.join(", "));
        s.push(')');
    }
    s
}

fn format_item(item: &Item) -> String {
    match item {
        Item::Struct(decl) => format_struct(decl),
        Item::Fn(decl) => format_fn(decl),
    }
}

fn format_struct(decl: &StructDecl) -> String {
    let mut s = String::new();
    if decl.linear {
        s.push_str("linear ");
    }
    s.push_str("struct ");
    s.push_str(&decl.name);
    if decl.fields.is_empty() {
        s.push_str(" {}");
    } else {
        s.push_str(" { ");
        let fields: Vec<String> = decl
            .fields
            .iter()
            .map(|f| format!("{}: {}", f.name, format_type(&f.ty)))
            .collect();
        s.push_str(&fields.join(", "));
        s.push_str(" }");
    }
    s
}

fn format_fn(decl: &FnDecl) -> String {
    let mut s = String::new();
    if decl.is_pure {
        s.push_str("pure ");
    }
    s.push_str("fn ");
    s.push_str(&decl.name);
    s.push('(');
    let params: Vec<String> = decl
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, format_type(&p.ty)))
        .collect();
    s.push_str(&params.join(", "));
    s.push(')');
    if let Some(ret) = &decl.ret {
        s.push_str(" -> ");
        s.push_str(&format_type(ret));
    }
    // Contracts (if any) each go on their own indented line, and the body's
    // brace then starts a fresh line; otherwise the brace shares the signature
    // line. This is the canonical form the parser round-trips (`spec/01` §7).
    if decl.requires.is_empty() && decl.ensures.is_empty() {
        s.push(' ');
    } else {
        let pad = indent(1);
        for r in &decl.requires {
            s.push('\n');
            s.push_str(&pad);
            s.push_str("requires ");
            s.push_str(&format_expr(r));
        }
        for e in &decl.ensures {
            s.push('\n');
            s.push_str(&pad);
            s.push_str("ensures ");
            s.push_str(&format_expr(e));
        }
        s.push('\n');
    }
    s.push_str(&format_block(&decl.body, 0));
    s
}

fn format_type(ty: &Type) -> String {
    match ty {
        Type::Unit => "()".to_string(),
        Type::Named(path) => path.join("."),
        Type::Slice(inner) => format!("[]{}", format_type(inner)),
        Type::Ref { mutable, inner } => {
            let kw = if *mutable { "&mut " } else { "&" };
            format!("{}{}", kw, format_type(inner))
        }
    }
}

/// Format a block whose braces sit at indentation `level`; its contents are at
/// `level + 1`. An empty block is `{}`; otherwise it is multi-line.
fn format_block(block: &Block, level: usize) -> String {
    if block.stmts.is_empty() && block.tail.is_none() {
        return "{}".to_string();
    }

    let inner = level + 1;
    let pad = indent(inner);
    let mut s = String::from("{\n");

    for stmt in &block.stmts {
        s.push_str(&pad);
        s.push_str(&format_stmt(stmt));
        s.push('\n');
    }
    if let Some(tail) = &block.tail {
        s.push_str(&pad);
        s.push_str(&format_tail(tail, inner));
        s.push('\n');
    }

    s.push_str(&indent(level));
    s.push('}');
    s
}

fn format_stmt(stmt: &Stmt) -> String {
    let (kw, name, ty, value) = match stmt {
        Stmt::Let { name, ty, value } => ("let", name, ty, value),
        Stmt::Var { name, ty, value } => ("var", name, ty, value),
    };
    let mut s = format!("{kw} {name}");
    if let Some(ty) = ty {
        s.push_str(": ");
        s.push_str(&format_type(ty));
    }
    s.push_str(" = ");
    s.push_str(&format_expr(value));
    s
}

/// Format a block tail. `level` is the indentation of the line it sits on (the
/// caller has already emitted the leading indent for the first line).
fn format_tail(tail: &Tail, level: usize) -> String {
    match tail {
        Tail::Expr(e) => format_expr(e),
        Tail::Return(None) => "return".to_string(),
        Tail::Return(Some(e)) => format!("return {}", format_expr(e)),
        Tail::If(if_expr) => format_if(if_expr, level),
    }
}

/// Format an `if`/`else` chain. The `if` and the closing braces sit at `level`;
/// branch bodies are at `level + 1`. The first line carries no leading indent
/// (the caller supplied it); `} else` shares a line.
fn format_if(if_expr: &IfExpr, level: usize) -> String {
    let mut s = String::from("if ");
    s.push_str(&format_expr(&if_expr.cond));
    s.push(' ');
    s.push_str(&format_block(&if_expr.then, level));
    if let Some(els) = &if_expr.els {
        s.push_str(" else ");
        match els {
            Else::If(inner) => s.push_str(&format_if(inner, level)),
            Else::Block(block) => s.push_str(&format_block(block, level)),
        }
    }
    s
}

fn format_expr(expr: &Expr) -> String {
    match expr {
        Expr::Unit => "()".to_string(),
        Expr::Int(n) => n.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Str(s) => format!("\"{}\"", escape_str(s)),
        Expr::Var(name) => name.clone(),
        Expr::Field(base, name) => format!("{}.{}", format_expr(base), name),
        Expr::Call(callee, args) => {
            let args: Vec<String> = args.iter().map(format_expr).collect();
            format!("{}({})", format_expr(callee), args.join(", "))
        }
        Expr::Binary(lhs, op, rhs) => {
            format!(
                "({} {} {})",
                format_expr(lhs),
                op.as_str(),
                format_expr(rhs)
            )
        }
    }
}

/// Escape a string literal's contents, the inverse of the lexer's unescaping.
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}
