//! # marv-interp — tree-walking interpreter (milestone M4)
//!
//! A tree-walking evaluator over the canonical **Core IR** (`marv-core`). It is
//! the semantics *oracle*: the reference meaning of a marv program, used to
//! differentially test the Cranelift backend (`marv-codegen-cl`) and kept
//! permanently afterward as that oracle. Because Core is the single artifact
//! every backend consumes, "the interpreter and Cranelift agree" is a precise,
//! checkable statement (the M4 acceptance gate).
//!
//! ## What it executes
//!
//! A [`Program`] is a set of content-addressed definitions plus the declaration
//! [`World`] they resolve against — exactly what the front end (`parse → lower`)
//! or a Core-IR snapshot produces. Globals are resolved by **symbol hash**
//! (`marv_core::symbol_hash`), matching how lowering emits cross-definition
//! references (see `marv_core::lower`). Application is curried: a reference to an
//! n-ary function yields a [`Value::Partial`] that triggers the call once its
//! n-th argument arrives (`spec/02` §C).
//!
//! ## Capabilities are injected, never ambient (`spec/03` §4.5)
//!
//! [`Program::run`] takes the host's **grant set**. The entry point's
//! capability parameters are filled from that set and from nothing else; a
//! `perform` on an ungranted capability is impossible because the value never
//! exists. Every `perform` is recorded as an [`Effect`], so a run's authority
//! use is auditable. This is the sandbox model: the same property the static
//! effect row guarantees at compile time, enforced again at the entry boundary.
//!
//! Note that the *static* check (`marv-types`) already rejects a function that
//! performs a capability outside its declared row before it can ever run — so
//! the runtime grant check is a redundant, defense-in-depth backstop, not the
//! primary guarantee.

mod value;

use std::collections::{HashMap, HashSet};

use marv_core::ir::*;
use marv_core::symbol_hash;
use marv_types::World;

pub use value::{Effect, Value};

/// A loadable program: every definition keyed by the symbol hash its callers
/// use, plus the [`World`] that gives meaning to nominal/capability/error
/// hashes the bodies mention.
pub struct Program {
    /// `symbol_hash(qualified_name)` → its definition. Holds every kind of def;
    /// only `Fn`s are callable, but `Ctor`/`Proj`/`Match` consult the others
    /// indirectly through [`Program::world`].
    defs: HashMap<Hash, DefEntry>,
    /// Human name (bare and module-qualified) → symbol hash, for entry lookup.
    names: HashMap<String, Hash>,
    world: World,
}

/// One definition as the interpreter holds it.
struct DefEntry {
    #[allow(dead_code)]
    name: String,
    qualified: String,
    def: Def,
}

/// A failure that stops a run. Type/effect/capability errors are *not* here:
/// the M2 checker rejects those before a `Program` is ever run. These are the
/// residual dynamic conditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The named entry point does not exist (or no default could be chosen).
    NoSuchEntry(String),
    /// The entry's capability parameter was not in the host's grant set
    /// (`spec/03` §4.5). The static check makes this unreachable for well-typed
    /// programs; it remains as a defense-in-depth backstop at the boundary.
    UngrantedCapability(String),
    /// Too few command-line arguments for the entry's value parameters.
    MissingArgument { index: usize, ty: String },
    /// An argument string could not be parsed at the entry's expected type.
    BadArgument {
        index: usize,
        ty: String,
        got: String,
    },
    /// Integer division (or remainder) by zero.
    DivByZero,
    /// A `raise` reached the entry boundary uncaught (no surface `?`/`match`
    /// consumed it). Carries the error's display name.
    Uncaught(String),
    /// A construct the interpreter does not model (it should never arise from a
    /// checked program through the current front end). Carries a description.
    Unsupported(String),
    /// A referenced global is neither a known function nor a value.
    UnknownGlobal(Hash),
    /// A `requires` precondition was violated at runtime (Tier 1, `spec/01` §7).
    /// Carries the rendered clause.
    PreconditionFailed(String),
    /// An `ensures` postcondition was violated at runtime (Tier 1).
    PostconditionFailed(String),
    /// A loop `invariant` was violated at runtime (Tier 1, `spec/01` §7) — it did
    /// not hold when the loop condition was about to be tested. Carries the
    /// rendered clause with the offending concrete values substituted.
    InvariantViolated(String),
}

