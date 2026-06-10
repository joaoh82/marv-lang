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
//! The body is symbolically evaluated into an SMT term for `result`: `if`/`else`
//! becomes `ite`, arithmetic and comparisons map to their SMT counterparts,
//! `let` binds, parameters are SMT constants. A fresh constant `res` is
//! constrained to equal that term. Each precondition is asserted as an
//! assumption. Then, per postcondition `P`, the solver is asked whether
//! `requires ∧ res = body ∧ ¬P` is satisfiable: **unsat** proves `P`; **sat**
//! yields a model = counterexample.
//!
//! ## Loops (MARV-22)
//!
//! A [`Core::Loop`] is discharged with the standard Hoare-style verification
//! conditions over its recorded `invariant` `Inv`:
//!
//! 1. **Initiation** — under the `requires` (and the path condition guarding
//!    the loop), `Inv` holds on the initial carried state.
//! 2. **Consecution** — for an *arbitrary* (havocked) carried state satisfying
//!    `Inv ∧ cond`, the body's next-state tuple satisfies `Inv` again.
//! 3. **Use** — the loop's value is a fresh exit state about which exactly
//!    `Inv ∧ ¬cond` is assumed (guarded by the path condition), which the
//!    `ensures` discharge then consumes through the post-loop projections.
//!
//! Initiation and consecution are checked eagerly while encoding the body; a
//! `sat` answer surfaces as [`VerifyOutcome::Failed`] with a counterexample. An
//! invariant that holds but is too weak to imply an `ensures` shows up as a
//! counterexample for that postcondition — the agent's cue to strengthen it. A
//! loop *without* an invariant is still sound to pass through: nothing is
//! assumed about its exit state beyond `¬cond`.
//!
//! Contract atoms use the flat convention `marv_core::lower` emits: `Var(k)` is
//! the k-th parameter and `Var(n)` (n = arity) is `result`. Loop-invariant
//! atoms instead use de Bruijn *indices* into the loop-header environment (the
//! same convention the Tier-1 interpreter evaluates); Core erases names, so the
//! carried slots render positionally as `s0`, `s1`, … in obligations and
//! counterexamples (primed, `s0'`, for post-iteration values).

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

/// Whether a definition's body contains a loop with a recorded `invariant` —
/// i.e. carries a Tier-2 obligation even when `requires`/`ensures` are empty.
pub fn has_loop_invariant(def: &Def) -> bool {
    def.body.as_ref().is_some_and(core_has_invariant)
}

