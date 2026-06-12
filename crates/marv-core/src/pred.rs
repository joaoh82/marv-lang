//! Human-readable rendering of contract [`Pred`]s (`spec/02` §C).
//!
//! The Core `Pred` language is names-erased: a contract variable is just an
//! index, in one of two conventions (see [`Pred`]) — the **flat** convention of
//! `requires`/`ensures` (`Var(k)` = the k-th parameter, `Var(n)` = `result`),
//! or the **de Bruijn** convention of loop invariants (indices into the
//! loop-header environment). [`render_pred`] turns one back into readable text
//! by asking the caller to map each non-binder index to a name, so diagnostics
//! and proof obligations read like the source (`result >= lo`) rather than the
//! encoding (`v3 >= v1`).
//!
//! Quantifier binders (MARV-11) have no caller-visible index: they are named
//! positionally by nesting depth — `i` for the outermost, then `i1`, `i2`, … —
//! in both conventions.

use crate::ir::{ArithOp, Atom, CExpr, CNode, CmpOp, Literal, Pred};

/// Which variable-index convention a [`Pred`] uses (see the type's docs).
#[derive(Clone, Copy)]
pub enum PredVars {
    /// `requires`/`ensures`: `Var(k)` = parameter k, `Var(n)` = `result`,
    /// `Var(n + 1 + j)` = the j-th enclosing quantifier binder. `arity` is n.
    Flat { arity: u32 },
    /// Loop invariants: de Bruijn indices into the loop-header environment;
    /// each quantifier binds index 0 in its body.
    DeBruijn,
}

/// Render a flat-convention contract predicate to a source-like string.
/// `name(i)` maps a flat contract index to a display name (a parameter name,
/// or `result` for the index one past the last parameter). Quantifier binders
/// are rendered positionally (`i`, `i1`, …); `arity` is taken as the largest
/// index `name` covers, inferred lazily — callers with quantified contracts
/// should prefer [`render_pred_with`] and say the arity explicitly.
pub fn render_pred(p: &Pred, name: &dyn Fn(u32) -> String) -> String {
    // Without an arity we cannot tell binder indices from `result`; predicates
    // produced before MARV-11 contain no quantifiers, and new callers pass the
    // convention explicitly. `u32::MAX` keeps every index on the caller.
    render(p, name, PredVars::Flat { arity: u32::MAX }, 0)
}

/// Render a contract predicate under an explicit index convention. `name(i)`
/// maps non-binder indices to display names: flat contract indices for
/// [`PredVars::Flat`], or environment indices *as the caller knows them*
/// (without any quantifier shift) for [`PredVars::DeBruijn`].
pub fn render_pred_with(p: &Pred, name: &dyn Fn(u32) -> String, vars: PredVars) -> String {
    render(p, name, vars, 0)
}

/// The display name of the quantifier binder introduced at nesting depth `j`
/// (0 = outermost): `i`, `i1`, `i2`, …
fn binder_name(j: u32) -> String {
    if j == 0 {
        "i".to_string()
    } else {
        format!("i{j}")
    }
}

fn render(p: &Pred, name: &dyn Fn(u32) -> String, vars: PredVars, depth: u32) -> String {
    match p {
        Pred::True => "true".to_string(),
        Pred::False => "false".to_string(),
        Pred::Cmp(op, a, b) => {
            format!(
                "{} {} {}",
                render_cexpr(a, name, vars, depth),
                cmp_str(*op),
                render_cexpr(b, name, vars, depth)
            )
        }
        Pred::And(l, r) => format!(
            "({} and {})",
            render(l, name, vars, depth),
            render(r, name, vars, depth)
        ),
        Pred::Or(l, r) => format!(
            "({} or {})",
            render(l, name, vars, depth),
            render(r, name, vars, depth)
        ),
        Pred::Not(inner) => format!("not {}", render(inner, name, vars, depth)),
        Pred::Forall { domain, body } => format!(
            "forall {} in {}..{}: {}",
            binder_name(depth),
            render_cexpr(&domain.0, name, vars, depth),
            render_cexpr(&domain.1, name, vars, depth),
            render(body, name, vars, depth + 1)
        ),
        Pred::Exists { domain, body } => format!(
            "exists {} in {}..{}: {}",
            binder_name(depth),
            render_cexpr(&domain.0, name, vars, depth),
            render_cexpr(&domain.1, name, vars, depth),
            render(body, name, vars, depth + 1)
        ),
    }
}

fn render_cexpr(e: &CExpr, name: &dyn Fn(u32) -> String, vars: PredVars, depth: u32) -> String {
    match e {
        CExpr::Atom(a) => render_var_atom(a, name, vars, depth),
        CExpr::Node(n) => match &**n {
            CNode::Bin(op, l, r) => format!(
                "({} {} {})",
                render_cexpr(l, name, vars, depth),
                arith_str(*op),
                render_cexpr(r, name, vars, depth)
            ),
            CNode::Neg(inner) => format!("-{}", render_cexpr(inner, name, vars, depth)),
            CNode::Len(inner) => format!("len({})", render_cexpr(inner, name, vars, depth)),
            CNode::Index(base, index) => format!(
                "{}[{}]",
                render_cexpr(base, name, vars, depth),
                render_cexpr(index, name, vars, depth)
            ),
            // Field names are erased with the rest of the identity; render the
            // declaration index positionally.
            CNode::Proj(base, idx) => {
                format!("{}.{}", render_cexpr(base, name, vars, depth), idx)
            }
        },
    }
}

/// Resolve a `Var` index to its display name under `vars` at quantifier
/// nesting `depth`; literals and globals render directly.
fn render_var_atom(a: &Atom, name: &dyn Fn(u32) -> String, vars: PredVars, depth: u32) -> String {
    let Atom::Var(i) = a else {
        return render_atom(a);
    };
    match vars {
        PredVars::Flat { arity } => {
            // Indices above `result` (arity) are quantifier binders, j counted
            // from the outermost enclosing quantifier.
            if *i > arity {
                let j = *i - arity - 1;
                if j < depth {
                    return binder_name(j);
                }
            }
            name(*i)
        }
        PredVars::DeBruijn => {
            // The innermost `depth` indices are quantifier binders; everything
            // beyond unwinds the shift so the caller sees its own env index.
            if *i < depth {
                binder_name(depth - 1 - *i)
            } else {
                name(*i - depth)
            }
        }
    }
}

fn render_atom(a: &Atom) -> String {
    match a {
        Atom::Var(i) => format!("v{i}"),
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

fn arith_str(op: ArithOp) -> &'static str {
    match op {
        ArithOp::Add => "+",
        ArithOp::Sub => "-",
        ArithOp::Mul => "*",
        ArithOp::Div => "/",
        ArithOp::Rem => "%",
    }
}