/// Which contract a Tier-1 failure came from, for the error variant/message.
#[derive(Clone, Copy)]
enum Contract {
    Pre,
    Post,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::NoSuchEntry(e) => write!(f, "no entry point `{e}`"),
            RunError::UngrantedCapability(c) => {
                write!(f, "entry requires capability `{c}`, which was not granted")
            }
            RunError::MissingArgument { index, ty } => {
                write!(f, "missing argument {index} of type `{ty}`")
            }
            RunError::BadArgument { index, ty, got } => {
                write!(f, "argument {index} `{got}` is not a valid `{ty}`")
            }
            RunError::DivByZero => write!(f, "division by zero"),
            RunError::Uncaught(e) => write!(f, "uncaught error `{e}`"),
            RunError::Unsupported(d) => write!(f, "unsupported construct: {d}"),
            RunError::UnknownGlobal(h) => write!(f, "unknown global {}", h.to_b3()),
            RunError::PreconditionFailed(p) => write!(f, "precondition violated: requires {p}"),
            RunError::PostconditionFailed(p) => write!(f, "postcondition violated: ensures {p}"),
            RunError::InvariantViolated(p) => write!(f, "loop invariant violated: {p}"),
        }
    }
}

impl std::error::Error for RunError {}

/// Check a list of contract predicates against the parameter values (and, for
/// postconditions, the result). A violation is a Tier-1 runtime failure
/// (`spec/01` §7). Atoms use the flat contract convention (`Var(k)` = parameter
/// k, `Var(n)` = `result`).
fn check_contracts(
    preds: &[Pred],
    params: &[Value],
    result: Option<&Value>,
    which: Contract,
) -> Result<(), RunError> {
    for p in preds {
        match eval_pred(p, params, result) {
            Some(true) => {}
            // A predicate the runtime can't evaluate is skipped, not failed —
            // it is left to Tier-2 / a future runtime extension.
            None => {}
            Some(false) => {
                let label = |i: u32| -> String {
                    let i = i as usize;
                    if i < params.len() {
                        format!("arg{i}")
                    } else {
                        "result".to_string()
                    }
                };
                let rendered = marv_core::render_pred(p, &label);
                return Err(match which {
                    Contract::Pre => RunError::PreconditionFailed(rendered),
                    Contract::Post => RunError::PostconditionFailed(rendered),
                });
            }
        }
    }
    Ok(())
}

/// Evaluate a contract predicate to a boolean, or `None` if it uses a feature
/// the runtime does not evaluate yet (bounded quantifiers).
fn eval_pred(p: &Pred, params: &[Value], result: Option<&Value>) -> Option<bool> {
    match p {
        Pred::True => Some(true),
        Pred::False => Some(false),
        Pred::Cmp(op, a, b) => {
            let x = pred_atom(a, params, result)?;
            let y = pred_atom(b, params, result)?;
            let ord = compare(&x, &y).ok()?;
            Some(cmp_matches(*op, ord))
        }
        Pred::And(l, r) => Some(eval_pred(l, params, result)? && eval_pred(r, params, result)?),
        Pred::Or(l, r) => Some(eval_pred(l, params, result)? || eval_pred(r, params, result)?),
        Pred::Not(inner) => Some(!eval_pred(inner, params, result)?),
        Pred::Forall { .. } | Pred::Exists { .. } => None,
    }
}

/// Whether a comparison `op` holds for an ordering result (`None` means the two
/// values are incomparable, so every comparison is false).
fn cmp_matches(op: CmpOp, ord: Option<std::cmp::Ordering>) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match op {
        CmpOp::Eq => ord == Some(Equal),
        CmpOp::Ne => ord != Some(Equal),
        CmpOp::Lt => ord == Some(Less),
        CmpOp::Le => matches!(ord, Some(Less | Equal)),
        CmpOp::Gt => ord == Some(Greater),
        CmpOp::Ge => matches!(ord, Some(Greater | Equal)),
    }
}

