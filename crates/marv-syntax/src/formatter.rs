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
        out.push('\n'); // blank line before each item (and its doc comments)
        out.push_str(&format_docs(item.docs()));
        out.push_str(&format_item(item));
        out.push('\n');
    }

    out
}

/// Format an item's doc-comment lines as canonical `/// text` lines (one per
/// entry, each terminated by a newline), at column 0. An empty-content line is
/// rendered as a bare `///`. Returns `""` when the item has no docs.
fn format_docs(docs: &[String]) -> String {
    let mut s = String::new();
    for line in docs {
        if line.is_empty() {
            s.push_str("///\n");
        } else {
            s.push_str("/// ");
            s.push_str(line);
            s.push('\n');
        }
    }
    s
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
        Item::Error(decl) => format_error(decl),
        Item::Fn(decl) => format_fn(decl),
        Item::Interface(decl) => format_interface(decl),
        Item::Impl(decl) => format_impl(decl),
    }
}

/// Format an `error` declaration inline: `error Name { V1, V2 }`. Variants are
/// bare names, so (like a one-line struct) they stay on the signature line; an
/// empty variant list cannot occur (the grammar requires at least one).
fn format_error(decl: &ErrorDecl) -> String {
    format!("error {} {{ {} }}", decl.name, decl.variants.join(", "))
}

/// Format a generic parameter list: `""` when empty, else `[A, B: Bound, ...]`
/// (`spec/02` §B `generics`).
fn format_generics(generics: &[Generic]) -> String {
    if generics.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = generics.iter().map(format_generic).collect();
        format!("[{}]", parts.join(", "))
    }
}

/// Format one generic parameter with its optional interface bound.
fn format_generic(g: &Generic) -> String {
    match &g.bound {
        None => g.name.clone(),
        Some(b) => format!("{}: {}", g.name, format_bound(b)),
    }
}

/// Format an interface bound `Path` or `Path[args]` (`spec/02` §B `bound`).
fn format_bound(b: &Bound) -> String {
    let path = b.path.join(".");
    if b.args.is_empty() {
        path
    } else {
        let args: Vec<String> = b.args.iter().map(format_type).collect();
        format!("{}[{}]", path, args.join(", "))
    }
}

/// Indent every non-empty line of `text` by `level` four-space units. Used to
/// place `interface`/`impl` members (formatted at column 0) inside their block.
fn indent_lines(text: &str, level: usize) -> String {
    let pad = indent(level);
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format an `interface` declaration (`spec/02` §B `interface_decl`): one method
/// signature per line at 4-space indentation. An interface with no methods is
/// `{}` on the signature line.
fn format_interface(decl: &InterfaceDecl) -> String {
    let mut s = format!("interface {}{}", decl.name, format_generics(&decl.generics));
    if decl.methods.is_empty() {
        s.push_str(" {}");
        return s;
    }
    s.push_str(" {\n");
    for m in &decl.methods {
        let mut sig = format_docs(&m.docs);
        sig.push_str(&format_fn_sig(m));
        s.push_str(&indent_lines(&sig, 1));
        s.push('\n');
    }
    s.push('}');
    s
}

/// Format an abstract method signature inside an `interface` (`spec/02` §B
/// `fn_sig`): no body, no contracts.
fn format_fn_sig(sig: &FnSig) -> String {
    let mut s = format!("fn {}{}(", sig.name, format_generics(&sig.generics));
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, format_type(&p.ty)))
        .collect();
    s.push_str(&params.join(", "));
    s.push(')');
    if let Some(ret) = &sig.ret {
        s.push_str(" -> ");
        s.push_str(&format_type(ret));
    }
    s
}

