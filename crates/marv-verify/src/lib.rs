//! # marv-verify — SMT contract discharge (milestone M6, Tier 2)
//!
//! The verified-subset half of marv's layered verification (`spec/01` §7). Given
//! a **pure** function over integers and booleans, [`verify_def`] discharges its
//! `ensures` postconditions against the function body and its `requires`
//! preconditions using an SMT solver (z3, driven over SMT-LIB by `easy-smt`),
//! returning one of:
//!
//! - [`VerifyOutcome::Proved`] — every obligation holds for all inputs (Tier 2).
//! - [`VerifyOutcome::Failed`] — an obligation can be violated; carries a
//!   concrete **counterexample** assignment the agent can iterate against
//!   (`spec/03` §4.3).
//! - [`VerifyOutcome::Unsupported`] — the function is outside the decidable-ish
//!   subset; the honest answer is to fall back to Tier-1 runtime checks.
//! - [`VerifyOutcome::SolverUnavailable`] — no SMT solver could be launched;
//!   like `Unsupported`, callers fall back to runtime checks.
//!
//! ## How a function becomes a formula
//!
//! The body is symbolically evaluated into an SMT term for `result`
//! ([`encode_body`]): `if`/`else` becomes `ite`, arithmetic and comparisons map
//! to their SMT counterparts, `let` binds, parameters are SMT constants. A fresh
//! constant `res` is constrained to equal that term. Each precondition is
//! asserted as an assumption. Then, per postcondition `P`, the solver is asked
//! whether `requires ∧ res = body ∧ ¬P` is satisfiable: **unsat** proves `P`;
//! **sat** yields a model = counterexample.
//!
//! Contract atoms use the flat convention `marv_core::lower` emits: `Var(k)` is
//! the k-th parameter and `Var(n)` (n = arity) is `result`.

use easy_smt::{Context, ContextBuilder, Response, SExpr};

use marv_core::ir::*;
use marv_core::render_pred;

/// The result of discharging a definition's contracts (`spec/03` §3.3, §4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// All postconditions proved for all inputs.
    Proved,
    /// A postcondition can fail; `counterexample` maps each parameter (and
    /// `result`) to a concrete value, `obligation` is the violated clause.
    Failed {
        obligation: String,
        counterexample: Vec<(String, String)>,
        message: String,
    },
    /// Outside the verified subset — fall back to Tier-1 runtime checks.
    Unsupported { reason: String },
    /// No solver could be launched — fall back to Tier-1 runtime checks.
    SolverUnavailable { reason: String },
}

impl VerifyOutcome {
    /// The short status string the protocol reports (`spec/03` §4.3).
    pub fn status(&self) -> &'static str {
        match self {
            VerifyOutcome::Proved => "proved",
            VerifyOutcome::Failed { .. } => "failed",
            VerifyOutcome::Unsupported { .. } | VerifyOutcome::SolverUnavailable { .. } => {
                "unsupported"
            }
        }
    }
}

/// Verify one definition's contracts. `param_names` labels the parameters for
/// counterexamples and obligation messages (missing names render as `arg{i}`).
pub fn verify_def(def: &Def, param_names: &[String]) -> VerifyOutcome {
    // Only pure functions with a body are in the verified subset.
    if def.kind != DefKind::Fn {
        return unsupported("only functions carry verifiable contracts");
    }
    let Some(body) = &def.body else {
        return unsupported("function has no body to verify");
    };

    let (param_tys, ret_ty, eff) = peel_arrow(&def.ty);
    if !eff.is_empty() {
        return unsupported("Tier-2 verification covers only `pure` functions");
    }
    // Nothing to discharge ⇒ trivially proved.
    if def.ensures.is_empty() {
        return VerifyOutcome::Proved;
    }
    // Parameter and result types must be in the scalar subset.
    for t in &param_tys {
        if smt_sort_kind(t).is_none() {
            return unsupported(&format!(
                "parameter type `{}` is outside the verified subset (ints/bools)",
                show_type(t)
            ));
        }
    }
    if smt_sort_kind(&ret_ty).is_none() {
        return unsupported(&format!(
            "return type `{}` is outside the verified subset (ints/bools)",
            show_type(&ret_ty)
        ));
    }

    let n = param_tys.len();
    let label = |i: u32| -> String {
        let i = i as usize;
        if i < n {
            param_names
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("arg{i}"))
        } else {
            "result".to_string()
        }
    };

    // Launch the solver; absence is a fallback condition, not a hard error.
    let mut ctx = match ContextBuilder::new().with_z3_defaults().build() {
        Ok(c) => c,
        Err(e) => {
            return VerifyOutcome::SolverUnavailable {
                reason: format!("could not launch z3: {e}"),
            }
        }
    };

    match discharge(&mut ctx, def, body, &param_tys, &ret_ty, &label) {
        Ok(o) => o,
        Err(VerifyErr::Unsupported(reason)) => VerifyOutcome::Unsupported { reason },
        Err(VerifyErr::Solver(e)) => VerifyOutcome::SolverUnavailable {
            reason: format!("solver I/O error: {e}"),
        },
    }
}