/// Resolve a contract atom to a runtime value.
fn pred_atom(a: &Atom, params: &[Value], result: Option<&Value>) -> Option<Value> {
    match a {
        Atom::Var(i) => {
            let i = *i as usize;
            if i < params.len() {
                Some(params[i].clone())
            } else if i == params.len() {
                result.cloned()
            } else {
                None
            }
        }
        Atom::Lit(l) => Some(lit_value(l)),
        Atom::Global(_) => None,
    }
}

/// The result of a completed run: the entry point's value and the ordered log
/// of capability effects it performed.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub value: Value,
    pub effects: Vec<Effect>,
}

impl Program {
    /// Assemble a program from definitions named in `module_path`'s scope.
    ///
    /// Each def is keyed under `symbol_hash("<module>.<name>")` — the exact hash
    /// a body's `Atom::Global` carries for an in-module reference (see
    /// `marv_core::lower`), so calls resolve. The `world` supplies the
    /// declarations (capabilities, enums, …) the bodies reference.
    pub fn new(module_path: &str, defs: Vec<(String, Def)>, world: World) -> Self {
        let mut def_map = HashMap::new();
        let mut names = HashMap::new();
        for (name, def) in defs {
            let qualified = qualify(module_path, &name);
            let h = symbol_hash(&qualified);
            names.insert(name.clone(), h);
            names.insert(qualified.clone(), h);
            def_map.insert(
                h,
                DefEntry {
                    name,
                    qualified,
                    def,
                },
            );
        }
        Program {
            defs: def_map,
            names,
            world,
        }
    }

    /// Resolve an entry point: an explicit `entry` name (bare or qualified), or
    /// — when `entry` is empty — `main`, else the sole function if there is
    /// exactly one. Returns the function's symbol hash.
    fn resolve_entry(&self, entry: &str) -> Result<Hash, RunError> {
        if !entry.is_empty() {
            return self
                .names
                .get(entry)
                .copied()
                .filter(|h| self.is_fn(h))
                .ok_or_else(|| RunError::NoSuchEntry(entry.to_string()));
        }
        if let Some(h) = self.names.get("main").copied().filter(|h| self.is_fn(h)) {
            return Ok(h);
        }
        let fns: Vec<Hash> = self
            .defs
            .iter()
            .filter(|(_, e)| e.def.kind == DefKind::Fn)
            .map(|(h, _)| *h)
            .collect();
        match fns.as_slice() {
            [h] => Ok(*h),
            _ => Err(RunError::NoSuchEntry("main".to_string())),
        }
    }

    fn is_fn(&self, h: &Hash) -> bool {
        self.defs
            .get(h)
            .map(|e| e.def.kind == DefKind::Fn)
            .unwrap_or(false)
    }

