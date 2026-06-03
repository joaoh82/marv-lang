//! Human-readable rendering of contract [`Pred`]s (`spec/02` §C).
//!
//! The Core `Pred` language is names-erased: a contract atom is a flat index
//! (`Var(k)` = the k-th parameter, `Var(n)` = `result` — the convention
//! `marv_core::lower` produces and the M6 Tier-1/Tier-2 verifiers consume).
//! [`render_pred`] turns one back into readable text by asking the caller to map
//! each index to a name, so diagnostics and proof obligations read like the
//! source (`result >= lo`) rather than the encoding (`v3 >= v1`).

use crate::ir::{Atom, CmpOp, Literal, Pred};

/// Render a contract predicate to a source-like string. `name(i)` maps a flat
/// contract index to a display name (typically a parameter name, or `result`
/// for the index one past the last parameter).
pub fn render_pred(p: &Pred, name: &dyn Fn(u32) -> String) -> String {
    match p {
        Pred::True => "true".to_string(),
        Pred::False => "false".to_string(),
        Pred::Cmp(op, a, b) => {
            format!(
                "{} {} {}",
                render_atom(a, name),
                cmp_str(*op),
                render_atom(b, name)
            )
        }
        Pred::And(l, r) => format!("({} and {})", render_pred(l, name), render_pred(r, name)),
        Pred::Or(l, r) => format!("({} or {})", render_pred(l, name), render_pred(r, name)),
        Pred::Not(inner) => format!("not {}", render_pred(inner, name)),
        Pred::Forall { domain, body } => format!(
            "forall i in {}..{}: {}",
            render_atom(&domain.0, name),
            render_atom(&domain.1, name),
            render_pred(body, name)
        ),
        Pred::Exists { domain, body } => format!(
            "exists i in {}..{}: {}",
            render_atom(&domain.0, name),
            render_atom(&domain.1, name),
            render_pred(body, name)
        ),
    }
}

fn render_atom(a: &Atom, name: &dyn Fn(u32) -> String) -> String {
    match a {
        Atom::Var(i) => name(*i),
        Atom::Lit(Literal::Int(n)) => n.to_string(),
        Atom::Lit(Literal::Bool(b)) => b.to_string(),
        Atom::Lit(Literal::Unit) => "()".to_string(),
        Atom::Lit(Literal::Float(bits)) => format!("{}", f64::from_bits(*bits)),
        Atom::Lit(Literal::Str(s)) => format!("{s:?}"),
        Atom::Lit(Literal::Char(c)) => format!("{c:?}"),
        Atom::Global(h) => format!("global#{}", &h.to_hex()[..8]),
    }
}

fn cmp_str(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "==",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
    }
}