/// Format an `impl` block (`spec/02` §B `impl_decl`): each method is a full
/// function declaration at 4-space indentation, methods separated by a blank
/// line. An empty impl is `{}` on the header line.
fn format_impl(decl: &ImplDecl) -> String {
    let args: Vec<String> = decl.args.iter().map(format_type).collect();
    let mut s = format!("impl {}[{}]", decl.interface.join("."), args.join(", "));
    if decl.methods.is_empty() {
        s.push_str(" {}");
        return s;
    }
    s.push_str(" {\n");
    let bodies: Vec<String> = decl
        .methods
        .iter()
        .map(|m| {
            let mut body = format_docs(&m.docs);
            body.push_str(&format_fn(m));
            indent_lines(&body, 1)
        })
        .collect();
    s.push_str(&bodies.join("\n\n"));
    s.push('\n');
    s.push('}');
    s
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
    s.push_str(&format_generics(&decl.generics));
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
    if decl.is_unsafe {
        s.push_str("unsafe ");
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
        Type::Array { len, elem } => format!("[{}]{}", len, format_type(elem)),
        Type::Ref { mutable, inner } => {
            let kw = if *mutable { "&mut " } else { "&" };
            format!("{}{}", kw, format_type(inner))
        }
        Type::ErrorUnion(Some(inner)) => format!("!{}", format_type(inner)),
        Type::ErrorUnion(None) => "!".to_string(),
        Type::Optional(inner) => format!("?{}", format_type(inner)),
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
        Expr::Char(c) => format!("'{}'", escape_char(*c)),
        Expr::Var(name) => name.clone(),
        Expr::Field(base, name) => format!("{}.{}", format_expr(base), name),
        Expr::Call(callee, args) => {
            let args: Vec<String> = args.iter().map(format_expr).collect();
            format!("{}({})", format_expr(callee), args.join(", "))
        }
        Expr::Index(base, index) => {
            format!("{}[{}]", format_expr(base), format_expr(index))
        }
        Expr::Slice(base, start, end) => {
            format!(
                "{}[{}..{}]",
                format_expr(base),
                format_expr(start),
                format_expr(end)
            )
        }
        // An array literal is its bracketed, comma-separated elements: `[1, 2, 3]`
        // (and `[]` for the empty form). Re-parses to the same `Array`.
        Expr::Array(elems) => {
            let parts: Vec<String> = elems.iter().map(format_expr).collect();
            format!("[{}]", parts.join(", "))
        }
        Expr::ListLiteral { alloc, items } => {
            let item_parts: Vec<String> = items.iter().map(format_expr).collect();
            let items = format!("[{}]", item_parts.join(", "));
            match alloc {
                Some(alloc) => {
                    format!("List {{ alloc: {}, items: {} }}", format_expr(alloc), items)
                }
                None => format!("List {{ items: {} }}", items),
            }
        }
        Expr::SetLiteral { alloc, items } => {
            let item_parts: Vec<String> = items.iter().map(format_expr).collect();
            let items = format!("[{}]", item_parts.join(", "));
            match alloc {
                Some(alloc) => {
                    format!("Set {{ alloc: {}, items: {} }}", format_expr(alloc), items)
                }
                None => format!("Set {{ items: {} }}", items),
            }
        }
        Expr::MapLiteral {
            alloc,
            keys,
            values,
        } => {
            let key_parts: Vec<String> = keys.iter().map(format_expr).collect();
            let value_parts: Vec<String> = values.iter().map(format_expr).collect();
            let keys = format!("[{}]", key_parts.join(", "));
            let values = format!("[{}]", value_parts.join(", "));
            match alloc {
                Some(alloc) => format!(
                    "Map {{ alloc: {}, keys: {}, values: {} }}",
                    format_expr(alloc),
                    keys,
                    values
                ),
                None => format!("Map {{ keys: {}, values: {} }}", keys, values),
            }
        }
        Expr::Try(inner) => format!("{}?", format_expr(inner)),
        // A cast is fully parenthesized, like a binary node, so the canonical
        // form is unambiguous and re-parses to the same `Cast`.
        Expr::Cast(inner, ty) => format!("({} as {})", format_expr(inner), format_type(ty)),
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
        // A prefix unary is its operator directly against the operand: `-x`,
        // `&x`, `not x`, `&mut x`. The word operators `not` and `&mut` end in a
        // letter, so they take a single separating space; `-`/`&` abut their
        // operand. The operand is a `postfix`/`unary` (binary and cast nodes are
        // themselves parenthesized), so no extra parentheses are needed and the
        // form re-parses to the same `Unary`.
        Expr::Unary(op, operand) => {
            let sep = match op {
                UnOp::Not | UnOp::RefMut => " ",
                UnOp::Neg | UnOp::Ref => "",
            };
            format!("{}{}{}", op.as_str(), sep, format_expr(operand))
        }
        // A bounded quantifier is fully parenthesized, like a binary node, so
        // its maximal-right body cannot capture a following operator and the
        // canonical form re-parses to the same `Quant` (`spec/02` §B
        // `quant_expr`, MARV-11).
        Expr::Quant {
            exists,
            binder,
            lo,
            hi,
            body,
        } => {
            let kw = if *exists { "exists" } else { "forall" };
            format!(
                "({kw} {binder} in {}..{}: {})",
                format_expr(lo),
                format_expr(hi),
                format_expr(body)
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

/// Escape a character literal's scalar, the inverse of the lexer's char
/// unescaping. Only the delimiter `'`, the escape lead `\`, and the standard
/// control escapes need escaping; everything else prints verbatim.
fn escape_char(c: char) -> String {
    match c {
        '\'' => "\\'".to_string(),
        '\\' => "\\\\".to_string(),
        '\n' => "\\n".to_string(),
        '\t' => "\\t".to_string(),
        '\r' => "\\r".to_string(),
        '\0' => "\\0".to_string(),
        other => other.to_string(),
    }
}