enum VerifyErr {
    Unsupported(String),
    Solver(std::io::Error),
}

impl From<std::io::Error> for VerifyErr {
    fn from(e: std::io::Error) -> Self {
        VerifyErr::Solver(e)
    }
}

fn unsupported(reason: &str) -> VerifyOutcome {
    VerifyOutcome::Unsupported {
        reason: reason.to_string(),
    }
}

/// Build the SMT problem and check each postcondition.
fn discharge(
    ctx: &mut Context,
    def: &Def,
    body: &Core,
    param_tys: &[Type],
    ret_ty: &Type,
    label: &dyn Fn(u32) -> String,
) -> Result<VerifyOutcome, VerifyErr> {
    // Declare a constant per parameter (`p0`, `p1`, …) and the result (`res`).
    let mut consts: Vec<SExpr> = Vec::with_capacity(param_tys.len() + 1);
    for (i, t) in param_tys.iter().enumerate() {
        let sort = sort_of(ctx, t);
        let c = ctx.declare_const(format!("p{i}"), sort)?;
        consts.push(c);
    }
    let res = ctx.declare_const("res", sort_of(ctx, ret_ty))?;

    // res = <symbolic body>.
    let inner = peel_lams(body);
    let body_term = encode_body(ctx, inner, &consts).map_err(VerifyErr::Unsupported)?;
    let eq = ctx.eq(res, body_term);
    ctx.assert(eq)?;

    // Assume preconditions.
    for r in &def.requires {
        let p = encode_pred(ctx, r, &consts, None).map_err(VerifyErr::Unsupported)?;
        ctx.assert(p)?;
    }

    // Discharge each postcondition: prove ¬(requires ∧ body ∧ ¬ensures) is unsat.
    for ens in &def.ensures {
        let formula = encode_pred(ctx, ens, &consts, Some(res)).map_err(VerifyErr::Unsupported)?;
        ctx.push()?;
        let negated = ctx.not(formula);
        ctx.assert(negated)?;
        let resp = ctx.check()?;
        match resp {
            Response::Unsat => {
                ctx.pop()?;
            }
            Response::Sat => {
                let counterexample = model(ctx, &consts, res, label)?;
                let obligation = render_pred(ens, label);
                return Ok(VerifyOutcome::Failed {
                    message: format!("postcondition `{obligation}` can be violated"),
                    obligation,
                    counterexample,
                });
            }
            Response::Unknown => {
                ctx.pop()?;
                return Ok(VerifyOutcome::Unsupported {
                    reason: format!(
                        "solver returned `unknown` for `{}`",
                        render_pred(ens, label)
                    ),
                });
            }
        }
    }

    let _ = def; // (all fields consumed above)
    Ok(VerifyOutcome::Proved)
}

/// Read the satisfying assignment back as `(name, value)` pairs.
fn model(
    ctx: &mut Context,
    consts: &[SExpr],
    res: SExpr,
    label: &dyn Fn(u32) -> String,
) -> Result<Vec<(String, String)>, VerifyErr> {
    let mut query: Vec<SExpr> = consts.to_vec();
    query.push(res);
    let values = ctx.get_value(query)?;
    let mut out = Vec::with_capacity(values.len());
    for (i, (_, val)) in values.iter().enumerate() {
        let name = label(i as u32);
        out.push((name, render_value(ctx, *val)));
    }
    Ok(out)
}

/// Render a model value: an integer, a boolean, or its raw s-expression text.
fn render_value(ctx: &Context, v: SExpr) -> String {
    if let Some(n) = ctx.get_i64(v) {
        return n.to_string();
    }
    match ctx.get_atom(v) {
        Some("true") => "true".to_string(),
        Some("false") => "false".to_string(),
        Some(a) => a.to_string(),
        None => ctx.display(v).to_string(),
    }
}

// ---- symbolic body encoding --------------------------------------------

