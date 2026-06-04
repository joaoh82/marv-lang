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
        Item::Enum(decl) => format_enum(decl),
        Item::Fn(decl) => format_fn(decl),
    }
}

/// Format a generic parameter / argument list: `""` when empty, else
/// `[A, B, ...]`.
fn format_generics(names: &[String]) -> String {
    if names.is_empty() {
        String::new()
    } else {
        format!("[{}]", names.join(", "))
    }
}

/// Format an `enum` declaration in canonical form: one variant per line, each
/// terminated by a comma (including the last), at 4-space indentation. An
/// empty enum is `{}` on the signature line.
fn format_enum(decl: &EnumDecl) -> String {
    let mut s = format!("enum {}{}", decl.name, format_generics(&decl.generics));
    if decl.variants.is_empty() {
        s.push_str(" {}");
        return s;
    }
    s.push_str(" {\n");
    let pad = indent(1);
    for v in &decl.variants {
        s.push_str(&pad);
        s.push_str(&v.name);
        if !v.fields.is_empty() {
            let tys: Vec<String> = v.fields.iter().map(format_type).collect();
            s.push('(');
            s.push_str(&tys.join(", "));
            s.push(')');
        }
        s.push_str(",\n");
    }
    s.push('}');
    s
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
    s.push_str(&format_generics(&decl.generics));
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
        Type::Generic { path, args } => {
            let args: Vec<String> = args.iter().map(format_type).collect();
            format!("{}[{}]", path.join("."), args.join(", "))
        }
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
        s.push_str(&format_stmt(stmt, inner));
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

/// Format a statement that begins at indentation `level` (the caller has already
/// emitted the leading indent for its first line). `level` matters only for the
/// multi-line loop statements, whose continuation lines and body indent relative
/// to it.
fn format_stmt(stmt: &Stmt, level: usize) -> String {
    let (kw, name, ty, value) = match stmt {
        Stmt::Let { name, ty, value } => ("let", name, ty, value),
        Stmt::Var { name, ty, value } => ("var", name, ty, value),
        Stmt::Assign { target, value } => {
            return format!("{} = {}", format_lvalue(target), format_expr(value));
        }
        Stmt::While {
            cond,
            invariants,
            body,
        } => return format_while(cond, invariants, body, level),
        Stmt::For { binder, iter, body } => return format_for(binder, iter, body, level),
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

/// Format an assignment target (`spec/02` §B `lvalue`).
fn format_lvalue(lv: &LValue) -> String {
    match lv {
        LValue::Var(name) => name.clone(),
        LValue::Field(base, field) => format!("{}.{}", format_lvalue(base), field),
        LValue::Index(base, index) => format!("{}[{}]", format_lvalue(base), format_expr(index)),
    }
}

/// Format a block tail. `level` is the indentation of the line it sits on (the
/// caller has already emitted the leading indent for the first line).
fn format_tail(tail: &Tail, level: usize) -> String {
    match tail {
        Tail::Expr(e) => format_expr(e),
        Tail::Return(None) => "return".to_string(),
        Tail::Return(Some(e)) => format!("return {}", format_expr(e)),
        Tail::If(if_expr) => format_if(if_expr, level),
        Tail::Match(m) => format_match(m, level),
    }
}

/// Format a `match`. The `match` keyword and the closing brace sit at `level`;
/// arms are at `level + 1`. The first line carries no leading indent (the caller
/// supplied it).
fn format_match(m: &MatchExpr, level: usize) -> String {
    let mut s = String::from("match ");
    s.push_str(&format_expr(&m.scrutinee));
    s.push_str(" {\n");
    let arm_level = level + 1;
    let pad = indent(arm_level);
    for arm in &m.arms {
        s.push_str(&pad);
        s.push_str(&format_pattern(&arm.pat));
        s.push_str(" => ");
        match &arm.body {
            ArmBody::Expr(e) => s.push_str(&format_expr(e)),
            ArmBody::Block(b) => s.push_str(&format_block(b, arm_level)),
        }
        s.push_str(",\n");
    }
    s.push_str(&indent(level));
    s.push('}');
    s
}

fn format_pattern(pat: &Pattern) -> String {
    match pat {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Ctor { path, fields } => {
            let mut s = path.join(".");
            if !fields.is_empty() {
                let fs: Vec<String> = fields.iter().map(format_field_pat).collect();
                s.push('(');
                s.push_str(&fs.join(", "));
                s.push(')');
            }
            s
        }
    }
}

fn format_field_pat(fp: &FieldPat) -> String {
    match fp {
        FieldPat::Bind(name) => name.clone(),
        FieldPat::Wildcard => "_".to_string(),
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

/// Format a `while` loop. Without invariants the body brace shares the head line
/// (`while <cond> { .. }`, like `if`). With invariants, each `invariant` clause
/// goes on its own line at `level + 1` and the body brace then starts a fresh
/// line at `level` — mirroring the canonical form for `fn` contract clauses.
fn format_while(cond: &Expr, invariants: &[Expr], body: &Block, level: usize) -> String {
    let mut s = String::from("while ");
    s.push_str(&format_expr(cond));
    if invariants.is_empty() {
        s.push(' ');
        s.push_str(&format_block(body, level));
    } else {
        let pad = indent(level + 1);
        for inv in invariants {
            s.push('\n');
            s.push_str(&pad);
            s.push_str("invariant ");
            s.push_str(&format_expr(inv));
        }
        s.push('\n');
        s.push_str(&indent(level));
        s.push_str(&format_block(body, level));
    }
    s
}

/// Format a `for` loop: `for <binder> in <iter> { .. }`, the body brace sharing
/// the head line.
fn format_for(binder: &str, iter: &Expr, body: &Block, level: usize) -> String {
    format!(
        "for {binder} in {} {}",
        format_expr(iter),
        format_block(body, level)
    )
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
        Expr::Index(base, index) => {
            format!("{}[{}]", format_expr(base), format_expr(index))
        }
        Expr::Struct { path, fields } => {
            if fields.is_empty() {
                format!("{} {{}}", path.join("."))
            } else {
                let inits: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, format_expr(&f.value)))
                    .collect();
                format!("{} {{ {} }}", path.join("."), inits.join(", "))
            }
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