fn core_has_invariant(c: &Core) -> bool {
    match c {
        Core::Let { value, body } => core_has_invariant(value) || core_has_invariant(body),
        Core::Match { branches, .. } => branches.iter().any(|b| core_has_invariant(&b.body)),
        Core::Lam { body, .. } => core_has_invariant(body),
        Core::Loop {
            invariant,
            cond,
            body,
            ..
        } => invariant.is_some() || core_has_invariant(cond) || core_has_invariant(body),
        _ => false,
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
    // Nothing to discharge ⇒ trivially proved. A loop `invariant` is an
    // obligation of its own, so its presence forces the full discharge even
    // with no `ensures`.
    let inner = peel_lams(body);
    if def.ensures.is_empty() && !core_has_invariant(inner) {
        return VerifyOutcome::Proved;
    }
    // Parameter types must be in the scalar subset. The result type only
    // matters when an `ensures` mentions it — an invariant-only function may
    // return anything (e.g. unit).
    for t in &param_tys {
        if smt_sort_kind(t).is_none() {
            return unsupported(&format!(
                "parameter type `{}` is outside the verified subset (ints/bools)",
                show_type(t)
            ));
        }
    }
    if !def.ensures.is_empty() && smt_sort_kind(&ret_ty).is_none() {
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

    match discharge(&mut ctx, def, inner, &param_tys, &ret_ty, &label) {
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
    // Declare a constant per parameter (`p0`, `p1`, …).
    let mut consts: Vec<SExpr> = Vec::with_capacity(param_tys.len());
    let mut env: Vec<Sym> = Vec::with_capacity(param_tys.len());
    for (i, t) in param_tys.iter().enumerate() {
        let c = ctx.declare_const(format!("p{i}"), sort_of(ctx, t))?;
        consts.push(c);
        env.push(Sym::Scalar(
            c,
            smt_sort_kind(t).expect("parameter types gated to the scalar subset"),
        ));
    }

    // Assume preconditions *before* encoding the body: loop verification
    // conditions discharged during encoding must see them.
    for r in &def.requires {
        let p = encode_pred(ctx, r, &consts, None).map_err(VerifyErr::Unsupported)?;
        ctx.assert(p)?;
    }

    // Symbolically evaluate the body. Encoding a loop checks its invariant's
    // initiation and consecution obligations in place, so it can short-circuit
    // into a `Failed`/`Unsupported` outcome of its own.
    let path = ctx.true_();
    let mut enc = Encoder {
        ctx,
        params: consts.clone(),
        n_params: param_tys.len(),
        label,
        loops: 0,
    };
    let body_sym = match enc.encode(body, &mut env, path) {
        Ok(s) => s,
        Err(Stop::Unsupported(reason)) => return Ok(VerifyOutcome::Unsupported { reason }),
        Err(Stop::Outcome(o)) => return Ok(o),
        Err(Stop::Io(e)) => return Err(VerifyErr::Solver(e)),
    };

    // With no postconditions the obligations were exactly the loop invariants,
    // all discharged during encoding.
    if def.ensures.is_empty() {
        return Ok(VerifyOutcome::Proved);
    }

    // res = <symbolic body>.
    let Sym::Scalar(body_term, _) = body_sym else {
        return Ok(unsupported(
            "function result is outside the verified subset (ints/bools)",
        ));
    };
    let res = ctx.declare_const("res", sort_of(ctx, ret_ty))?;
    let eq = ctx.eq(res, body_term);
    ctx.assert(eq)?;

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

/// A symbolic value: a scalar SMT term with its sort, the carried-state tuple
/// a [`Core::Loop`] evaluates to (consumed by [`Core::Proj`]), or unit (the
/// value of an expression statement — bound but never operated on).
#[derive(Clone)]
enum Sym {
    Unit,
    Scalar(SExpr, SortKind),
    Tuple(Vec<(SExpr, SortKind)>),
}

/// Why encoding stopped: an out-of-subset construct, a finished outcome (a
/// loop-invariant counterexample or a solver `unknown`), or solver I/O failure.
enum Stop {
    Unsupported(String),
    Outcome(VerifyOutcome),
    Io(std::io::Error),
}

impl From<std::io::Error> for Stop {
    fn from(e: std::io::Error) -> Self {
        Stop::Io(e)
    }
}

fn stop(reason: &str) -> Stop {
    Stop::Unsupported(reason.to_string())
}

/// Symbolic encoder for function bodies. `env` entries are indexed by de
/// Bruijn level (atoms resolve as indices from the innermost slot, as the
/// interpreter does); `path` is the conjunction of the branch conditions
/// guarding the term being encoded, so a loop nested under an `if` generates
/// verification conditions only for the executions that actually reach it.
struct Encoder<'a, 'b> {
    ctx: &'a mut Context,
    /// Parameter constants, queried for counterexample models.
    params: Vec<SExpr>,
    n_params: usize,
    /// Flat contract-index labels (parameter names / `result`).
    label: &'b dyn Fn(u32) -> String,
    /// Loop counter for fresh havoc/exit constant names (deterministic).
    loops: u32,
}

impl Encoder<'_, '_> {
    fn encode(&mut self, c: &Core, env: &mut Vec<Sym>, path: SExpr) -> Result<Sym, Stop> {
        match c {
            Core::Atom(a) => self.atom(a, env),

            Core::Let { value, body } => {
                let v = self.encode(value, env, path)?;
                env.push(v);
                let r = self.encode(body, env, path);
                env.pop();
                r
            }

            Core::Prim { op, args } => {
                let mut a = Vec::with_capacity(args.len());
                for x in args {
                    let s = self.atom(x, env)?;
                    a.push(self.scalar(s, "primitive operand")?.0);
                }
                let (term, sort) = encode_prim(self.ctx, *op, &a).map_err(Stop::Unsupported)?;
                Ok(Sym::Scalar(term, sort))
            }

            Core::Match {
                scrutinee,
                branches,
            } => {
                // Two-arm boolean `if`/`else` only: branch 0 = false, 1 = true.
                if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
                    return Err(stop("match other than a two-arm boolean `if`/`else`"));
                }
                let s = self.atom(scrutinee, env)?;
                let (cond, _) = self.scalar(s, "match scrutinee")?;
                let not_cond = self.ctx.not(cond);
                let then_path = self.ctx.and(path, cond);
                let else_path = self.ctx.and(path, not_cond);
                let then_s = self.encode(&branches[1].body, env, then_path)?;
                let else_s = self.encode(&branches[0].body, env, else_path)?;
                match (then_s, else_s) {
                    (Sym::Unit, Sym::Unit) => Ok(Sym::Unit),
                    (Sym::Scalar(t, sort), Sym::Scalar(e, _)) => {
                        Ok(Sym::Scalar(self.ctx.ite(cond, t, e), sort))
                    }
                    _ => Err(stop("a loop-state tuple cannot flow through a `match`")),
                }
            }

            Core::Proj { base, idx } => match self.atom(base, env)? {
                Sym::Tuple(fields) => fields
                    .get(*idx as usize)
                    .map(|&(e, s)| Sym::Scalar(e, s))
                    .ok_or_else(|| stop("projection index out of range on loop state")),
                _ => Err(stop("aggregates/ADTs are outside the verified subset")),
            },

            Core::Loop {
                state,
                invariant,
                cond,
                body,
            } => self.encode_loop(state, invariant.as_deref(), cond, body, env, path),

            Core::Cast { .. } => Err(stop("`as` casts are outside the verified subset")),
            Core::Ref { .. } => Err(stop("references are outside the verified subset")),
            Core::App { .. } => Err(stop("function calls are outside the verified subset")),
            Core::Ctor { .. } | Core::Array { .. } | Core::IndexSet { .. } => {
                Err(stop("aggregates/ADTs are outside the verified subset"))
            }
            Core::Perform { .. } => Err(stop("a `perform` makes the function impure")),
            Core::Raise { .. } => Err(stop("`raise` is outside the verified subset")),
            Core::Lam { .. } => Err(stop("nested lambda is outside the verified subset")),
        }
    }

    /// Discharge a loop's verification conditions and return its value: a
    /// fresh exit-state tuple constrained by `path ⇒ Inv ∧ ¬cond`.
    fn encode_loop(
        &mut self,
        state: &[Atom],
        invariant: Option<&Pred>,
        cond: &Core,
        body: &Core,
        env: &mut Vec<Sym>,
        path: SExpr,
    ) -> Result<Sym, Stop> {
        let k = state.len();
        let header_depth = env.len();
        let id = self.loops;
        self.loops += 1;

        // Initial values of the carried slots, in the enclosing scope.
        let mut inits: Vec<(SExpr, SortKind)> = Vec::with_capacity(k);
        for a in state {
            let s = self.atom(a, env)?;
            inits.push(self.scalar(s, "loop-carried state")?);
        }

        // Invariant atoms are de Bruijn indices at loop-header depth. Carried
        // slots have no names in Core — label them positionally (`s{j}`).
        let depth = header_depth + k;
        let n_params = self.n_params;
        let outer = self.label;
        let inv_label = move |idx: u32| -> String {
            let Some(level) = depth.checked_sub(idx as usize + 1) else {
                return format!("?{idx}");
            };
            if level >= header_depth {
                format!("s{}", level - header_depth)
            } else if level < n_params {
                outer(level as u32)
            } else {
                format!("v{level}")
            }
        };

        // 1. Initiation: `path ∧ requires ⇒ Inv(init)`.
        if let Some(inv) = invariant {
            env.extend(inits.iter().map(|&(e, s)| Sym::Scalar(e, s)));
            let inv_init = self.inv_pred(inv, env)?;
            env.truncate(header_depth);

            self.ctx.push()?;
            self.ctx.assert(path)?;
            let neg = self.ctx.not(inv_init);
            self.ctx.assert(neg)?;
            match self.ctx.check()? {
                Response::Unsat => self.ctx.pop()?,
                Response::Sat => {
                    let obligation = render_pred(inv, &inv_label);
                    let mut items = self.named_params();
                    items.extend(
                        inits
                            .iter()
                            .enumerate()
                            .map(|(j, &(e, _))| (format!("s{j}"), e)),
                    );
                    let counterexample = self.values(items)?;
                    return Err(Stop::Outcome(VerifyOutcome::Failed {
                        message: format!("loop invariant `{obligation}` can fail on entry"),
                        obligation,
                        counterexample,
                    }));
                }
                Response::Unknown => {
                    return Err(Stop::Outcome(VerifyOutcome::Unsupported {
                        reason: format!(
                            "solver returned `unknown` for loop invariant `{}` (initiation)",
                            render_pred(inv, &inv_label)
                        ),
                    }));
                }
            }
        }

        // 2. Consecution: `path ∧ Inv(s) ∧ cond(s) ⇒ Inv(next(s))` for an
        // arbitrary (havocked) carried state `s`. The body walk happens even
        // without an invariant, so nested loops still get their verification
        // conditions (under the sound, weaker assumption of an unconstrained
        // outer state).
        let mut havoc: Vec<(SExpr, SortKind)> = Vec::with_capacity(k);
        for (j, &(_, s)) in inits.iter().enumerate() {
            let c = self.ctx.declare_const(format!("l{id}s{j}"), self.sort(s))?;
            havoc.push((c, s));
        }
        self.ctx.push()?;
        self.ctx.assert(path)?;
        env.extend(havoc.iter().map(|&(e, s)| Sym::Scalar(e, s)));
        let inv_h = match invariant {
            Some(inv) => {
                let p = self.inv_pred(inv, env)?;
                self.ctx.assert(p)?;
                Some(p)
            }
            None => None,
        };
        let cond_sym = self.encode(cond, env, path)?;
        let (cond_h, _) = self.scalar(cond_sym, "loop condition")?;
        self.ctx.assert(cond_h)?;
        // The path for obligations nested in the body: this iteration runs.
        let mut body_path = self.ctx.and(path, cond_h);
        if let Some(ih) = inv_h {
            body_path = self.ctx.and(body_path, ih);
        }
        let next = self.loop_next(body, env, k, body_path)?;
        if let Some(inv) = invariant {
            env.truncate(header_depth);
            env.extend(next.iter().map(|&(e, s)| Sym::Scalar(e, s)));
            let inv_next = self.inv_pred(inv, env)?;
            let neg = self.ctx.not(inv_next);
            self.ctx.assert(neg)?;
            match self.ctx.check()? {
                Response::Unsat => {}
                Response::Sat => {
                    let obligation = render_pred(inv, &inv_label);
                    let mut items = self.named_params();
                    items.extend(
                        havoc
                            .iter()
                            .enumerate()
                            .map(|(j, &(e, _))| (format!("s{j}"), e)),
                    );
                    items.extend(
                        next.iter()
                            .enumerate()
                            .map(|(j, &(e, _))| (format!("s{j}'"), e)),
                    );
                    let counterexample = self.values(items)?;
                    return Err(Stop::Outcome(VerifyOutcome::Failed {
                        message: format!(
                            "loop invariant `{obligation}` is not preserved by the loop body"
                        ),
                        obligation,
                        counterexample,
                    }));
                }
                Response::Unknown => {
                    return Err(Stop::Outcome(VerifyOutcome::Unsupported {
                        reason: format!(
                            "solver returned `unknown` for loop invariant `{}` (consecution)",
                            render_pred(inv, &inv_label)
                        ),
                    }));
                }
            }
        }
        env.truncate(header_depth);
        self.ctx.pop()?;

        // 3. Use: the loop's value is a fresh exit state, about which exactly
        // `Inv ∧ ¬cond` may be assumed — guarded by `path` so an unreachable
        // (or non-terminating) loop cannot poison the rest of the function.
        let mut exit: Vec<(SExpr, SortKind)> = Vec::with_capacity(k);
        for (j, &(_, s)) in inits.iter().enumerate() {
            let c = self.ctx.declare_const(format!("l{id}x{j}"), self.sort(s))?;
            exit.push((c, s));
        }
        env.extend(exit.iter().map(|&(e, s)| Sym::Scalar(e, s)));
        let inv_x = match invariant {
            Some(inv) => Some(self.inv_pred(inv, env)?),
            None => None,
        };
        let cond_sym = self.encode(cond, env, path)?;
        let (cond_x, _) = self.scalar(cond_sym, "loop condition")?;
        env.truncate(header_depth);
        let mut fact = self.ctx.not(cond_x);
        if let Some(ix) = inv_x {
            fact = self.ctx.and(ix, fact);
        }
        let guarded = self.ctx.imp(path, fact);
        self.ctx.assert(guarded)?;
        Ok(Sym::Tuple(exit))
    }

    /// Encode a loop body to its next-state tuple: the `let` spine runs, the
    /// terminal `Ctor` bundles the carried values, and a branch-join `Match`
    /// tail (MARV-21) merges the per-branch tuples componentwise.
    fn loop_next(
        &mut self,
        body: &Core,
        env: &mut Vec<Sym>,
        k: usize,
        path: SExpr,
    ) -> Result<Vec<(SExpr, SortKind)>, Stop> {
        match body {
            Core::Let { value, body } => {
                let v = self.encode(value, env, path)?;
                env.push(v);
                let r = self.loop_next(body, env, k, path);
                env.pop();
                r
            }
            Core::Ctor { fields, .. } => {
                if fields.len() != k {
                    return Err(stop("loop body did not produce its carried state"));
                }
                fields
                    .iter()
                    .map(|a| {
                        let s = self.atom(a, env)?;
                        self.scalar(s, "loop-carried state")
                    })
                    .collect()
            }
            Core::Match {
                scrutinee,
                branches,
            } => {
                if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
                    return Err(stop("match other than a two-arm boolean `if`/`else`"));
                }
                let s = self.atom(scrutinee, env)?;
                let (cond, _) = self.scalar(s, "match scrutinee")?;
                let not_cond = self.ctx.not(cond);
                let then_path = self.ctx.and(path, cond);
                let else_path = self.ctx.and(path, not_cond);
                let t = self.loop_next(&branches[1].body, env, k, then_path)?;
                let e = self.loop_next(&branches[0].body, env, k, else_path)?;
                Ok(t.into_iter()
                    .zip(e)
                    .map(|((te, sort), (ee, _))| (self.ctx.ite(cond, te, ee), sort))
                    .collect())
            }
            _ => Err(stop("loop body form is outside the verified subset")),
        }
    }

    /// Encode a loop-invariant predicate. Unlike `requires`/`ensures` (flat
    /// convention, [`encode_pred`]), its atoms are de Bruijn *indices* into the
    /// loop-header environment — the convention `marv_core::lower` finalizes
    /// and the Tier-1 interpreter evaluates.
    fn inv_pred(&mut self, p: &Pred, env: &[Sym]) -> Result<SExpr, Stop> {
        match p {
            Pred::True => Ok(self.ctx.true_()),
            Pred::False => Ok(self.ctx.false_()),
            Pred::Cmp(op, l, r) => {
                let x = self.inv_atom(l, env)?;
                let y = self.inv_atom(r, env)?;
                Ok(match op {
                    CmpOp::Eq => self.ctx.eq(x, y),
                    CmpOp::Ne => {
                        let e = self.ctx.eq(x, y);
                        self.ctx.not(e)
                    }
                    CmpOp::Lt => self.ctx.lt(x, y),
                    CmpOp::Le => self.ctx.lte(x, y),
                    CmpOp::Gt => self.ctx.gt(x, y),
                    CmpOp::Ge => self.ctx.gte(x, y),
                })
            }
            Pred::And(l, r) => {
                let x = self.inv_pred(l, env)?;
                let y = self.inv_pred(r, env)?;
                Ok(self.ctx.and(x, y))
            }
            Pred::Or(l, r) => {
                let x = self.inv_pred(l, env)?;
                let y = self.inv_pred(r, env)?;
                Ok(self.ctx.or(x, y))
            }
            Pred::Not(inner) => {
                let x = self.inv_pred(inner, env)?;
                Ok(self.ctx.not(x))
            }
            Pred::Forall { .. } | Pred::Exists { .. } => Err(stop(
                "bounded quantifiers are not yet in the verified subset",
            )),
        }
    }

    fn inv_atom(&self, a: &Atom, env: &[Sym]) -> Result<SExpr, Stop> {
        match a {
            Atom::Var(_) => {
                let s = self.atom(a, env)?;
                Ok(self.scalar(s, "invariant variable")?.0)
            }
            _ => match self.atom(a, env)? {
                Sym::Scalar(e, _) => Ok(e),
                _ => Err(stop("non-scalar literal in contract")),
            },
        }
    }

    /// Resolve an atom against `env` (de Bruijn index from the innermost slot,
    /// as the interpreter does).
    fn atom(&self, a: &Atom, env: &[Sym]) -> Result<Sym, Stop> {
        match a {
            Atom::Var(idx) => {
                let d = env.len();
                let i = (*idx as usize) + 1;
                if i > d {
                    return Err(stop("variable out of scope"));
                }
                Ok(env[d - i].clone())
            }
            Atom::Lit(Literal::Int(n)) => Ok(Sym::Scalar(self.ctx.numeral(*n), SortKind::Int)),
            Atom::Lit(Literal::Bool(b)) => Ok(Sym::Scalar(
                if *b {
                    self.ctx.true_()
                } else {
                    self.ctx.false_()
                },
                SortKind::Bool,
            )),
            Atom::Lit(Literal::Unit) => Ok(Sym::Unit),
            Atom::Lit(_) => Err(stop("non-scalar literal is outside the verified subset")),
            Atom::Global(_) => Err(stop("global reference is outside the verified subset")),
        }
    }

    fn scalar(&self, s: Sym, what: &str) -> Result<(SExpr, SortKind), Stop> {
        match s {
            Sym::Scalar(e, sort) => Ok((e, sort)),
            _ => Err(stop(&format!(
                "{what} is outside the verified subset (ints/bools)"
            ))),
        }
    }

    fn sort(&self, k: SortKind) -> SExpr {
        match k {
            SortKind::Bool => self.ctx.bool_sort(),
            SortKind::Int => self.ctx.int_sort(),
        }
    }

    /// Parameter constants paired with their display names.
    fn named_params(&self) -> Vec<(String, SExpr)> {
        self.params
            .iter()
            .enumerate()
            .map(|(i, &c)| ((self.label)(i as u32), c))
            .collect()
    }

    /// Read the current model's values for `items` as `(name, value)` pairs.
    fn values(&mut self, items: Vec<(String, SExpr)>) -> Result<Vec<(String, String)>, Stop> {
        let exprs: Vec<SExpr> = items.iter().map(|&(_, e)| e).collect();
        let got = self.ctx.get_value(exprs)?;
        Ok(items
            .into_iter()
            .zip(got)
            .map(|((name, _), (_, v))| (name, render_value(self.ctx, v)))
            .collect())
    }
}

fn encode_prim(ctx: &Context, op: PrimOp, a: &[SExpr]) -> Result<(SExpr, SortKind), String> {
    use PrimOp::*;
    let bin = |i: usize, j: usize| (a[i], a[j]);
    Ok(match op {
        Add => {
            let (x, y) = bin(0, 1);
            (ctx.plus(x, y), SortKind::Int)
        }
        Sub => {
            let (x, y) = bin(0, 1);
            (ctx.sub(x, y), SortKind::Int)
        }
        Mul => {
            let (x, y) = bin(0, 1);
            (ctx.times(x, y), SortKind::Int)
        }
        // SMT `div`/`mod` are Euclidean, but marv's `/`/`%` truncate toward zero;
        // rather than emit an unsound encoding, treat them as out-of-subset.
        Div | Rem => {
            return Err("integer division/remainder is not yet in the verified subset".to_string())
        }
        Eq => {
            let (x, y) = bin(0, 1);
            (ctx.eq(x, y), SortKind::Bool)
        }
        Ne => {
            let (x, y) = bin(0, 1);
            let e = ctx.eq(x, y);
            (ctx.not(e), SortKind::Bool)
        }
        Lt => {
            let (x, y) = bin(0, 1);
            (ctx.lt(x, y), SortKind::Bool)
        }
        Le => {
            let (x, y) = bin(0, 1);
            (ctx.lte(x, y), SortKind::Bool)
        }
        Gt => {
            let (x, y) = bin(0, 1);
            (ctx.gt(x, y), SortKind::Bool)
        }
        Ge => {
            let (x, y) = bin(0, 1);
            (ctx.gte(x, y), SortKind::Bool)
        }
        And => {
            let (x, y) = bin(0, 1);
            (ctx.and(x, y), SortKind::Bool)
        }
        Or => {
            let (x, y) = bin(0, 1);
            (ctx.or(x, y), SortKind::Bool)
        }
        Not => (ctx.not(a[0]), SortKind::Bool),
        // `-x` as `0 - x` (exact for SMT integer subtraction).
        Neg => {
            let zero = ctx.numeral(0);
            (ctx.sub(zero, a[0]), SortKind::Int)
        }
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

#[derive(Clone, Copy)]
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