/// Encode a body term to an SMT expression under `env` (parameter constants
/// indexed by de Bruijn *level*, as the interpreter does). Returns the
/// unsupported reason as `Err`.
fn encode_body(ctx: &Context, c: &Core, env: &[SExpr]) -> Result<SExpr, String> {
    match c {
        Core::Atom(a) => encode_body_atom(ctx, a, env),

        Core::Let { value, body } => {
            let v = encode_body(ctx, value, env)?;
            let mut env2 = env.to_vec();
            env2.push(v);
            encode_body(ctx, body, &env2)
        }

        Core::Prim { op, args } => {
            let a: Vec<SExpr> = args
                .iter()
                .map(|x| encode_body_atom(ctx, x, env))
                .collect::<Result<_, _>>()?;
            encode_prim(ctx, *op, &a)
        }

        Core::Match {
            scrutinee,
            branches,
        } => {
            // Two-arm boolean `if`/`else` only: branch 0 = false, 1 = true.
            if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
                return Err("match other than a two-arm boolean `if`/`else`".to_string());
            }
            let cond = encode_body_atom(ctx, scrutinee, env)?;
            let then_t = encode_body(ctx, &branches[1].body, env)?;
            let else_t = encode_body(ctx, &branches[0].body, env)?;
            Ok(ctx.ite(cond, then_t, else_t))
        }

        Core::App { .. } => Err("function calls are outside the verified subset".to_string()),
        Core::Ctor { .. } | Core::Proj { .. } => {
            Err("aggregates/ADTs are outside the verified subset".to_string())
        }
        Core::Perform { .. } => Err("a `perform` makes the function impure".to_string()),
        Core::Raise { .. } => Err("`raise` is outside the verified subset".to_string()),
        Core::Loop { .. } => Err("loops are outside the verified subset".to_string()),
        Core::Lam { .. } => Err("nested lambda is outside the verified subset".to_string()),
    }
}

fn encode_body_atom(ctx: &Context, a: &Atom, env: &[SExpr]) -> Result<SExpr, String> {
    match a {
        Atom::Var(idx) => {
            let d = env.len();
            let i = (*idx as usize) + 1;
            if i > d {
                return Err("variable out of scope".to_string());
            }
            Ok(env[d - i])
        }
        Atom::Lit(Literal::Int(n)) => Ok(ctx.numeral(*n)),
        Atom::Lit(Literal::Bool(b)) => Ok(if *b { ctx.true_() } else { ctx.false_() }),
        Atom::Lit(_) => Err("non-scalar literal is outside the verified subset".to_string()),
        Atom::Global(_) => Err("global reference is outside the verified subset".to_string()),
    }
}

fn encode_prim(ctx: &Context, op: PrimOp, a: &[SExpr]) -> Result<SExpr, String> {
    use PrimOp::*;
    let bin = |i: usize, j: usize| (a[i], a[j]);
    Ok(match op {
        Add => {
            let (x, y) = bin(0, 1);
            ctx.plus(x, y)
        }
        Sub => {
            let (x, y) = bin(0, 1);
            ctx.sub(x, y)
        }
        Mul => {
            let (x, y) = bin(0, 1);
            ctx.times(x, y)
        }
        // SMT `div`/`mod` are Euclidean, but marv's `/`/`%` truncate toward zero;
        // rather than emit an unsound encoding, treat them as out-of-subset.
        Div | Rem => {
            return Err("integer division/remainder is not yet in the verified subset".to_string())
        }
        Eq => {
            let (x, y) = bin(0, 1);
            ctx.eq(x, y)
        }
        Ne => {
            let (x, y) = bin(0, 1);
            let e = ctx.eq(x, y);
            ctx.not(e)
        }
        Lt => {
            let (x, y) = bin(0, 1);
            ctx.lt(x, y)
        }
        Le => {
            let (x, y) = bin(0, 1);
            ctx.lte(x, y)
        }
        Gt => {
            let (x, y) = bin(0, 1);
            ctx.gt(x, y)
        }
        Ge => {
            let (x, y) = bin(0, 1);
            ctx.gte(x, y)
        }
        And => {
            let (x, y) = bin(0, 1);
            ctx.and(x, y)
        }
        Or => {
            let (x, y) = bin(0, 1);
            ctx.or(x, y)
        }
        Not => ctx.not(a[0]),
        Len | Index => return Err("len/index is outside the verified subset".to_string()),
    })
}

// ---- predicate encoding -------------------------------------------------

