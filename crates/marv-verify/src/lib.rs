//! # marv-verify — SMT contract discharge (milestone M6, Tier 2)
//!
//! The verified-subset half of marv's layered verification (`spec/01` §7). Given
//! a **pure** function, [`verify_def`] discharges its `ensures` postconditions
//! (and loop `invariant`s) against the function body and its `requires`
//! preconditions using an SMT solver (z3, driven over SMT-LIB by `easy-smt`),
//! returning one of:
//!
//! - [`VerifyOutcome::Proved`] — every obligation holds for all inputs (Tier 2).
//! - [`VerifyOutcome::Failed`] — an obligation can be violated; carries a
//!   concrete **counterexample** assignment the agent can iterate against
//!   (`spec/03` §4.3).
//! - [`VerifyOutcome::Unsupported`] — the function is outside the decidable-ish
//!   subset (or the solver answered `unknown`); the honest answer is to fall
//!   back to Tier-1 runtime checks.
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
//! ## The verified subset (MARV-11)
//!
//! Beyond ints and bools, the encoding covers:
//!
//! - **Fixed-width 64-bit wrapping arithmetic (MARV-38).** Integer terms are
//!   SMT `Int`s, but every `+ - * / %` and unary `-` is wrapped back into
//!   `[i64::MIN, i64::MAX]` by [`wrap64`] (two's-complement reduction modulo
//!   2⁶⁴), and every havocked integer (parameter, ADT field, slice length,
//!   loop-carried slot) is constrained to that range. So Tier 2 computes the
//!   *same* wrapped values the runtime does (`wrapping_add`/`wrapping_mul`/… in
//!   `marv-interp`, 64-bit registers in codegen): `ensures result > x` for
//!   `x + 1` is now **refuted** with the counterexample `x = i64::MAX` (the add
//!   wraps to `i64::MIN`), where mathematical-integer encodings falsely proved
//!   it. Keeping `Int` (rather than switching the sort to `(_ BitVec 64)`) is
//!   deliberate: nonlinear bitvector reasoning is intractable here — the
//!   division identity `x == y*(x/y) + (x%y)` times out past 60 s as a 64-bit
//!   bitvector but discharges in a fraction of a second as wrapped `Int`s,
//!   and quantifiers stay over the friendly integer domain.
//! - **Truncating `/` and `%`.** SMT-LIB `div`/`mod` are Euclidean (the
//!   remainder is always non-negative) while marv truncates toward zero; the
//!   encoding corrects the Euclidean quotient by ±1 on the inexact negative
//!   cases (see [`smt_tdiv`]), so `-7 / 2` proves as `-3`, not `-4`. The
//!   wrapping pass also captures the one overflowing division — `i64::MIN / -1`
//!   wraps to `i64::MIN`, matching `wrapping_div`. Division by zero traps at
//!   runtime (Tier 1); Tier 2 treats it as an unspecified integer, which is
//!   sound for partial correctness — a trapping execution never reaches the
//!   postcondition (a counterexample whose divisor is 0 may thus be spurious).
//! - **Arrays and slices** of ints/bools, as SMT arrays paired with a length
//!   term (`[N]T` has a literal length; a slice's is an unconstrained
//!   non-negative constant). `len`, indexing, array literals, and slice element
//!   stores all encode; out-of-bounds reads are unspecified values (the runtime
//!   traps — same partial-correctness argument as division).
//! - **Structs and enums**, encoded *unpacked*: a value is an integer tag plus
//!   per-variant field terms (no SMT datatypes needed). Construction, `match`
//!   (branch-joined with `ite`), and struct projection encode; parameters of
//!   nominal type are havocked from their declaration in the [`World`]
//!   (recursive types are honestly `unsupported`).
//! - **Bounded quantifiers** `forall i in lo..hi: P` / `exists …` in contracts
//!   and invariants, encoded as guarded SMT quantifiers over the integers.
//! - **`old(e)`** in `ensures` — erased at lowering (parameters are immutable
//!   values, so the pre-state of a contract expression is the expression).
//!
//! Everything else stays an explicit `unsupported`, never a silent wrong
//! `proved`. One honest caveat remains: quantifiers plus nonlinear arithmetic
//! can drive the solver to `unknown`, which reports as `unsupported` (a
//! per-query soft timeout keeps `verify` from hanging). Note that modeling
//! wrapping makes Tier 2 *correctly stricter* — a contract that silently
//! relied on unbounded integers (e.g. a loop accumulator claimed `>= 0` whose
//! sum can overflow) now yields a counterexample instead of an unsound proof.
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
//! assumed about its exit state beyond `¬cond`. Carried state may be scalar,
//! array, or (declared, non-recursive) ADT-valued.
//!
//! Contract atoms use the flat convention `marv_core::lower` emits: `Var(k)` is
//! the k-th parameter and `Var(n)` (n = arity) is `result`. Loop-invariant
//! atoms instead use de Bruijn *indices* into the loop-header environment (the
//! same convention the Tier-1 interpreter evaluates); Core erases names, so the
//! carried slots render positionally as `s0`, `s1`, … in obligations and
//! counterexamples (primed, `s0'`, for post-iteration values), and quantifier
//! binders render as `i`, `i1`, ….

use easy_smt::{Context, ContextBuilder, Response, SExpr};

use marv_core::ir::*;
use marv_core::{render_pred_with, PredVars};
use marv_types::World;