    /// The display names of every callable function, in deterministic order
    /// (for CLI listing and error messages).
    pub fn function_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .defs
            .values()
            .filter(|e| e.def.kind == DefKind::Fn)
            .map(|e| e.qualified.clone())
            .collect();
        v.sort();
        v
    }

    /// Run `entry` with the host-provided capability `grant` set and the
    /// command-line `args` (filling the entry's non-capability value
    /// parameters, in order). See `spec/03` §4.5.
    pub fn run(&self, entry: &str, grant: &[String], args: &[String]) -> Result<Outcome, RunError> {
        let h = self.resolve_entry(entry)?;
        let def = &self.defs[&h].def;
        let grant_set: HashSet<&str> = grant.iter().map(String::as_str).collect();

        // Build the initial environment by binding each declared parameter, in
        // de Bruijn level order (level 0 = outermost / first parameter).
        let param_tys = peel_param_types(&def.ty);
        let mut env: Vec<Value> = Vec::with_capacity(param_tys.len());
        let mut arg_cursor = 0usize;
        for (i, pty) in param_tys.iter().enumerate() {
            let v = self.bind_param(i, pty, &grant_set, args, &mut arg_cursor)?;
            env.push(v);
        }

        // Tier-1 contract checking (`spec/01` §7): in this debug runner, every
        // `requires` is checked before the body executes. Contract atoms use the
        // flat convention `Var(k)` = parameter k, so the parameter environment
        // (`env`, by level) is exactly the variable context.
        let params = env.clone();
        check_contracts(&def.requires, &params, None, Contract::Pre)?;

        // Evaluate the innermost body under the bound parameters.
        let body = peel_lams(
            def.body
                .as_ref()
                .ok_or_else(|| RunError::Unsupported(format!("entry `{entry}` has no body")))?,
        );
        let mut effects = Vec::new();
        let value = self.eval(body, &mut env, &mut effects)?;

        // …and every `ensures` after, with `result` bound to the returned value.
        check_contracts(&def.ensures, &params, Some(&value), Contract::Post)?;

        Ok(Outcome { value, effects })
    }

    /// Materialize one entry parameter: a capability is injected from the grant
    /// set, a unit needs nothing, and any other (value) parameter consumes the
    /// next command-line argument.
    fn bind_param(
        &self,
        index: usize,
        ty: &Type,
        grant: &HashSet<&str>,
        args: &[String],
        cursor: &mut usize,
    ) -> Result<Value, RunError> {
        if let Type::Nominal { def, .. } = ty {
            if self.world.is_cap(def) {
                let name = self.world.cap_name(def);
                if grant.contains(name.as_str()) {
                    return Ok(Value::Cap(name));
                }
                return Err(RunError::UngrantedCapability(name));
            }
        }
        match ty {
            Type::Unit => Ok(Value::Unit),
            _ => {
                let raw = args.get(*cursor).ok_or_else(|| RunError::MissingArgument {
                    index,
                    ty: show_type(ty),
                })?;
                *cursor += 1;
                parse_arg(index, ty, raw)
            }
        }
    }

    // ---- evaluation -----------------------------------------------------

    /// Evaluate a Core term under `env` (indexed by de Bruijn *level*), pushing
    /// and popping bindings around the binders it introduces so `env` is
    /// restored on return.
    fn eval(
        &self,
        c: &Core,
        env: &mut Vec<Value>,
        eff: &mut Vec<Effect>,
    ) -> Result<Value, RunError> {
        match c {
            Core::Atom(a) => self.eval_atom(a, env),

            Core::Let { value, body } => {
                let v = self.eval(value, env, eff)?;
                env.push(v);
                let r = self.eval(body, env, eff);
                env.pop();
                r
            }

            Core::Lam { .. } => {
                // The front end produces lambdas only as a definition's curried
                // spine, which `run`/`apply` peel before evaluation ever reaches
                // here. A first-class lambda value has no surface form yet.
                Err(RunError::Unsupported("first-class lambda".to_string()))
            }

            Core::App { func, arg } => {
                let f = self.eval_atom(func, env)?;
                let a = self.eval_atom(arg, env)?;
                self.apply(f, a, eff)
            }

            Core::Ctor { tag, fields, .. } => {
                let fields = fields
                    .iter()
                    .map(|a| self.eval_atom(a, env))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::Agg { tag: *tag, fields })
            }

            Core::Proj { base, idx } => {
                let base = self.eval_atom(base, env)?;
                match base {
                    Value::Agg { fields, .. } => fields
                        .into_iter()
                        .nth(*idx as usize)
                        .ok_or_else(|| RunError::Unsupported("projection out of range".into())),
                    other => Err(RunError::Unsupported(format!(
                        "projection of non-aggregate `{}`",
                        other.render()
                    ))),
                }
            }

            Core::Match {
                scrutinee,
                branches,
            } => self.eval_match(scrutinee, branches, env, eff),

            Core::Prim { op, args } => {
                let args = args
                    .iter()
                    .map(|a| self.eval_atom(a, env))
                    .collect::<Result<Vec<_>, _>>()?;
                eval_prim(*op, &args)
            }

            Core::Perform { cap, op, args } => {
                let capv = self.eval_atom(cap, env)?;
                let name = match capv {
                    Value::Cap(n) => n,
                    other => {
                        return Err(RunError::Unsupported(format!(
                            "perform on non-capability `{}`",
                            other.render()
                        )))
                    }
                };
                let args = args
                    .iter()
                    .map(|a| self.eval_atom(a, env))
                    .collect::<Result<Vec<_>, _>>()?;
                eff.push(Effect {
                    cap: name,
                    op: op.0,
                    args,
                });
                // A modeled host op returns unit; richer host behavior (real I/O)
                // is layered in as the capability surface grows.
                Ok(Value::Unit)
            }

            Core::Raise { error, .. } => Err(RunError::Uncaught(self.world.error_name(error))),

            Core::Loop {
                state,
                invariant,
                cond,
                body,
            } => {
                // Loop-carried state, threaded functionally (`spec/02` §C `Loop`).
                // The carried variables become the innermost `k` env slots, which
                // `invariant`/`cond`/`body` read; the body evaluates to their next
                // values (a tuple) which we write back; the loop evaluates to their
                // final values (a tuple) so the enclosing scope can project them.
                let k = state.len();
                // Evaluate every initial value against the *enclosing* environment
                // before pushing any — a later state atom must not resolve against
                // an already-pushed earlier carried slot.
                let mut inits = Vec::with_capacity(k);
                for a in state {
                    inits.push(self.eval_atom(a, env)?);
                }
                env.extend(inits);
                loop {
                    // Tier-1 invariant check (`spec/01` §7): the invariant must hold
                    // each time the condition is about to be tested — loop entry and
                    // every re-entry.
                    if let Some(inv) = invariant {
                        if let Some(false) = self.eval_loop_invariant(inv, env) {
                            let report = self.render_loop_invariant(inv, env);
                            return Err(RunError::InvariantViolated(report));
                        }
                    }
                    let c = self.eval(cond, env, eff)?;
                    match c.as_bool() {
                        Some(true) => {}
                        Some(false) => break,
                        None => {
                            return Err(RunError::Unsupported(
                                "loop condition is not a boolean".into(),
                            ))
                        }
                    }
                    let next = self.eval(body, env, eff)?;
                    let new_fields = match next {
                        Value::Agg { fields, .. } => fields,
                        Value::Unit if k == 0 => Vec::new(),
                        other => {
                            return Err(RunError::Unsupported(format!(
                                "loop body did not produce its carried state (got `{}`)",
                                other.render()
                            )))
                        }
                    };
                    let base = env.len() - k;
                    for (j, v) in new_fields.into_iter().take(k).enumerate() {
                        env[base + j] = v;
                    }
                }
                // Pop the carried slots and return their final values as a tuple.
                let base = env.len() - k;
                let final_state = env.split_off(base);
                Ok(Value::Agg {
                    tag: 0,
                    fields: final_state,
                })
            }
        }
    }

    /// Evaluate a loop invariant against the live environment, resolving its atoms
    /// as de Bruijn indices into `env` (unlike contract `Pred`s, whose atoms use
    /// the flat parameter convention). Returns `None` for a clause the runtime
    /// does not evaluate (bounded quantifiers) — treated as "not violated".
    fn eval_loop_invariant(&self, p: &Pred, env: &[Value]) -> Option<bool> {
        match p {
            Pred::True => Some(true),
            Pred::False => Some(false),
            Pred::Cmp(op, a, b) => {
                let x = self.eval_atom(a, env).ok()?;
                let y = self.eval_atom(b, env).ok()?;
                let ord = compare(&x, &y).ok()?;
                Some(cmp_matches(*op, ord))
            }
            Pred::And(l, r) => {
                Some(self.eval_loop_invariant(l, env)? && self.eval_loop_invariant(r, env)?)
            }
            Pred::Or(l, r) => {
                Some(self.eval_loop_invariant(l, env)? || self.eval_loop_invariant(r, env)?)
            }
            Pred::Not(inner) => Some(!self.eval_loop_invariant(inner, env)?),
            Pred::Forall { .. } | Pred::Exists { .. } => None,
        }
    }

    /// Render a violated loop invariant with its atoms' concrete runtime values
    /// substituted (e.g. `5 <= 3`), for a structured Tier-1 failure report.
    fn render_loop_invariant(&self, p: &Pred, env: &[Value]) -> String {
        let label = |idx: u32| -> String {
            self.eval_atom(&Atom::Var(idx), env)
                .map(|v| v.render())
                .unwrap_or_else(|_| format!("v{idx}"))
        };
        marv_core::render_pred(p, &label)
    }

    fn eval_match(
        &self,
        scrutinee: &Atom,
        branches: &[Branch],
        env: &mut Vec<Value>,
        eff: &mut Vec<Effect>,
    ) -> Result<Value, RunError> {
        let s = self.eval_atom(scrutinee, env)?;
        let (tag, fields): (u32, Vec<Value>) = match s {
            // `bool` desugars to a two-variant match: false = tag 0, true = 1
            // (`spec/02` §D), with no bound fields.
            Value::Bool(b) => (b as u32, Vec::new()),
            Value::Agg { tag, fields } => (tag, fields),
            other => {
                return Err(RunError::Unsupported(format!(
                    "match on non-matchable `{}`",
                    other.render()
                )))
            }
        };
        let branch = branches
            .get(tag as usize)
            .ok_or_else(|| RunError::Unsupported("non-exhaustive match at runtime".into()))?;

        // Bind the variant's fields (the branch's `binds` arity) at fresh levels.
        let pushed = branch.binds as usize;
        for k in 0..pushed {
            env.push(fields.get(k).cloned().unwrap_or(Value::Unit));
        }
        let r = self.eval(&branch.body, env, eff);
        for _ in 0..pushed {
            env.pop();
        }
        r
    }

    fn eval_atom(&self, a: &Atom, env: &[Value]) -> Result<Value, RunError> {
        match a {
            Atom::Lit(l) => Ok(lit_value(l)),
            Atom::Var(idx) => {
                let d = env.len();
                let i = (*idx as usize) + 1;
                if i > d {
                    return Err(RunError::Unsupported(format!(
                        "de Bruijn index {idx} out of scope at depth {d}"
                    )));
                }
                Ok(env[d - i].clone())
            }
            Atom::Global(h) => {
                let entry = self.defs.get(h).ok_or(RunError::UnknownGlobal(*h))?;
                match entry.def.kind {
                    // A function reference is a not-yet-applied call.
                    DefKind::Fn => Ok(Value::Partial {
                        func: *h,
                        got: Vec::new(),
                    }),
                    // A const (or other value def) evaluates its body with no
                    // parameters in scope.
                    _ => {
                        let body = entry.def.body.as_ref().ok_or(RunError::UnknownGlobal(*h))?;
                        let mut e = Vec::new();
                        let mut sink = Vec::new();
                        self.eval(body, &mut e, &mut sink)
                    }
                }
            }
        }
    }

    /// Apply a (possibly partially-applied) function value to one argument,
    /// firing the call when the last curried parameter arrives.
    fn apply(&self, f: Value, arg: Value, eff: &mut Vec<Effect>) -> Result<Value, RunError> {
        let (func, mut got) = match f {
            Value::Partial { func, got } => (func, got),
            other => {
                return Err(RunError::Unsupported(format!(
                    "application of non-function `{}`",
                    other.render()
                )))
            }
        };
        got.push(arg);
        let entry = self.defs.get(&func).ok_or(RunError::UnknownGlobal(func))?;
        let body = entry
            .def
            .body
            .as_ref()
            .ok_or(RunError::UnknownGlobal(func))?;
        let arity = lam_arity(body);
        if got.len() < arity {
            return Ok(Value::Partial { func, got });
        }
        // Saturated: evaluate the innermost body with the arguments bound at
        // levels 0..arity.
        let inner = peel_lams(body);
        let mut env = got;
        self.eval(inner, &mut env, eff)
    }
}