/// Encode a contract predicate to an SMT boolean. `consts` are the parameter
/// constants (flat index); `result` is the result constant if in scope.
fn encode_pred(
    ctx: &Context,
    p: &Pred,
    consts: &[SExpr],
    result: Option<SExpr>,
) -> Result<SExpr, String> {
    match p {
        Pred::True => Ok(ctx.true_()),
        Pred::False => Ok(ctx.false_()),
        Pred::Cmp(op, l, r) => {
            let x = encode_pred_atom(ctx, l, consts, result)?;
            let y = encode_pred_atom(ctx, r, consts, result)?;
            Ok(match op {
                CmpOp::Eq => ctx.eq(x, y),
                CmpOp::Ne => {
                    let e = ctx.eq(x, y);
                    ctx.not(e)
                }
                CmpOp::Lt => ctx.lt(x, y),
                CmpOp::Le => ctx.lte(x, y),
                CmpOp::Gt => ctx.gt(x, y),
                CmpOp::Ge => ctx.gte(x, y),
            })
        }
        Pred::And(l, r) => {
            let x = encode_pred(ctx, l, consts, result)?;
            let y = encode_pred(ctx, r, consts, result)?;
            Ok(ctx.and(x, y))
        }
        Pred::Or(l, r) => {
            let x = encode_pred(ctx, l, consts, result)?;
            let y = encode_pred(ctx, r, consts, result)?;
            Ok(ctx.or(x, y))
        }
        Pred::Not(inner) => {
            let x = encode_pred(ctx, inner, consts, result)?;
            Ok(ctx.not(x))
        }
        Pred::Forall { .. } | Pred::Exists { .. } => {
            Err("bounded quantifiers are not yet in the verified subset".to_string())
        }
    }
}

fn encode_pred_atom(
    ctx: &Context,
    a: &Atom,
    consts: &[SExpr],
    result: Option<SExpr>,
) -> Result<SExpr, String> {
    match a {
        Atom::Var(i) => {
            let i = *i as usize;
            if i < consts.len() {
                Ok(consts[i])
            } else if i == consts.len() {
                result.ok_or_else(|| "`result` used where it is not in scope".to_string())
            } else {
                Err("contract variable out of range".to_string())
            }
        }
        Atom::Lit(Literal::Int(n)) => Ok(ctx.numeral(*n)),
        Atom::Lit(Literal::Bool(b)) => Ok(if *b { ctx.true_() } else { ctx.false_() }),
        Atom::Lit(_) => Err("non-scalar literal in contract".to_string()),
        Atom::Global(_) => Err("global reference in contract".to_string()),
    }
}

// ---- type helpers -------------------------------------------------------

/// Which scalar SMT sort a type maps to (`None` ⇒ outside the subset).
fn smt_sort_kind(t: &Type) -> Option<SortKind> {
    match t {
        Type::Int(_) => Some(SortKind::Int),
        Type::Bool => Some(SortKind::Bool),
        _ => None,
    }
}

enum SortKind {
    Int,
    Bool,
}

fn sort_of(ctx: &Context, t: &Type) -> SExpr {
    match smt_sort_kind(t) {
        Some(SortKind::Bool) => ctx.bool_sort(),
        // Default to Int for the scalar subset (callers gate non-scalar types).
        _ => ctx.int_sort(),
    }
}

fn peel_arrow(ty: &Type) -> (Vec<Type>, Type, EffectRow) {
    let mut params = Vec::new();
    let mut cur = ty;
    let mut eff = EffectRow::empty();
    while let Type::Arrow {
        param,
        ret,
        effects,
    } = cur
    {
        params.push((**param).clone());
        eff = effects.clone();
        cur = ret;
    }
    (params, cur.clone(), eff)
}

fn peel_lams(mut body: &Core) -> &Core {
    while let Core::Lam { body: inner, .. } = body {
        body = inner;
    }
    body
}

fn show_type(t: &Type) -> String {
    match t {
        Type::Unit => "()".into(),
        Type::Bool => "bool".into(),
        Type::Int(_) => "int".into(),
        Type::Float(_) => "float".into(),
        Type::Str => "str".into(),
        Type::Char => "char".into(),
        Type::Array(_, _) => "array".into(),
        Type::Slice(_) => "slice".into(),
        Type::Tuple(_) => "tuple".into(),
        Type::Arrow { .. } => "fn".into(),
        Type::Nominal { .. } => "nominal".into(),
        Type::Ref { .. } => "ref".into(),
        Type::Linear(_) => "linear".into(),
        Type::Var(_) => "tyvar".into(),
    }
}