/// Soft per-query solver timeout (milliseconds). Quantifiers and nonlinear
/// arithmetic can make z3 diverge; past this budget it answers `unknown`,
/// which reports as an honest `unsupported`.
const SOLVER_TIMEOUT_MS: i64 = 10_000;

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
/// counterexamples and obligation messages (missing names render as `arg{i}`);
/// `world` resolves nominal (struct/enum) parameter and carried-state types so
/// they can be havocked — pass an empty [`World`] when no declarations are
/// available, at the cost of ADT-typed parameters reporting `unsupported`.
pub fn verify_def(def: &Def, param_names: &[String], world: &World) -> VerifyOutcome {
    // Only pure functions with a body are in the verified subset.
    if def.kind != DefKind::Fn {
        return unsupported("only functions carry verifiable contracts");
    }
    let Some(body) = &def.body else {
        return unsupported("function has no body to verify");
    };

    let (param_tys, _ret_ty, eff) = peel_arrow(&def.ty);
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
    // Best-effort soft timeout so a diverging query answers `unknown` instead
    // of hanging `verify`.
    let timeout = ctx.numeral(SOLVER_TIMEOUT_MS);
    let _ = ctx.set_option(":timeout", timeout);

    match discharge(&mut ctx, def, inner, &param_tys, &label, world) {
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
    label: &dyn Fn(u32) -> String,
    world: &World,
) -> Result<VerifyOutcome, VerifyErr> {
    let path = ctx.true_();
    let mut enc = Encoder {
        ctx,
        params: Vec::new(),
        n_params: param_tys.len(),
        label,
        loops: 0,
        fresh: 0,
        world,
    };

    // Havoc one symbolic value per parameter (`p0`, `p1`, …).
    for (i, t) in param_tys.iter().enumerate() {
        let s = match enc.havoc_type(t, &format!("p{i}"), &mut Vec::new()) {
            Ok(s) => s,
            Err(Stop::Unsupported(reason)) => {
                return Ok(VerifyOutcome::Unsupported {
                    reason: format!("parameter `{}`: {reason}", label(i as u32)),
                })
            }
            Err(Stop::Outcome(o)) => return Ok(o),
            Err(Stop::Io(e)) => return Err(VerifyErr::Solver(e)),
        };
        enc.params.push(s);
    }

    // Assume preconditions *before* encoding the body: loop verification
    // conditions discharged during encoding must see them.
    for r in &def.requires {
        let p = encode_flat_pred(enc.ctx, r, &enc.params, None, &mut Vec::new())
            .map_err(VerifyErr::Unsupported)?;
        enc.ctx.assert(p)?;
    }

    // Symbolically evaluate the body. Encoding a loop checks its invariant's
    // initiation and consecution obligations in place, so it can short-circuit
    // into a `Failed`/`Unsupported` outcome of its own.
    let mut env: Vec<Sym> = enc.params.clone();
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

    // `res = <symbolic body>` — a named constant for scalar results (it reads
    // well in counterexamples); aggregate results flow through as-is.
    let params = enc.params.clone();
    let result_sym = match body_sym {
        Sym::Scalar(term, kind) => {
            let res = enc.ctx.declare_const("res", enc.sort(kind))?;
            let eq = enc.ctx.eq(res, term);
            enc.ctx.assert(eq)?;
            Sym::Scalar(res, kind)
        }
        other => other,
    };

    // Discharge each postcondition: prove ¬(requires ∧ body ∧ ¬ensures) is unsat.
    let arity = params.len() as u32;
    for ens in &def.ensures {
        let formula = encode_flat_pred(enc.ctx, ens, &params, Some(&result_sym), &mut Vec::new())
            .map_err(VerifyErr::Unsupported)?;
        enc.ctx.push()?;
        let negated = enc.ctx.not(formula);
        enc.ctx.assert(negated)?;
        let resp = enc.ctx.check()?;
        match resp {
            Response::Unsat => {
                enc.ctx.pop()?;
            }
            Response::Sat => {
                let mut items = enc.named_params();
                model_items(&label(arity), &result_sym, &mut items);
                let counterexample = match enc.values(items) {
                    Ok(v) => v,
                    Err(Stop::Io(e)) => return Err(VerifyErr::Solver(e)),
                    Err(Stop::Unsupported(r)) => return Err(VerifyErr::Unsupported(r)),
                    Err(Stop::Outcome(o)) => return Ok(o),
                };
                let obligation = render_pred_with(ens, label, PredVars::Flat { arity });
                return Ok(VerifyOutcome::Failed {
                    message: format!("postcondition `{obligation}` can be violated"),
                    obligation,
                    counterexample,
                });
            }
            Response::Unknown => {
                enc.ctx.pop()?;
                return Ok(VerifyOutcome::Unsupported {
                    reason: format!(
                        "solver returned `unknown` for `{}`",
                        render_pred_with(ens, label, PredVars::Flat { arity })
                    ),
                });
            }
        }
    }

    Ok(VerifyOutcome::Proved)
}

/// Render a model value: an integer, a boolean, or its raw s-expression text
/// (array models print as their store-chain).
fn render_value(ctx: &Context, v: SExpr) -> String {
    if let Some(n) = ctx.get_i64(v) {
        return n.to_string();
    }
    // `i64::MIN` comes back as `(- 9223372036854775808)`: its magnitude 2⁶³
    // overflows `i64` before negation, so `get_i64` fails. Widen to `i128`.
    if let Some(n) = ctx.get_i128(v) {
        return n.to_string();
    }
    match ctx.get_atom(v) {
        Some("true") => "true".to_string(),
        Some("false") => "false".to_string(),
        Some(a) => a.to_string(),
        None => ctx.display(v).to_string(),
    }
}

// ---- symbolic values -----------------------------------------------------

/// A symbolic value:
///
/// - `Scalar` — an int/bool SMT term.
/// - `Array` — an SMT array (`Int → elem`) paired with its length term.
/// - `Adt` — a struct/enum in the *unpacked* encoding: an integer `tag` term
///   plus, per variant, its field values (`None` when this value can never
///   carry that variant, e.g. a direct `Ctor`). Products are tag 0 with a
///   single variant.
/// - `Tuple` — the carried-state bundle a [`Core::Loop`] evaluates to
///   (consumed by [`Core::Proj`]).
/// - `Unit` — the value of an expression statement; bound but never operated
///   on.
#[derive(Clone)]
enum Sym {
    Unit,
    Scalar(SExpr, SortKind),
    Array {
        arr: SExpr,
        len: SExpr,
        elem: SortKind,
    },
    Adt {
        ty: Hash,
        tag: SExpr,
        variants: Vec<Option<Vec<Sym>>>,
    },
    Tuple(Vec<Sym>),
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

/// Flatten a symbolic value into displayable `(name, term)` pairs for a
/// counterexample model: scalars directly, arrays as their length plus the
/// array term, a struct as its fields (positionally — names are erased), an
/// enum as its tag (which variant is live is model-dependent, so per-variant
/// fields would mislead).
fn model_items(name: &str, s: &Sym, out: &mut Vec<(String, SExpr)>) {
    match s {
        Sym::Unit => {}
        Sym::Scalar(e, _) => out.push((name.to_string(), *e)),
        Sym::Array { arr, len, .. } => {
            out.push((format!("len({name})"), *len));
            out.push((name.to_string(), *arr));
        }
        Sym::Adt { tag, variants, .. } => {
            if let [Some(fields)] = variants.as_slice() {
                for (i, f) in fields.iter().enumerate() {
                    model_items(&format!("{name}.{i}"), f, out);
                }
            } else {
                out.push((format!("{name}.tag"), *tag));
            }
        }
        Sym::Tuple(items) => {
            for (i, item) in items.iter().enumerate() {
                model_items(&format!("{name}.{i}"), item, out);
            }
        }
    }
}

/// Symbolic encoder for function bodies. `env` entries are indexed by de
/// Bruijn level (atoms resolve as indices from the innermost slot, as the
/// interpreter does); `path` is the conjunction of the branch conditions
/// guarding the term being encoded, so a loop nested under an `if` generates
/// verification conditions only for the executions that actually reach it.
struct Encoder<'a, 'b> {
    ctx: &'a mut Context,
    /// Parameter values, queried for counterexample models.
    params: Vec<Sym>,
    n_params: usize,
    /// Flat contract-index labels (parameter names / `result`).
    label: &'b dyn Fn(u32) -> String,
    /// Loop counter for fresh havoc/exit constant names (deterministic).
    loops: u32,
    /// Counter for other fresh constant names (arrays, ADT fields).
    fresh: u32,
    /// Struct/enum declarations, for havocking nominal-typed values.
    world: &'b World,
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
                let mut syms = Vec::with_capacity(args.len());
                for x in args {
                    syms.push(self.atom(x, env)?);
                }
                self.prim(*op, &syms)
            }

            Core::Match {
                scrutinee,
                branches,
            } => {
                let s = self.atom(scrutinee, env)?;
                self.encode_match(s, branches, env, path)
            }

            Core::Proj { base, idx } => match self.atom(base, env)? {
                Sym::Tuple(fields) => fields
                    .get(*idx as usize)
                    .cloned()
                    .ok_or_else(|| stop("projection index out of range on loop state")),
                // Struct field projection: products are single-variant.
                Sym::Adt { variants, .. } if variants.len() == 1 => variants[0]
                    .as_ref()
                    .and_then(|fs| fs.get(*idx as usize).cloned())
                    .ok_or_else(|| stop("projection index out of range on struct")),
                _ => Err(stop("projection on a multi-variant enum value")),
            },

            Core::Ctor { ty, tag, fields } => {
                let mut fs = Vec::with_capacity(fields.len());
                for a in fields {
                    fs.push(self.atom(a, env)?);
                }
                // Size the variant table from the declaration when known, so
                // `match` joins line up; an unknown hash (e.g. the synthetic
                // loop-state bundle) gets exactly its own slot.
                let n = if let Some(e) = self.world.enum_decl(ty) {
                    e.variants.len()
                } else if self.world.struct_decl(ty).is_some() {
                    1
                } else {
                    *tag as usize + 1
                };
                let mut variants: Vec<Option<Vec<Sym>>> = vec![None; n.max(*tag as usize + 1)];
                variants[*tag as usize] = Some(fs);
                Ok(Sym::Adt {
                    ty: *ty,
                    tag: self.ctx.numeral(*tag as i64),
                    variants,
                })
            }

            Core::Array { elem, items } => {
                let Some(elem_kind) = smt_sort_kind(elem) else {
                    return Err(stop(
                        "array element type is outside the verified subset (ints/bools)",
                    ));
                };
                let name = self.fresh_name("arr");
                let sort = self.array_sort(elem_kind);
                let mut arr = self.ctx.declare_const(name, sort)?;
                for (i, item) in items.iter().enumerate() {
                    let s = self.atom(item, env)?;
                    let (v, _) = self.scalar(s, "array element")?;
                    let idx = self.ctx.numeral(i as i64);
                    arr = self.ctx.store(arr, idx, v);
                }
                Ok(Sym::Array {
                    arr,
                    len: self.ctx.numeral(items.len() as i64),
                    elem: elem_kind,
                })
            }

            Core::IndexSet { base, index, value } => {
                let b = self.atom(base, env)?;
                let Sym::Array { arr, len, elem } = b else {
                    return Err(stop("element store on a non-array value"));
                };
                let i = self.atom(index, env)?;
                let (i, _) = self.scalar(i, "store index")?;
                let v = self.atom(value, env)?;
                let (v, _) = self.scalar(v, "stored element")?;
                // An out-of-bounds store traps at runtime (Tier 1, MARV-34);
                // here it writes a phantom location no in-bounds read sees —
                // sound for partial correctness.
                Ok(Sym::Array {
                    arr: self.ctx.store(arr, i, v),
                    len,
                    elem,
                })
            }

            Core::Loop {
                state,
                invariant,
                cond,
                body,
            } => self.encode_loop(state, invariant.as_deref(), cond, body, env, path),

            Core::Cast { .. } => Err(stop("`as` casts are outside the verified subset")),
            Core::Ref { .. } => Err(stop("references are outside the verified subset")),
            Core::App { .. } => Err(stop("function calls are outside the verified subset")),
            Core::Perform { .. } => Err(stop("a `perform` makes the function impure")),
            Core::Raise { .. } => Err(stop("`raise` is outside the verified subset")),
            Core::Lam { .. } => Err(stop("nested lambda is outside the verified subset")),
        }
    }

    /// Encode a primitive over already-resolved operands. `len`/`index` consume
    /// arrays; everything else is scalar.
    fn prim(&mut self, op: PrimOp, args: &[Sym]) -> Result<Sym, Stop> {
        match op {
            PrimOp::Len => match args.first() {
                Some(Sym::Array { len, .. }) => Ok(Sym::Scalar(*len, SortKind::Int)),
                _ => Err(stop("`len` of a non-array value")),
            },
            PrimOp::Index => match (args.first(), args.get(1)) {
                (Some(Sym::Array { arr, elem, .. }), Some(Sym::Scalar(i, _))) => {
                    // An out-of-bounds read traps at runtime (Tier 1, MARV-34);
                    // here it is an unspecified value — sound for partial
                    // correctness.
                    Ok(Sym::Scalar(self.ctx.select(*arr, *i), *elem))
                }
                _ => Err(stop("`index` of a non-array value")),
            },
            _ => {
                let mut scalars = Vec::with_capacity(args.len());
                for s in args {
                    scalars.push(self.scalar(s.clone(), "primitive operand")?.0);
                }
                let (term, sort) =
                    encode_prim(self.ctx, op, &scalars).map_err(Stop::Unsupported)?;
                Ok(Sym::Scalar(term, sort))
            }
        }
    }

    /// Encode a `match`: a two-arm boolean `if`/`else` over a scalar bool, or a
    /// tag-indexed match over an (unpacked) enum value, branch results joined
    /// componentwise with `ite`.
    fn encode_match(
        &mut self,
        scrutinee: Sym,
        branches: &[Branch],
        env: &mut Vec<Sym>,
        path: SExpr,
    ) -> Result<Sym, Stop> {
        match scrutinee {
            Sym::Scalar(cond, SortKind::Bool) => {
                if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
                    return Err(stop("boolean match must be a two-arm `if`/`else`"));
                }
                let not_cond = self.ctx.not(cond);
                let then_path = self.ctx.and(path, cond);
                let else_path = self.ctx.and(path, not_cond);
                let then_s = self.encode(&branches[1].body, env, then_path)?;
                let else_s = self.encode(&branches[0].body, env, else_path)?;
                self.merge(cond, then_s, else_s)
            }
            Sym::Adt { tag, variants, .. } => {
                if branches.is_empty() {
                    return Err(stop("match with no branches"));
                }
                let mut arms: Vec<(SExpr, Sym)> = Vec::with_capacity(branches.len());
                for (k, br) in branches.iter().enumerate() {
                    let tag_k = self.ctx.numeral(k as i64);
                    let cond_k = self.ctx.eq(tag, tag_k);
                    let path_k = self.ctx.and(path, cond_k);
                    let depth = env.len();
                    let binds = br.binds as usize;
                    if binds > 0 {
                        let Some(Some(fields)) = variants.get(k) else {
                            return Err(stop(
                                "enum value's fields for a matched variant are unknown",
                            ));
                        };
                        if fields.len() != binds {
                            return Err(stop("match branch arity does not fit the variant"));
                        }
                        env.extend(fields.iter().cloned());
                    }
                    let r = self.encode(&br.body, env, path_k);
                    env.truncate(depth);
                    arms.push((cond_k, r?));
                }
                // Join right-to-left: the last branch is the `ite` default.
                let (_, mut acc) = arms.pop().expect("at least one branch");
                for (cond_k, r) in arms.into_iter().rev() {
                    acc = self.merge(cond_k, r, acc)?;
                }
                Ok(acc)
            }
            _ => Err(stop("match scrutinee is outside the verified subset")),
        }
    }

    /// Join two branch values with `ite(cond, t, e)`, componentwise for
    /// aggregates. For ADTs, a variant populated on only one side is taken
    /// as-is — reading variant k's fields is only ever guarded by `tag == k`,
    /// which implies the side that carried them.
    fn merge(&mut self, cond: SExpr, t: Sym, e: Sym) -> Result<Sym, Stop> {
        match (t, e) {
            (Sym::Unit, Sym::Unit) => Ok(Sym::Unit),
            (Sym::Scalar(a, sort), Sym::Scalar(b, _)) => {
                Ok(Sym::Scalar(self.ctx.ite(cond, a, b), sort))
            }
            (
                Sym::Array {
                    arr: aa,
                    len: la,
                    elem,
                },
                Sym::Array {
                    arr: ab, len: lb, ..
                },
            ) => Ok(Sym::Array {
                arr: self.ctx.ite(cond, aa, ab),
                len: self.ctx.ite(cond, la, lb),
                elem,
            }),
            (
                Sym::Adt {
                    ty,
                    tag: ta,
                    variants: va,
                },
                Sym::Adt {
                    tag: tb,
                    variants: vb,
                    ..
                },
            ) => {
                let n = va.len().max(vb.len());
                let mut variants: Vec<Option<Vec<Sym>>> = Vec::with_capacity(n);
                for k in 0..n {
                    let a = va.get(k).cloned().flatten();
                    let b = vb.get(k).cloned().flatten();
                    variants.push(match (a, b) {
                        (Some(fa), Some(fb)) => {
                            if fa.len() != fb.len() {
                                return Err(stop("branches build the same variant differently"));
                            }
                            let mut fs = Vec::with_capacity(fa.len());
                            for (x, y) in fa.into_iter().zip(fb) {
                                fs.push(self.merge(cond, x, y)?);
                            }
                            Some(fs)
                        }
                        (one, other) => one.or(other),
                    });
                }
                Ok(Sym::Adt {
                    ty,
                    tag: self.ctx.ite(cond, ta, tb),
                    variants,
                })
            }
            (Sym::Tuple(a), Sym::Tuple(b)) => {
                if a.len() != b.len() {
                    return Err(stop("branches produce differently-sized loop states"));
                }
                let mut items = Vec::with_capacity(a.len());
                for (x, y) in a.into_iter().zip(b) {
                    items.push(self.merge(cond, x, y)?);
                }
                Ok(Sym::Tuple(items))
            }
            _ => Err(stop("branches produce differently-shaped values")),
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
        let mut inits: Vec<Sym> = Vec::with_capacity(k);
        for a in state {
            inits.push(self.atom(a, env)?);
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
        let render_inv =
            |inv: &Pred| -> String { render_pred_with(inv, &inv_label, PredVars::DeBruijn) };

        // 1. Initiation: `path ∧ requires ⇒ Inv(init)`.
        if let Some(inv) = invariant {
            env.extend(inits.iter().cloned());
            let inv_init = self.inv_pred(inv, env)?;
            env.truncate(header_depth);

            self.ctx.push()?;
            self.ctx.assert(path)?;
            let neg = self.ctx.not(inv_init);
            self.ctx.assert(neg)?;
            match self.ctx.check()? {
                Response::Unsat => self.ctx.pop()?,
                Response::Sat => {
                    let obligation = render_inv(inv);
                    let mut items = self.named_params();
                    for (j, s) in inits.iter().enumerate() {
                        model_items(&format!("s{j}"), s, &mut items);
                    }
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
                            render_inv(inv)
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
        let mut havoc: Vec<Sym> = Vec::with_capacity(k);
        for (j, init) in inits.iter().enumerate() {
            let h = self.havoc_like(init, &format!("l{id}s{j}"))?;
            havoc.push(h);
        }
        self.ctx.push()?;
        self.ctx.assert(path)?;
        env.extend(havoc.iter().cloned());
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
        let body_sym = self.encode(body, env, body_path)?;
        let next = as_state(body_sym, k)?;
        if let Some(inv) = invariant {
            env.truncate(header_depth);
            env.extend(next.iter().cloned());
            let inv_next = self.inv_pred(inv, env)?;
            let neg = self.ctx.not(inv_next);
            self.ctx.assert(neg)?;
            match self.ctx.check()? {
                Response::Unsat => {}
                Response::Sat => {
                    let obligation = render_inv(inv);
                    let mut items = self.named_params();
                    for (j, s) in havoc.iter().enumerate() {
                        model_items(&format!("s{j}"), s, &mut items);
                    }
                    for (j, s) in next.iter().enumerate() {
                        model_items(&format!("s{j}'"), s, &mut items);
                    }
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
                            render_inv(inv)
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
        let mut exit: Vec<Sym> = Vec::with_capacity(k);
        for (j, init) in inits.iter().enumerate() {
            let x = self.havoc_like(init, &format!("l{id}x{j}"))?;
            exit.push(x);
        }
        env.extend(exit.iter().cloned());
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

    /// Encode a loop-invariant predicate. Unlike `requires`/`ensures` (flat
    /// convention, [`encode_flat_pred`]), its variables are de Bruijn *indices*
    /// into the loop-header environment — the convention `marv_core::lower`
    /// finalizes and the Tier-1 interpreter evaluates. A quantifier binds index
    /// 0 within its body, so it pushes its bound variable as the innermost slot.
    fn inv_pred(&mut self, p: &Pred, env: &mut Vec<Sym>) -> Result<SExpr, Stop> {
        match p {
            Pred::True => Ok(self.ctx.true_()),
            Pred::False => Ok(self.ctx.false_()),
            Pred::Cmp(op, l, r) => {
                let x = self.inv_cexpr(l, env)?;
                let y = self.inv_cexpr(r, env)?;
                encode_cmp(self.ctx, *op, &x, &y).map_err(Stop::Unsupported)
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
            Pred::Forall { domain, body } | Pred::Exists { domain, body } => {
                let exists = matches!(p, Pred::Exists { .. });
                let lo = self.inv_cexpr(&domain.0, env)?;
                let (lo, _) = self.scalar(lo, "quantifier bound")?;
                let hi = self.inv_cexpr(&domain.1, env)?;
                let (hi, _) = self.scalar(hi, "quantifier bound")?;
                let name = format!("qi{}", env.len());
                let qv = self.ctx.atom(name.as_str());
                env.push(Sym::Scalar(qv, SortKind::Int));
                let inner = self.inv_pred(body, env);
                env.pop();
                let inner = inner?;
                Ok(quantify(self.ctx, exists, &name, qv, lo, hi, inner))
            }
        }
    }

    /// Encode a loop-invariant contract expression against the environment.
    fn inv_cexpr(&mut self, e: &CExpr, env: &mut Vec<Sym>) -> Result<Sym, Stop> {
        match e {
            CExpr::Atom(a) => self.atom(a, env),
            CExpr::Node(n) => match &**n {
                CNode::Bin(op, l, r) => {
                    let x = self.inv_cexpr(l, env)?;
                    let (x, _) = self.scalar(x, "contract operand")?;
                    let y = self.inv_cexpr(r, env)?;
                    let (y, _) = self.scalar(y, "contract operand")?;
                    Ok(Sym::Scalar(smt_arith(self.ctx, *op, x, y), SortKind::Int))
                }
                CNode::Neg(inner) => {
                    let x = self.inv_cexpr(inner, env)?;
                    let (x, _) = self.scalar(x, "contract operand")?;
                    let zero = self.ctx.numeral(0);
                    let neg = self.ctx.sub(zero, x);
                    Ok(Sym::Scalar(wrap64(self.ctx, neg), SortKind::Int))
                }
                CNode::Len(inner) => match self.inv_cexpr(inner, env)? {
                    Sym::Array { len, .. } => Ok(Sym::Scalar(len, SortKind::Int)),
                    _ => Err(stop("`len` of a non-array value in a contract")),
                },
                CNode::Index(base, index) => {
                    let b = self.inv_cexpr(base, env)?;
                    let i = self.inv_cexpr(index, env)?;
                    let (i, _) = self.scalar(i, "contract index")?;
                    match b {
                        Sym::Array { arr, elem, .. } => {
                            Ok(Sym::Scalar(self.ctx.select(arr, i), elem))
                        }
                        _ => Err(stop("indexing a non-array value in a contract")),
                    }
                }
                CNode::Proj(base, idx) => {
                    let b = self.inv_cexpr(base, env)?;
                    adt_field(&b, *idx).map_err(Stop::Unsupported)
                }
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

    fn array_sort(&self, elem: SortKind) -> SExpr {
        let int = self.ctx.int_sort();
        self.ctx.array_sort(int, self.sort(elem))
    }

    /// Constrain a havocked integer term to the runtime's value range
    /// `[i64::MIN, i64::MAX]`, so models pick only values the program could
    /// actually compute. Arithmetic results are already kept in range by
    /// [`wrap64`]; this pins down the *free* integers (parameters, ADT fields,
    /// loop-carried slots) the solver is otherwise free to send to infinity.
    fn assert_int_range(&mut self, v: SExpr) -> Result<(), Stop> {
        let lo = self.ctx.numeral(i64::MIN);
        let hi = self.ctx.numeral(i64::MAX);
        let ge = self.ctx.gte(v, lo);
        let le = self.ctx.lte(v, hi);
        let both = self.ctx.and(ge, le);
        self.ctx.assert(both)?;
        Ok(())
    }

    /// Constrain a havocked length term to `[0, i64::MAX]` — a non-negative
    /// count no larger than the runtime can address. The upper bound matters:
    /// it lets the prover see that an in-bounds index `i < len` cannot overflow
    /// when stepped (`i + 1`).
    fn assert_len_range(&mut self, len: SExpr) -> Result<(), Stop> {
        let lo = self.ctx.numeral(0);
        let hi = self.ctx.numeral(i64::MAX);
        let ge = self.ctx.gte(len, lo);
        let le = self.ctx.lte(len, hi);
        let both = self.ctx.and(ge, le);
        self.ctx.assert(both)?;
        Ok(())
    }

    fn fresh_name(&mut self, prefix: &str) -> String {
        let id = self.fresh;
        self.fresh += 1;
        format!("{prefix}{id}")
    }

    /// Declare a fresh symbolic value of type `t` named after `name` — a
    /// parameter or a havocked ADT field. `visiting` carries the nominal hashes
    /// currently being expanded, so a recursive type is an honest
    /// `unsupported` instead of an infinite expansion.
    fn havoc_type(&mut self, t: &Type, name: &str, visiting: &mut Vec<Hash>) -> Result<Sym, Stop> {
        match t {
            Type::Unit => Ok(Sym::Unit),
            Type::Bool => {
                let c = self.ctx.declare_const(name, self.ctx.bool_sort())?;
                Ok(Sym::Scalar(c, SortKind::Bool))
            }
            Type::Int(_) => {
                let c = self.ctx.declare_const(name, self.ctx.int_sort())?;
                self.assert_int_range(c)?;
                Ok(Sym::Scalar(c, SortKind::Int))
            }
            Type::Array(elem, n) => {
                let Some(elem_kind) = smt_sort_kind(elem) else {
                    return Err(stop(
                        "array element type is outside the verified subset (ints/bools)",
                    ));
                };
                let sort = self.array_sort(elem_kind);
                let arr = self.ctx.declare_const(name, sort)?;
                Ok(Sym::Array {
                    arr,
                    len: self.ctx.numeral(*n as i64),
                    elem: elem_kind,
                })
            }
            Type::Slice(elem) => {
                let Some(elem_kind) = smt_sort_kind(elem) else {
                    return Err(stop(
                        "slice element type is outside the verified subset (ints/bools)",
                    ));
                };
                let sort = self.array_sort(elem_kind);
                let arr = self.ctx.declare_const(name, sort)?;
                let len = self
                    .ctx
                    .declare_const(format!("{name}_len"), self.ctx.int_sort())?;
                self.assert_len_range(len)?;
                Ok(Sym::Array {
                    arr,
                    len,
                    elem: elem_kind,
                })
            }
            Type::Linear(inner) => self.havoc_type(inner, name, visiting),
            Type::Nominal { def, args } => {
                if !args.is_empty() {
                    return Err(stop("generic ADTs are outside the verified subset"));
                }
                if visiting.contains(def) {
                    return Err(stop("recursive types are outside the verified subset"));
                }
                visiting.push(*def);
                let r = self.havoc_nominal(def, name, visiting);
                visiting.pop();
                r
            }
            _ => Err(stop("type is outside the verified subset")),
        }
    }

    /// Havoc a struct/enum value from its declaration: a fresh tag constrained
    /// to the variant range plus fresh fields for *every* variant.
    fn havoc_nominal(
        &mut self,
        def: &Hash,
        name: &str,
        visiting: &mut Vec<Hash>,
    ) -> Result<Sym, Stop> {
        if let Some(s) = self.world.struct_decl(def) {
            let fields = s.fields.clone();
            let mut fs = Vec::with_capacity(fields.len());
            for (i, ft) in fields.iter().enumerate() {
                fs.push(self.havoc_type(ft, &format!("{name}_f{i}"), visiting)?);
            }
            return Ok(Sym::Adt {
                ty: *def,
                tag: self.ctx.numeral(0),
                variants: vec![Some(fs)],
            });
        }
        if let Some(e) = self.world.enum_decl(def) {
            let decl_variants = e.variants.clone();
            let tag = self
                .ctx
                .declare_const(format!("{name}_tag"), self.ctx.int_sort())?;
            let zero = self.ctx.numeral(0);
            let n = self.ctx.numeral(decl_variants.len() as i64);
            let lo = self.ctx.gte(tag, zero);
            let hi = self.ctx.lt(tag, n);
            let bounds = self.ctx.and(lo, hi);
            self.ctx.assert(bounds)?;
            let mut variants = Vec::with_capacity(decl_variants.len());
            for (k, v) in decl_variants.iter().enumerate() {
                let mut fs = Vec::with_capacity(v.fields.len());
                for (i, ft) in v.fields.iter().enumerate() {
                    fs.push(self.havoc_type(ft, &format!("{name}_v{k}f{i}"), visiting)?);
                }
                variants.push(Some(fs));
            }
            return Ok(Sym::Adt {
                ty: *def,
                tag,
                variants,
            });
        }
        Err(stop("nominal type has no known struct/enum declaration"))
    }

    /// A fresh symbolic value with the same *shape* as `model` — used to havoc
    /// loop-carried state for consecution and exit.
    fn havoc_like(&mut self, model: &Sym, name: &str) -> Result<Sym, Stop> {
        match model {
            Sym::Unit => Ok(Sym::Unit),
            Sym::Scalar(_, kind) => {
                let sort = self.sort(*kind);
                let c = self.ctx.declare_const(name, sort)?;
                if matches!(kind, SortKind::Int) {
                    self.assert_int_range(c)?;
                }
                Ok(Sym::Scalar(c, *kind))
            }
            Sym::Array { elem, .. } => {
                let sort = self.array_sort(*elem);
                let arr = self.ctx.declare_const(name, sort)?;
                let len = self
                    .ctx
                    .declare_const(format!("{name}_len"), self.ctx.int_sort())?;
                self.assert_len_range(len)?;
                Ok(Sym::Array {
                    arr,
                    len,
                    elem: *elem,
                })
            }
            // A carried ADT havocs from its declaration (all variants live).
            Sym::Adt { ty, .. } => self.havoc_nominal(&ty.clone(), name, &mut vec![*ty]),
            Sym::Tuple(_) => Err(stop("nested loop state cannot be havocked")),
        }
    }

    /// Parameter values flattened into displayable model items.
    fn named_params(&self) -> Vec<(String, SExpr)> {
        let mut out = Vec::new();
        for (i, s) in self.params.iter().enumerate() {
            model_items(&(self.label)(i as u32), s, &mut out);
        }
        out
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

/// Unbundle a loop body's value into its carried-state vector: the terminal
/// `Ctor` encodes as a single-variant ADT (or the body is `Unit` for a loop
/// that carries nothing).
fn as_state(s: Sym, k: usize) -> Result<Vec<Sym>, Stop> {
    let fields = match s {
        Sym::Adt { mut variants, .. } if variants.len() == 1 => variants
            .pop()
            .flatten()
            .ok_or_else(|| stop("loop body did not produce its carried state"))?,
        Sym::Tuple(fields) => fields,
        Sym::Unit if k == 0 => Vec::new(),
        _ => return Err(stop("loop body did not produce its carried state")),
    };
    if fields.len() != k {
        return Err(stop("loop body did not produce its carried state"));
    }
    Ok(fields)
}

// ---- primitive & arithmetic encoding --------------------------------------

fn encode_prim(ctx: &Context, op: PrimOp, a: &[SExpr]) -> Result<(SExpr, SortKind), String> {
    use PrimOp::*;
    let bin = |i: usize, j: usize| (a[i], a[j]);
    Ok(match op {
        Add => {
            let (x, y) = bin(0, 1);
            (smt_arith(ctx, ArithOp::Add, x, y), SortKind::Int)
        }
        Sub => {
            let (x, y) = bin(0, 1);
            (smt_arith(ctx, ArithOp::Sub, x, y), SortKind::Int)
        }
        Mul => {
            let (x, y) = bin(0, 1);
            (smt_arith(ctx, ArithOp::Mul, x, y), SortKind::Int)
        }
        // Truncate-toward-zero division/remainder, corrected from SMT's
        // Euclidean `div`/`mod` (see [`smt_tdiv`]).
        Div => {
            let (x, y) = bin(0, 1);
            (smt_arith(ctx, ArithOp::Div, x, y), SortKind::Int)
        }
        Rem => {
            let (x, y) = bin(0, 1);
            (smt_arith(ctx, ArithOp::Rem, x, y), SortKind::Int)
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
        // `-x` as `0 - x`, wrapped — only `-i64::MIN` overflows.
        Neg => {
            let zero = ctx.numeral(0);
            (wrap64(ctx, ctx.sub(zero, a[0])), SortKind::Int)
        }
        Len | Index => return Err("len/index of a non-array value".to_string()),
    })
}

/// Reduce an unbounded integer term into the two's-complement range
/// `[i64::MIN, i64::MAX]` — the fixed-width wrap marv's runtime performs
/// (`wrapping_add`/`wrapping_mul`/…). `wrap64(v) = ((v + 2⁶³) mod 2⁶⁴) − 2⁶³`,
/// using SMT-LIB's Euclidean `mod` (non-negative for a positive modulus), so
/// the result lands in `[−2⁶³, 2⁶³)` and equals `v` whenever `v` was already
/// in range. Wrapping each arithmetic result (rather than switching the sort to
/// `(_ BitVec 64)`) keeps division/quantifier reasoning tractable; see the
/// module docs.
fn wrap64(ctx: &Context, v: SExpr) -> SExpr {
    // 2⁶³ and 2⁶⁴ overflow i64, so build them from wider literals.
    let half = ctx.numeral(9_223_372_036_854_775_808_u64);
    let modulus = ctx.numeral(18_446_744_073_709_551_616_i128);
    let shifted = ctx.plus(v, half);
    let reduced = ctx.modulo(shifted, modulus);
    ctx.sub(reduced, half)
}

/// Truncate-toward-zero quotient over SMT integers. SMT-LIB `div` is Euclidean
/// (`0 <= mod < |y|`): it agrees with truncation when the division is exact or
/// the dividend is non-negative, and otherwise overshoots by exactly one step
/// *away from zero* — so correct by +1 for a positive divisor, −1 for a
/// negative one. `y = 0` is unspecified in both encodings (the runtime traps).
fn smt_tdiv(ctx: &Context, x: SExpr, y: SExpr) -> SExpr {
    let zero = ctx.numeral(0);
    let one = ctx.numeral(1);
    let q = ctx.div(x, y);
    let m = ctx.modulo(x, y);
    let x_nonneg = ctx.gte(x, zero);
    let exact = ctx.eq(m, zero);
    let agrees = ctx.or(x_nonneg, exact);
    let plus1 = ctx.plus(q, one);
    let minus1 = ctx.sub(q, one);
    let y_pos = ctx.gt(y, zero);
    let corrected = ctx.ite(y_pos, plus1, minus1);
    ctx.ite(agrees, q, corrected)
}

/// Encode contract/body integer arithmetic; `/` and `%` use the truncating
/// encoding shared with [`encode_prim`]. Every result is reduced through
/// [`wrap64`], so the term denotes the runtime's 64-bit wrapping value
/// (operands are already in range, so this is identity except on overflow).
fn smt_arith(ctx: &Context, op: ArithOp, x: SExpr, y: SExpr) -> SExpr {
    let raw = match op {
        ArithOp::Add => ctx.plus(x, y),
        ArithOp::Sub => ctx.sub(x, y),
        ArithOp::Mul => ctx.times(x, y),
        ArithOp::Div => smt_tdiv(ctx, x, y),
        ArithOp::Rem => {
            let q = smt_tdiv(ctx, x, y);
            let yq = ctx.times(y, q);
            ctx.sub(x, yq)
        }
    };
    wrap64(ctx, raw)
}

/// Build a guarded bounded quantifier: `forall q. lo <= q < hi ⇒ body` or
/// `exists q. lo <= q < hi ∧ body`.
fn quantify(
    ctx: &Context,
    exists: bool,
    name: &str,
    qv: SExpr,
    lo: SExpr,
    hi: SExpr,
    body: SExpr,
) -> SExpr {
    let ge = ctx.gte(qv, lo);
    let lt = ctx.lt(qv, hi);
    let bounds = ctx.and(ge, lt);
    let int = ctx.int_sort();
    if exists {
        let inner = ctx.and(bounds, body);
        ctx.exists(vec![(name.to_string(), int)], inner)
    } else {
        let inner = ctx.imp(bounds, body);
        ctx.forall(vec![(name.to_string(), int)], inner)
    }
}

/// Encode a comparison over two symbolic operands: `==`/`!=` for any pair of
/// same-shaped scalars, the orderings for integers.
fn encode_cmp(ctx: &Context, op: CmpOp, x: &Sym, y: &Sym) -> Result<SExpr, String> {
    let (Sym::Scalar(a, ka), Sym::Scalar(b, _kb)) = (x, y) else {
        return Err("contract comparison over a non-scalar value".to_string());
    };
    Ok(match op {
        CmpOp::Eq => ctx.eq(*a, *b),
        CmpOp::Ne => {
            let e = ctx.eq(*a, *b);
            ctx.not(e)
        }
        CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
            if matches!(ka, SortKind::Bool) {
                return Err("ordering comparison over booleans in a contract".to_string());
            }
            match op {
                CmpOp::Lt => ctx.lt(*a, *b),
                CmpOp::Le => ctx.lte(*a, *b),
                CmpOp::Gt => ctx.gt(*a, *b),
                CmpOp::Ge => ctx.gte(*a, *b),
                _ => unreachable!(),
            }
        }
    })
}

// ---- flat (requires/ensures) predicate encoding ---------------------------

/// Encode a flat-convention contract predicate to an SMT boolean. `params` are
/// the parameter values; `result` is the result value if in scope; `binders`
/// the SMT variables of the enclosing quantifiers, outermost first.
fn encode_flat_pred(
    ctx: &Context,
    p: &Pred,
    params: &[Sym],
    result: Option<&Sym>,
    binders: &mut Vec<SExpr>,
) -> Result<SExpr, String> {
    match p {
        Pred::True => Ok(ctx.true_()),
        Pred::False => Ok(ctx.false_()),
        Pred::Cmp(op, l, r) => {
            let x = encode_flat_cexpr(ctx, l, params, result, binders)?;
            let y = encode_flat_cexpr(ctx, r, params, result, binders)?;
            encode_cmp(ctx, *op, &x, &y)
        }
        Pred::And(l, r) => {
            let x = encode_flat_pred(ctx, l, params, result, binders)?;
            let y = encode_flat_pred(ctx, r, params, result, binders)?;
            Ok(ctx.and(x, y))
        }
        Pred::Or(l, r) => {
            let x = encode_flat_pred(ctx, l, params, result, binders)?;
            let y = encode_flat_pred(ctx, r, params, result, binders)?;
            Ok(ctx.or(x, y))
        }
        Pred::Not(inner) => {
            let x = encode_flat_pred(ctx, inner, params, result, binders)?;
            Ok(ctx.not(x))
        }
        Pred::Forall { domain, body } | Pred::Exists { domain, body } => {
            let exists = matches!(p, Pred::Exists { .. });
            let lo = encode_flat_cexpr(ctx, &domain.0, params, result, binders)?;
            let Sym::Scalar(lo, _) = lo else {
                return Err("quantifier bound is not an integer".to_string());
            };
            let hi = encode_flat_cexpr(ctx, &domain.1, params, result, binders)?;
            let Sym::Scalar(hi, _) = hi else {
                return Err("quantifier bound is not an integer".to_string());
            };
            let name = format!("q{}", binders.len());
            let qv = ctx.atom(name.as_str());
            binders.push(qv);
            let inner = encode_flat_pred(ctx, body, params, result, binders);
            binders.pop();
            Ok(quantify(ctx, exists, &name, qv, lo, hi, inner?))
        }
    }
}

fn encode_flat_cexpr(
    ctx: &Context,
    e: &CExpr,
    params: &[Sym],
    result: Option<&Sym>,
    binders: &[SExpr],
) -> Result<Sym, String> {
    match e {
        CExpr::Atom(a) => match a {
            Atom::Var(i) => {
                let i = *i as usize;
                if i < params.len() {
                    Ok(params[i].clone())
                } else if i == params.len() {
                    result
                        .cloned()
                        .ok_or_else(|| "`result` used where it is not in scope".to_string())
                } else {
                    binders
                        .get(i - params.len() - 1)
                        .map(|&qv| Sym::Scalar(qv, SortKind::Int))
                        .ok_or_else(|| "contract variable out of range".to_string())
                }
            }
            Atom::Lit(Literal::Int(n)) => Ok(Sym::Scalar(ctx.numeral(*n), SortKind::Int)),
            Atom::Lit(Literal::Bool(b)) => Ok(Sym::Scalar(
                if *b { ctx.true_() } else { ctx.false_() },
                SortKind::Bool,
            )),
            Atom::Lit(_) => Err("non-scalar literal in contract".to_string()),
            Atom::Global(_) => Err("global reference in contract".to_string()),
        },
        CExpr::Node(n) => match &**n {
            CNode::Bin(op, l, r) => {
                let x = encode_flat_cexpr(ctx, l, params, result, binders)?;
                let Sym::Scalar(x, _) = x else {
                    return Err("arithmetic over a non-integer contract value".to_string());
                };
                let y = encode_flat_cexpr(ctx, r, params, result, binders)?;
                let Sym::Scalar(y, _) = y else {
                    return Err("arithmetic over a non-integer contract value".to_string());
                };
                Ok(Sym::Scalar(smt_arith(ctx, *op, x, y), SortKind::Int))
            }
            CNode::Neg(inner) => {
                let x = encode_flat_cexpr(ctx, inner, params, result, binders)?;
                let Sym::Scalar(x, _) = x else {
                    return Err("negation of a non-integer contract value".to_string());
                };
                let zero = ctx.numeral(0);
                Ok(Sym::Scalar(wrap64(ctx, ctx.sub(zero, x)), SortKind::Int))
            }
            CNode::Len(inner) => match encode_flat_cexpr(ctx, inner, params, result, binders)? {
                Sym::Array { len, .. } => Ok(Sym::Scalar(len, SortKind::Int)),
                _ => Err("`len` of a non-array value in a contract".to_string()),
            },
            CNode::Index(base, index) => {
                let b = encode_flat_cexpr(ctx, base, params, result, binders)?;
                let i = encode_flat_cexpr(ctx, index, params, result, binders)?;
                let Sym::Scalar(i, _) = i else {
                    return Err("contract index is not an integer".to_string());
                };
                match b {
                    Sym::Array { arr, elem, .. } => Ok(Sym::Scalar(ctx.select(arr, i), elem)),
                    _ => Err("indexing a non-array value in a contract".to_string()),
                }
            }
            CNode::Proj(base, idx) => {
                let b = encode_flat_cexpr(ctx, base, params, result, binders)?;
                adt_field(&b, *idx)
            }
        },
    }
}

/// Project a struct field out of an unpacked ADT value (single-variant only —
/// enum fields are reachable only through `match`, never a contract).
fn adt_field(b: &Sym, idx: u32) -> Result<Sym, String> {
    match b {
        Sym::Adt { variants, .. } if variants.len() == 1 => variants[0]
            .as_ref()
            .and_then(|fs| fs.get(idx as usize).cloned())
            .ok_or_else(|| "field projection out of range in a contract".to_string()),
        _ => Err("field projection on a non-struct value in a contract".to_string()),
    }
}

// ---- type helpers -------------------------------------------------------

/// Which scalar SMT sort a type maps to (`None` ⇒ not a scalar).
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