// ============================ free helpers ===============================

/// Module-qualify a definition name (mirrors `marv_db::qualify`, kept here so
/// the interpreter does not depend on the query database).
fn qualify(module_path: &str, name: &str) -> String {
    if module_path.is_empty() {
        name.to_string()
    } else {
        format!("{module_path}.{name}")
    }
}

/// The number of curried lambdas wrapping a definition body (its arity).
fn lam_arity(mut body: &Core) -> usize {
    let mut n = 0;
    while let Core::Lam { body: inner, .. } = body {
        n += 1;
        body = inner;
    }
    n
}

/// Strip the curried lambda spine, returning the innermost (non-lambda) body.
fn peel_lams(mut body: &Core) -> &Core {
    while let Core::Lam { body: inner, .. } = body {
        body = inner;
    }
    body
}

/// The parameter types of a curried arrow, outermost first.
fn peel_param_types(mut ty: &Type) -> Vec<Type> {
    let mut params = Vec::new();
    while let Type::Arrow { param, ret, .. } = ty {
        params.push((**param).clone());
        ty = ret;
    }
    params
}

fn lit_value(l: &Literal) -> Value {
    match l {
        Literal::Unit => Value::Unit,
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Int(n) => Value::Int(*n),
        Literal::Float(bits) => Value::Float(f64::from_bits(*bits)),
        Literal::Str(s) => Value::Str(s.clone()),
        Literal::Char(c) => Value::Str(c.to_string()),
    }
}

/// Evaluate a total primitive over already-evaluated atomic operands
/// (`spec/02` §C `Prim`). The M2 checker has already type-checked the operands,
/// so the only residual dynamic failure is division by zero.
fn eval_prim(op: PrimOp, args: &[Value]) -> Result<Value, RunError> {
    use PrimOp::*;
    let int = |v: &Value| match v {
        Value::Int(n) => Some(*n),
        _ => None,
    };
    let float = |v: &Value| match v {
        Value::Float(x) => Some(*x),
        _ => None,
    };
    let a = args.first();
    let b = args.get(1);
    match op {
        Add | Sub | Mul | Div | Rem => {
            let (a, b) = (a.unwrap(), b.unwrap());
            if let (Some(x), Some(y)) = (int(a), int(b)) {
                let r = match op {
                    Add => x.wrapping_add(y),
                    Sub => x.wrapping_sub(y),
                    Mul => x.wrapping_mul(y),
                    Div => {
                        if y == 0 {
                            return Err(RunError::DivByZero);
                        }
                        x.wrapping_div(y)
                    }
                    Rem => {
                        if y == 0 {
                            return Err(RunError::DivByZero);
                        }
                        x.wrapping_rem(y)
                    }
                    _ => unreachable!(),
                };
                return Ok(Value::Int(r));
            }
            if let (Some(x), Some(y)) = (float(a), float(b)) {
                let r = match op {
                    Add => x + y,
                    Sub => x - y,
                    Mul => x * y,
                    Div => x / y,
                    Rem => x % y,
                    _ => unreachable!(),
                };
                return Ok(Value::Float(r));
            }
            Err(RunError::Unsupported(format!("arithmetic on {a:?}, {b:?}")))
        }
        Eq | Ne | Lt | Le | Gt | Ge => {
            let (a, b) = (a.unwrap(), b.unwrap());
            let ord = compare(a, b)?;
            let r = match op {
                Eq => ord == Some(std::cmp::Ordering::Equal),
                Ne => ord != Some(std::cmp::Ordering::Equal),
                Lt => ord == Some(std::cmp::Ordering::Less),
                Le => matches!(
                    ord,
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                ),
                Gt => ord == Some(std::cmp::Ordering::Greater),
                Ge => matches!(
                    ord,
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                ),
                _ => unreachable!(),
            };
            Ok(Value::Bool(r))
        }
        And => Ok(Value::Bool(bool_of(a)? && bool_of(b)?)),
        Or => Ok(Value::Bool(bool_of(a)? || bool_of(b)?)),
        Not => Ok(Value::Bool(!bool_of(a)?)),
        Len => match a {
            Some(Value::Agg { fields, .. }) => Ok(Value::Int(fields.len() as i64)),
            Some(Value::Str(s)) => Ok(Value::Int(s.len() as i64)),
            _ => Err(RunError::Unsupported("len of non-collection".into())),
        },
        Index => match (a, b) {
            (Some(Value::Agg { fields, .. }), Some(Value::Int(i))) => fields
                .get(*i as usize)
                .cloned()
                .ok_or_else(|| RunError::Unsupported("index out of range".into())),
            _ => Err(RunError::Unsupported("index of non-collection".into())),
        },
    }
}

/// Compare two scalars for the ordering/equality primitives. `None` ordering
/// means the partial comparison was undefined (e.g. NaN); a genuine type
/// mismatch is a checker-prevented error.
fn compare(a: &Value, b: &Value) -> Result<Option<std::cmp::Ordering>, RunError> {
    use Value::*;
    match (a, b) {
        (Int(x), Int(y)) => Ok(x.partial_cmp(y)),
        (Float(x), Float(y)) => Ok(x.partial_cmp(y)),
        (Bool(x), Bool(y)) => Ok(x.partial_cmp(y)),
        (Str(x), Str(y)) => Ok(x.partial_cmp(y)),
        (Unit, Unit) => Ok(Some(std::cmp::Ordering::Equal)),
        _ => Err(RunError::Unsupported(format!(
            "comparison of incompatible values `{}` and `{}`",
            a.render(),
            b.render()
        ))),
    }
}

fn bool_of(v: Option<&Value>) -> Result<bool, RunError> {
    v.and_then(Value::as_bool)
        .ok_or_else(|| RunError::Unsupported("expected a boolean operand".into()))
}

fn parse_arg(index: usize, ty: &Type, raw: &str) -> Result<Value, RunError> {
    let bad = || RunError::BadArgument {
        index,
        ty: show_type(ty),
        got: raw.to_string(),
    };
    match ty {
        Type::Int(_) => raw.parse::<i64>().map(Value::Int).map_err(|_| bad()),
        Type::Float(_) => raw.parse::<f64>().map(Value::Float).map_err(|_| bad()),
        Type::Bool => raw.parse::<bool>().map(Value::Bool).map_err(|_| bad()),
        Type::Str => Ok(Value::Str(raw.to_string())),
        _ => Err(RunError::Unsupported(format!(
            "cannot pass a command-line argument of type `{}`",
            show_type(ty)
        ))),
    }
}

/// A compact display of a Core type for error messages.
fn show_type(t: &Type) -> String {
    match t {
        Type::Unit => "()".into(),
        Type::Bool => "bool".into(),
        Type::Int(i) => format!("{i:?}").to_lowercase(),
        Type::Float(FloatTy::F32) => "f32".into(),
        Type::Float(FloatTy::F64) => "f64".into(),
        Type::Str => "str".into(),
        Type::Char => "char".into(),
        Type::Array(e, n) => format!("[{n}]{}", show_type(e)),
        Type::Slice(e) => format!("[]{}", show_type(e)),
        Type::Tuple(es) => {
            let inner: Vec<String> = es.iter().map(show_type).collect();
            format!("({})", inner.join(", "))
        }
        Type::Arrow { param, ret, .. } => format!("fn({}) -> {}", show_type(param), show_type(ret)),
        Type::Nominal { def, .. } => format!("nominal#{}", &def.to_hex()[..8]),
        Type::Ref { mutable: true, of } => format!("&mut {}", show_type(of)),
        Type::Ref { mutable: false, of } => format!("&{}", show_type(of)),
        Type::Linear(inner) => format!("linear {}", show_type(inner)),
        Type::Var(i) => format!("T{i}"),
    }
}
