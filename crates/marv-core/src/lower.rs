//! Lowering from the marv-syntax AST to the canonical Core IR
//! (`spec/02-grammar-and-core-ir.md` §§C–D).
//!
//! Three transformations happen here, in one pass plus a finalizer:
//!
//! 1. **Desugaring** (`spec/02` §D). `if`/`else` becomes a [`Core::Match`] on a
//!    `bool` scrutinee; method calls `a.m(x)` become curried free-function
//!    application `App(App(m, a), x)`; multi-argument calls are curried. (The
//!    other §D rules — `?`, `while`, `for`, optional/error sugar — concern
//!    surface forms the M0 parser does not yet produce; they slot in here as the
//!    grammar grows, with no change to the machinery below.)
//!
//! 2. **ANF normalization** (`spec/02` §C). Every non-atomic subexpression is
//!    hoisted into a `let`, left-to-right, so operands are always atomic and
//!    evaluation order is explicit. Bindings are collected as a flat,
//!    in-evaluation-order list and folded into a right-nested `Let` *spine* at
//!    the end of each block.
//!
//! 3. **de Bruijn conversion** (`spec/02` §C). To avoid the index-shifting that
//!    plagues naive ANF construction, lowering first records each variable as a
//!    de Bruijn *level* (counted from the outermost binder, hence stable as
//!    inner binders appear). A final [`to_indices`] pass rewrites every level
//!    `L` used at binder-depth `D` into the index `D − 1 − L`. Names never reach
//!    the Core, so alpha-equivalent surface programs lower to identical terms.
//!
//! Cross-definition references are resolved to [`Atom::Global`] /
//! [`Type::Nominal`] via [`symbol_hash`] (see its docs for why M1 keys on the
//! resolved name rather than the target's own content hash).

use std::collections::{HashMap, HashSet};

use marv_syntax::{
    ArmBody, BinOp, Block, Else, EnumDecl, Expr, Field, FieldPat, FnDecl, IfExpr, Item, MatchExpr,
    Module, Pattern, Stmt, StructDecl, Tail, Type as SType,
};

use crate::hash::symbol_hash;
use crate::ir::*;

/// A synthetic `()` argument for an empty, non-method call `f()` — a nullary
/// call is curried application to unit, matching the synthesized unit parameter
/// of a zero-parameter function.
static UNIT_ARG: Expr = Expr::Unit;

/// An error that prevented lowering. M1's only failure modes are field
/// projections whose base type cannot be resolved without the type checker
/// (which arrives in M2 and subsumes these). Everything else lowers totally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerError {
    /// A projection `base.field` whose base type M1 could not resolve statically
    /// (no annotation in scope, or the base is not a known in-module struct).
    UnresolvedProjection { field: String },
    /// A projection of a field the resolved struct does not declare.
    UnknownField { ty: String, field: String },
    /// A contract clause that is not a boolean predicate the `Pred` language can
    /// express (it must be a comparison, `and`/`or`, or a boolean literal).
    ContractNotPredicate,
    /// A contract comparison whose operand is not atomic. `Pred::Cmp` compares
    /// atoms (a variable, `result`, or a literal), so `result >= lo + 1` and the
    /// like cannot be expressed yet.
    ContractOperandNotAtomic,
    /// A contract referenced a name that is neither a parameter nor `result`.
    UnknownContractVar { name: String },
    /// `result` was used in a `requires` clause (it only exists post-return).
    ResultInRequires,
    /// A `match` whose arms name no enum constructor, so M1 cannot determine the
    /// scrutinee's variant set (an all-`_` match, or a match over a non-enum).
    MatchWithoutConstructor,
    /// A `match` whose arms mix constructors of different enums.
    MixedEnumPatterns { expected: String, found: String },
    /// A constructor pattern naming a variant no in-scope enum declares.
    UnknownConstructor { name: String },
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::UnresolvedProjection { field } => write!(
                f,
                "cannot resolve the type of the base of projection `.{field}` (M1 has no type \
                 inference; annotate the binding or wait for the M2 checker)"
            ),
            LowerError::UnknownField { ty, field } => {
                write!(f, "struct `{ty}` has no field `{field}`")
            }
            LowerError::ContractNotPredicate => write!(
                f,
                "a contract clause must be a boolean predicate (a comparison, `and`/`or`, or a \
                 boolean literal)"
            ),
            LowerError::ContractOperandNotAtomic => write!(
                f,
                "a contract comparison operand must be atomic (a parameter, `result`, or a literal)"
            ),
            LowerError::UnknownContractVar { name } => {
                write!(
                    f,
                    "contract refers to `{name}`, which is not a parameter or `result`"
                )
            }
            LowerError::ResultInRequires => {
                write!(
                    f,
                    "`result` may only appear in an `ensures` clause, not `requires`"
                )
            }
            LowerError::MatchWithoutConstructor => write!(
                f,
                "a `match` must name at least one enum constructor so its variant set is known \
                 (M1 lowers tag-indexed matches over enums)"
            ),
            LowerError::MixedEnumPatterns { expected, found } => write!(
                f,
                "`match` arms mix constructors of different enums (`{expected}` and `{found}`)"
            ),
            LowerError::UnknownConstructor { name } => {
                write!(f, "no enum in scope declares a constructor `{name}`")
            }
        }
    }
}

impl std::error::Error for LowerError {}

/// One variant of a lowered enum: its source name (display only — *not* part of
/// the content hash, `spec/02` §F) and its Core field types in tag order. This
/// is the non-hashed metadata the checker needs to resolve a `match` from source
/// (variant names + arities), which the names-erased [`Def`] cannot carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantInfo {
    pub name: String,
    pub fields: Vec<Type>,
}

/// One lowered top-level definition, paired with its content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefEntry {
    /// The definition's *source* name. Not part of the hash — renaming a `Def`
    /// does not change its identity (`spec/02` §F).
    pub name: String,
    pub def: Def,
    pub hash: Hash,
    /// For [`DefKind::Enum`] defs, the variant names + field types in tag order
    /// (see [`VariantInfo`]); `None` for every other kind.
    pub enum_variants: Option<Vec<VariantInfo>>,
}

/// A whole module lowered to Core: its definitions in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredModule {
    pub module: Vec<String>,
    pub defs: Vec<DefEntry>,
}

/// Lower a parsed module to Core, hashing each definition.
///
/// Enum *constructor* and `match` resolution sees only the enums declared in this
/// module. To lower a module that constructs or matches an enum imported from
/// another file (e.g. `std/result.mv` using `Option`), lower them together with
/// [`lower_modules`], which shares one constructor registry across the set.
pub fn lower_module(m: &Module) -> Result<LoweredModule, LowerError> {
    let reg = EnumReg::from_modules(std::slice::from_ref(m));
    lower_with_registry(m, &reg)
}

/// Lower several parsed modules that share a constructor namespace (a prelude
/// plus its dependents). A single [`EnumReg`] is built from *all* of them first,
/// so a `match` or constructor in one module resolves variants declared in
/// another. Each module is lowered independently and returned in input order.
pub fn lower_modules(ms: &[Module]) -> Result<Vec<LoweredModule>, LowerError> {
    let reg = EnumReg::from_modules(ms);
    ms.iter().map(|m| lower_with_registry(m, &reg)).collect()
}

fn lower_with_registry(m: &Module, reg: &EnumReg) -> Result<LoweredModule, LowerError> {
    let lw = Lowerer::new(m, reg.clone());
    let mut defs = Vec::with_capacity(m.items.len());
    for item in &m.items {
        let (name, def, enum_variants) = match item {
            Item::Fn(f) => (f.name.clone(), lw.lower_fn(f)?, None),
            Item::Struct(s) => (s.name.clone(), lw.lower_struct(s), None),
            Item::Enum(e) => {
                let (def, variants) = lw.lower_enum(e);
                (e.name.clone(), def, Some(variants))
            }
        };
        let hash = def.content_hash();
        defs.push(DefEntry {
            name,
            def,
            hash,
            enum_variants,
        });
    }
    Ok(LoweredModule {
        module: m.name.clone(),
        defs,
    })
}

/// A constructor reference resolved against the [`EnumReg`]: which enum it
/// belongs to (fully module-qualified), its tag (= declaration order), and its
/// arity (payload count).
#[derive(Debug, Clone)]
struct CtorRef {
    enum_qual: String,
    tag: u32,
    arity: usize,
}

/// The constructor/enum registry, built once from a set of modules so both
/// in-module and cross-module (imported) enum constructors resolve to a stable
/// nominal hash + tag. Keyed by every spelling a constructor may appear under:
/// the short `Enum.Variant`, the fully-qualified `mod.path.Enum.Variant`, and the
/// bare `Variant` (the bare form only when unambiguous across all enums).
#[derive(Debug, Clone, Default)]
struct EnumReg {
    ctors: HashMap<String, CtorRef>,
    /// Bare variant names that name more than one enum — removed from `ctors`, so
    /// they must be written qualified.
    ambiguous_bare: HashSet<String>,
    /// Enum short name *and* fully-qualified name → fully-qualified name, for
    /// resolving an enum *type* reference to the same nominal hash a constructor
    /// commits to.
    enum_qual: HashMap<String, String>,
    /// Fully-qualified enum name → its variant count (for exhaustive lowering).
    variant_count: HashMap<String, usize>,
}

impl EnumReg {
    fn from_modules(ms: &[Module]) -> Self {
        let mut reg = EnumReg::default();
        for m in ms {
            let mp = m.name.join(".");
            for item in &m.items {
                let Item::Enum(e) = item else { continue };
                let qual = if mp.is_empty() {
                    e.name.clone()
                } else {
                    format!("{mp}.{}", e.name)
                };
                reg.enum_qual.insert(e.name.clone(), qual.clone());
                reg.enum_qual.insert(qual.clone(), qual.clone());
                reg.variant_count.insert(qual.clone(), e.variants.len());
                for (tag, v) in e.variants.iter().enumerate() {
                    let cref = CtorRef {
                        enum_qual: qual.clone(),
                        tag: tag as u32,
                        arity: v.fields.len(),
                    };
                    reg.ctors
                        .insert(format!("{}.{}", e.name, v.name), cref.clone());
                    reg.ctors.insert(format!("{qual}.{}", v.name), cref.clone());
                    // The bare form is registered only while it stays unambiguous.
                    if reg.ambiguous_bare.contains(&v.name) {
                        // already known-ambiguous
                    } else if reg.ctors.contains_key(&v.name) {
                        reg.ambiguous_bare.insert(v.name.clone());
                        reg.ctors.remove(&v.name);
                    } else {
                        reg.ctors.insert(v.name.clone(), cref);
                    }
                }
            }
        }
        reg
    }

    /// Resolve a constructor spelling (bare `Variant`, short `Enum.Variant`, or
    /// fully-qualified) to its reference.
    fn ctor(&self, key: &str) -> Option<&CtorRef> {
        self.ctors.get(key)
    }

    /// The fully-qualified name an enum type reference denotes, if known.
    fn enum_qualified(&self, name: &str) -> Option<&String> {
        self.enum_qual.get(name)
    }
}

/// A local binding in scope during lowering: the atom it resolves to (with its
/// `Var` carrying a de Bruijn *level*) and its best-known surface type, used to
/// resolve field-projection indices.
#[derive(Debug, Clone)]
struct Binding {
    name: String,
    atom: Atom,
    ty: Option<SType>,
}

/// Collects the straight-line `let`-value computations of a single block, in
/// evaluation order, to be folded into a `Let` spine. `base_depth` is the binder
/// depth at which this block's first binding sits, so a freshly pushed binding's
/// level is `base_depth + lets.len()`.
struct Builder {
    base_depth: u32,
    lets: Vec<Core>,
}

impl Builder {
    fn new(base_depth: u32) -> Self {
        Builder {
            base_depth,
            lets: Vec::new(),
        }
    }

    /// The binder depth at the current point (params + bindings emitted so far).
    fn depth(&self) -> u32 {
        self.base_depth + self.lets.len() as u32
    }

    /// Bind a non-`Let` computation, returning the atom (a `Var` at the new
    /// binding's level) that names its result.
    fn push(&mut self, rhs: Core) -> Atom {
        let level = self.depth();
        self.lets.push(rhs);
        Atom::Var(level)
    }
}

/// Module-level context shared across every definition's lowering.
struct Lowerer {
    /// Dotted module path, e.g. `["geometry"]`.
    module_path: String,
    /// Names declared at module scope (structs + fns), used to module-qualify
    /// references so a free name resolves to a stable, module-scoped symbol.
    local_items: HashSet<String>,
    /// Struct declarations by source name, for projection-index resolution.
    structs: HashMap<String, Vec<Field>>,
    /// In-module function return types, for best-effort projection typing.
    fn_rets: HashMap<String, Option<SType>>,
    /// Constructor / enum registry (may span several modules; see [`EnumReg`]).
    reg: EnumReg,
}

impl Lowerer {
    fn new(m: &Module, reg: EnumReg) -> Self {
        let mut local_items = HashSet::new();
        let mut structs = HashMap::new();
        let mut fn_rets = HashMap::new();
        for item in &m.items {
            match item {
                Item::Struct(s) => {
                    local_items.insert(s.name.clone());
                    structs.insert(s.name.clone(), s.fields.clone());
                }
                Item::Fn(f) => {
                    local_items.insert(f.name.clone());
                    fn_rets.insert(f.name.clone(), f.ret.clone());
                }
                Item::Enum(_) => {}
            }
        }
        Lowerer {
            module_path: m.name.join("."),
            local_items,
            structs,
            fn_rets,
            reg,
        }
    }

    /// Module-qualify a value reference if it names an in-module item; otherwise
    /// leave it bare (an import or builtin).
    fn qualify_value(&self, name: &str) -> String {
        if self.local_items.contains(name) {
            format!("{}.{}", self.module_path, name)
        } else {
            name.to_string()
        }
    }

    // ---- definitions ----------------------------------------------------

    fn lower_struct(&self, s: &StructDecl) -> Def {
        // Field *names* are not part of identity (`spec/02` §F): a struct's
        // content is its ordered field types. `linear` is captured by wrapping.
        // Structs carry no generics in the surface AST, so the generic scope is
        // empty.
        let field_tys: Vec<Type> = s
            .fields
            .iter()
            .map(|f| self.lower_type(&f.ty, &[]))
            .collect();
        let prod = Type::Tuple(field_tys);
        let ty = if s.linear {
            Type::Linear(Box::new(prod))
        } else {
            prod
        };
        Def {
            kind: DefKind::Struct,
            ty,
            requires: Vec::new(),
            ensures: Vec::new(),
            body: None,
        }
    }

    /// Lower an `enum` declaration to a [`DefKind::Enum`] [`Def`] plus the variant
    /// metadata the checker needs. The hashed identity (`Def.ty`) is the ordered
    /// tuple of per-variant field-type tuples — names erased (`spec/02` §F) — so
    /// alpha-equivalent enums hash identically; the variant *names* travel
    /// alongside as non-hashed [`VariantInfo`].
    fn lower_enum(&self, e: &EnumDecl) -> (Def, Vec<VariantInfo>) {
        let mut variant_tys: Vec<Type> = Vec::with_capacity(e.variants.len());
        let mut info: Vec<VariantInfo> = Vec::with_capacity(e.variants.len());
        for v in &e.variants {
            let fields: Vec<Type> = v
                .fields
                .iter()
                .map(|t| self.lower_type(t, &e.generics))
                .collect();
            variant_tys.push(Type::Tuple(fields.clone()));
            info.push(VariantInfo {
                name: v.name.clone(),
                fields,
            });
        }
        let def = Def {
            kind: DefKind::Enum,
            ty: Type::Tuple(variant_tys),
            requires: Vec::new(),
            ensures: Vec::new(),
            body: None,
        };
        (def, info)
    }

    fn lower_fn(&self, f: &FnDecl) -> Result<Def, LowerError> {
        // A nullary function is curried application over a single `()` param, so
        // synthesize one (unnamed, hence never referenced) when there are none.
        let synth_unit = f.params.is_empty();
        let param_ctys: Vec<Type> = if synth_unit {
            vec![Type::Unit]
        } else {
            f.params
                .iter()
                .map(|p| self.lower_type(&p.ty, &f.generics))
                .collect()
        };

        // Params occupy de Bruijn levels 0..n; the body lowers at depth n.
        let mut env: Vec<Binding> = Vec::new();
        if !synth_unit {
            for (i, p) in f.params.iter().enumerate() {
                env.push(Binding {
                    name: p.name.clone(),
                    atom: Atom::Var(i as u32),
                    ty: Some(p.ty.clone()),
                });
            }
        }
        let n = param_ctys.len() as u32;
        let body_core = self.lower_block(&f.body, &env, n)?;

        let ret_ty = f
            .ret
            .as_ref()
            .map(|t| self.lower_type(t, &f.generics))
            .unwrap_or(Type::Unit);
        // M1 records declared purity only: `pure fn` ⇒ empty row; for a plain
        // `fn` the row is left empty as a placeholder — effect/error inference is
        // M2. The innermost arrow/lambda carries the row; partial-application
        // arrows are pure.
        let effects = EffectRow::empty();

        let last = param_ctys.len() - 1;
        let mut lam = body_core;
        let mut arrow = ret_ty;
        for (i, pty) in param_ctys.iter().enumerate().rev() {
            let eff = if i == last {
                effects.clone()
            } else {
                EffectRow::empty()
            };
            lam = Core::Lam {
                param: pty.clone(),
                effects: eff.clone(),
                body: Box::new(lam),
            };
            arrow = Type::Arrow {
                param: Box::new(pty.clone()),
                ret: Box::new(arrow),
                effects: eff,
            };
        }

        // Finalize: rewrite de Bruijn levels to indices over the whole term.
        let body = to_indices(&lam, 0);

        // Lower the contract clauses (`spec/01` §7). Contract atoms use a *flat*
        // convention independent of the body's de Bruijn spine: `Var(k)` is the
        // k-th parameter (0-based), and in `ensures` `Var(n)` (n = parameter
        // count) is `result`. This is the same convention the Tier-1 runtime
        // checker and the Tier-2 SMT verifier consume.
        let names: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
        let requires = f
            .requires
            .iter()
            .map(|e| self.lower_pred(e, &names, false))
            .collect::<Result<Vec<_>, _>>()?;
        let ensures = f
            .ensures
            .iter()
            .map(|e| self.lower_pred(e, &names, true))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Def {
            kind: DefKind::Fn,
            ty: arrow,
            requires,
            ensures,
            body: Some(body),
        })
    }

    // ---- contracts ------------------------------------------------------

    /// Lower a surface boolean expression into a contract [`Pred`]. `params` are
    /// the parameter names (their position is the flat contract index);
    /// `allow_result` permits `result` (index `params.len()`), as in `ensures`.
    fn lower_pred(
        &self,
        e: &Expr,
        params: &[&str],
        allow_result: bool,
    ) -> Result<Pred, LowerError> {
        match e {
            Expr::Bool(true) => Ok(Pred::True),
            Expr::Bool(false) => Ok(Pred::False),
            Expr::Binary(l, op, r) => match op {
                BinOp::And => Ok(Pred::And(
                    Box::new(self.lower_pred(l, params, allow_result)?),
                    Box::new(self.lower_pred(r, params, allow_result)?),
                )),
                BinOp::Or => Ok(Pred::Or(
                    Box::new(self.lower_pred(l, params, allow_result)?),
                    Box::new(self.lower_pred(r, params, allow_result)?),
                )),
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let cmp = cmp_op(*op).ok_or(LowerError::ContractNotPredicate)?;
                    let la = self.lower_pred_atom(l, params, allow_result)?;
                    let ra = self.lower_pred_atom(r, params, allow_result)?;
                    Ok(Pred::Cmp(cmp, la, ra))
                }
                // Arithmetic operators are not boolean predicates.
                _ => Err(LowerError::ContractNotPredicate),
            },
            _ => Err(LowerError::ContractNotPredicate),
        }
    }

    /// Lower a contract comparison operand to an [`Atom`] (a parameter, `result`,
    /// or a literal). Compound operands are rejected — `Pred::Cmp` is atomic.
    fn lower_pred_atom(
        &self,
        e: &Expr,
        params: &[&str],
        allow_result: bool,
    ) -> Result<Atom, LowerError> {
        match e {
            Expr::Int(n) => Ok(Atom::Lit(Literal::Int(*n))),
            Expr::Bool(b) => Ok(Atom::Lit(Literal::Bool(*b))),
            Expr::Var(name) if name == "result" => {
                if allow_result {
                    Ok(Atom::Var(params.len() as u32))
                } else {
                    Err(LowerError::ResultInRequires)
                }
            }
            Expr::Var(name) => params
                .iter()
                .position(|p| p == name)
                .map(|i| Atom::Var(i as u32))
                .ok_or_else(|| LowerError::UnknownContractVar { name: name.clone() }),
            _ => Err(LowerError::ContractOperandNotAtomic),
        }
    }

    // ---- blocks & tails -------------------------------------------------

    fn lower_block(
        &self,
        block: &Block,
        env_in: &[Binding],
        base_depth: u32,
    ) -> Result<Core, LowerError> {
        let mut env = env_in.to_vec();
        let mut b = Builder::new(base_depth);

        for stmt in &block.stmts {
            let (name, ann, value) = match stmt {
                Stmt::Let { name, ty, value } => (name, ty, value),
                Stmt::Var { name, ty, value } => (name, ty, value),
            };
            // Best-effort surface type for the bound name (annotation first,
            // else inferred from the value where M1 can).
            let vty = ann.clone().or_else(|| self.type_of_expr(value, &env));
            let atom = self.emit_atom(value, &env, &mut b)?;
            env.push(Binding {
                name: name.clone(),
                atom,
                ty: vty,
            });
        }

        let tail = self.lower_tail(&block.tail, &env, &mut b)?;
        Ok(fold_lets(b.lets, tail))
    }

    fn lower_tail(
        &self,
        tail: &Option<Tail>,
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Core, LowerError> {
        match tail {
            None => Ok(Core::Atom(Atom::Lit(Literal::Unit))),
            Some(Tail::Return(None)) => Ok(Core::Atom(Atom::Lit(Literal::Unit))),
            Some(Tail::Expr(e)) | Some(Tail::Return(Some(e))) => self.emit_tail(e, env, b),
            Some(Tail::If(ife)) => self.lower_if(ife, env, b),
            Some(Tail::Match(m)) => self.lower_match(m, env, b),
        }
    }

    /// Lower an `if`/`else` chain to a `bool` `Match`. Branch order follows
    /// variant tag: `false` (tag 0) then `true` (tag 1).
    fn lower_if(&self, ife: &IfExpr, env: &[Binding], b: &mut Builder) -> Result<Core, LowerError> {
        let scrutinee = self.emit_atom(&ife.cond, env, b)?;
        // The branches open at the depth reached after evaluating the condition.
        let branch_depth = b.depth();
        let then_core = self.lower_block(&ife.then, env, branch_depth)?;
        let else_core = match &ife.els {
            None => Core::Atom(Atom::Lit(Literal::Unit)),
            Some(Else::Block(blk)) => self.lower_block(blk, env, branch_depth)?,
            Some(Else::If(inner)) => {
                // The nested `else if` is the else-branch's value: lower it with
                // its own spine so the inner condition's bindings stay scoped to
                // that branch.
                let mut ib = Builder::new(branch_depth);
                let m = self.lower_if(inner, env, &mut ib)?;
                fold_lets(ib.lets, m)
            }
        };
        Ok(Core::Match {
            scrutinee,
            branches: vec![
                Branch {
                    binds: 0,
                    body: else_core,
                },
                Branch {
                    binds: 0,
                    body: then_core,
                },
            ],
        })
    }

    /// Lower a `match` to a tag-indexed [`Core::Match`] (`spec/02` §C). Branch `i`
    /// covers variant tag `i`; arms may be written in any order and are reordered
    /// here. A `_` arm fills every otherwise-uncovered tag. Branches stop at the
    /// first uncovered tag, so a non-exhaustive `match` yields *fewer* branches
    /// than the enum has variants — exactly what the M2 checker counts to fire its
    /// exhaustiveness diagnostic.
    fn lower_match(
        &self,
        m: &MatchExpr,
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Core, LowerError> {
        let scrutinee = self.emit_atom(&m.scrutinee, env, b)?;
        let branch_depth = b.depth();

        // Determine the enum from the first constructor pattern, then its variant
        // count. (An all-`_` match has no variant set M1 can resolve.)
        let enum_qual = self.match_enum(m)?;
        let count = self.reg.variant_count.get(&enum_qual).copied().unwrap_or(0);

        // Lower each arm into the slot of the tag it covers; a `_` arm is held
        // aside to fill the gaps afterward.
        let mut slots: Vec<Option<Branch>> = (0..count).map(|_| None).collect();
        let mut wildcard: Option<Core> = None;
        for arm in &m.arms {
            match &arm.pat {
                Pattern::Wildcard => {
                    if wildcard.is_none() {
                        wildcard = Some(self.lower_arm_body(&arm.body, env, branch_depth)?);
                    }
                }
                Pattern::Ctor { path, fields } => {
                    let key = path.join(".");
                    let cref = self
                        .reg
                        .ctor(&key)
                        .ok_or_else(|| LowerError::UnknownConstructor { name: key.clone() })?;
                    if cref.enum_qual != enum_qual {
                        return Err(LowerError::MixedEnumPatterns {
                            expected: enum_qual.clone(),
                            found: cref.enum_qual.clone(),
                        });
                    }
                    let tag = cref.tag as usize;
                    // Bind the variant's fields at levels [branch_depth, +binds).
                    let binds = fields.len() as u32;
                    let mut benv = env.to_vec();
                    for (i, fp) in fields.iter().enumerate() {
                        if let FieldPat::Bind(name) = fp {
                            benv.push(Binding {
                                name: name.clone(),
                                atom: Atom::Var(branch_depth + i as u32),
                                ty: None,
                            });
                        }
                    }
                    let body = self.lower_arm_body(&arm.body, &benv, branch_depth + binds)?;
                    if tag < slots.len() && slots[tag].is_none() {
                        slots[tag] = Some(Branch { binds, body });
                    }
                }
            }
        }

        // Fill remaining tags with the `_` body (binds 0), if present.
        if let Some(w) = &wildcard {
            for slot in slots.iter_mut() {
                if slot.is_none() {
                    *slot = Some(Branch {
                        binds: 0,
                        body: w.clone(),
                    });
                }
            }
        }

        // Branches are the covered prefix: stop at the first uncovered tag.
        let mut branches = Vec::with_capacity(slots.len());
        for slot in slots {
            match slot {
                Some(br) => branches.push(br),
                None => break,
            }
        }

        Ok(Core::Match {
            scrutinee,
            branches,
        })
    }

    /// The fully-qualified enum a `match`'s arms range over, taken from its first
    /// constructor pattern.
    fn match_enum(&self, m: &MatchExpr) -> Result<String, LowerError> {
        for arm in &m.arms {
            if let Pattern::Ctor { path, .. } = &arm.pat {
                let key = path.join(".");
                let cref = self
                    .reg
                    .ctor(&key)
                    .ok_or_else(|| LowerError::UnknownConstructor { name: key.clone() })?;
                return Ok(cref.enum_qual.clone());
            }
        }
        Err(LowerError::MatchWithoutConstructor)
    }

    /// Lower a `match` arm body (an expression or a block) into a self-contained
    /// `Core` with its own `let` spine, opened at `base_depth`.
    fn lower_arm_body(
        &self,
        body: &ArmBody,
        env: &[Binding],
        base_depth: u32,
    ) -> Result<Core, LowerError> {
        match body {
            ArmBody::Expr(e) => {
                let mut bb = Builder::new(base_depth);
                let core = self.emit_tail(e, env, &mut bb)?;
                Ok(fold_lets(bb.lets, core))
            }
            ArmBody::Block(blk) => self.lower_block(blk, env, base_depth),
        }
    }

    // ---- expressions ----------------------------------------------------

    /// Lower `e` to an **atom**, hoisting any compound computation into a `let`
    /// recorded in `b`. Atomic expressions add no binding.
    fn emit_atom(&self, e: &Expr, env: &[Binding], b: &mut Builder) -> Result<Atom, LowerError> {
        match e {
            Expr::Unit => Ok(Atom::Lit(Literal::Unit)),
            Expr::Int(n) => Ok(Atom::Lit(Literal::Int(*n))),
            Expr::Bool(v) => Ok(Atom::Lit(Literal::Bool(*v))),
            Expr::Str(s) => Ok(Atom::Lit(Literal::Str(s.clone()))),
            Expr::Var(name) => {
                // A local binding wins over a same-named constructor; otherwise a
                // bare nullary variant (`None`) is a `Ctor`.
                if let Some(a) = self.resolve_local(name, env) {
                    Ok(a)
                } else if let Some(c) = self.nullary_ctor(name) {
                    Ok(b.push(ctor_node(&c, Vec::new())))
                } else {
                    Ok(Atom::Global(symbol_hash(&self.qualify_value(name))))
                }
            }
            Expr::Binary(l, op, r) => {
                let al = self.emit_atom(l, env, b)?;
                let ar = self.emit_atom(r, env, b)?;
                Ok(b.push(Core::Prim {
                    op: prim_op(*op),
                    args: vec![al, ar],
                }))
            }
            Expr::Field(base, field) => {
                // `Enum.Variant` (nullary, e.g. `Option.None`) is a constructor,
                // not a field projection.
                if let Some(c) = self.field_nullary_ctor(base, field, env) {
                    return Ok(b.push(ctor_node(&c, Vec::new())));
                }
                let ab = self.emit_atom(base, env, b)?;
                let idx = self.resolve_proj(base, field, env)?;
                Ok(b.push(Core::Proj { base: ab, idx }))
            }
            Expr::Call(callee, args) => {
                if let Some(c) = self.callee_ctor(callee, env) {
                    let node = self.emit_ctor(&c, args, env, b)?;
                    return Ok(b.push(node));
                }
                let (func, eff_args) = self.call_parts(callee, args, env, b)?;
                let mut cur = func;
                for arg_e in eff_args {
                    let aa = self.emit_atom(arg_e, env, b)?;
                    cur = b.push(Core::App { func: cur, arg: aa });
                }
                Ok(cur)
            }
        }
    }

    /// Lower `e` as a block's **tail** computation: like [`Self::emit_atom`] but
    /// the final node is returned *unbound* (it is the block's result), avoiding
    /// a redundant trailing copy.
    fn emit_tail(&self, e: &Expr, env: &[Binding], b: &mut Builder) -> Result<Core, LowerError> {
        match e {
            Expr::Unit | Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Var(_) => {
                Ok(Core::Atom(self.emit_atom(e, env, b)?))
            }
            Expr::Binary(l, op, r) => {
                let al = self.emit_atom(l, env, b)?;
                let ar = self.emit_atom(r, env, b)?;
                Ok(Core::Prim {
                    op: prim_op(*op),
                    args: vec![al, ar],
                })
            }
            Expr::Field(base, field) => {
                if let Some(c) = self.field_nullary_ctor(base, field, env) {
                    return Ok(ctor_node(&c, Vec::new()));
                }
                let ab = self.emit_atom(base, env, b)?;
                let idx = self.resolve_proj(base, field, env)?;
                Ok(Core::Proj { base: ab, idx })
            }
            Expr::Call(callee, args) => {
                if let Some(c) = self.callee_ctor(callee, env) {
                    return self.emit_ctor(&c, args, env, b);
                }
                let (func, eff_args) = self.call_parts(callee, args, env, b)?;
                let n = eff_args.len();
                let mut cur = func;
                for (i, arg_e) in eff_args.into_iter().enumerate() {
                    let aa = self.emit_atom(arg_e, env, b)?;
                    if i + 1 == n {
                        return Ok(Core::App { func: cur, arg: aa });
                    }
                    cur = b.push(Core::App { func: cur, arg: aa });
                }
                unreachable!("call always has at least one (possibly synthetic) argument")
            }
        }
    }

    // ---- constructor resolution ----------------------------------------

    /// A bare nullary constructor (`None`) — but only if no local binding
    /// shadows the name (checked by the caller) and the variant has no payload.
    fn nullary_ctor(&self, name: &str) -> Option<CtorRef> {
        self.reg.ctor(name).filter(|c| c.arity == 0).cloned()
    }

    /// `Enum.Variant` used without a call, as a nullary constructor — when `base`
    /// is a plain (non-local) name and `Enum.Variant` names an arity-0 variant.
    fn field_nullary_ctor(&self, base: &Expr, field: &str, env: &[Binding]) -> Option<CtorRef> {
        let Expr::Var(en) = base else { return None };
        if self.resolve_local(en, env).is_some() {
            return None; // a real value being projected, not an enum path
        }
        self.reg
            .ctor(&format!("{en}.{field}"))
            .filter(|c| c.arity == 0)
            .cloned()
    }

    /// The constructor a call's *callee* names, if any — a bare `Variant(..)` or
    /// a qualified `Enum.Variant(..)`. A local binding of the same name takes
    /// precedence (it is an ordinary call, not a constructor).
    fn callee_ctor(&self, callee: &Expr, env: &[Binding]) -> Option<CtorRef> {
        match callee {
            Expr::Var(name) if self.resolve_local(name, env).is_none() => {
                self.reg.ctor(name).cloned()
            }
            Expr::Field(base, variant) => {
                let Expr::Var(en) = &**base else { return None };
                if self.resolve_local(en, env).is_some() {
                    return None;
                }
                self.reg.ctor(&format!("{en}.{variant}")).cloned()
            }
            _ => None,
        }
    }

    /// Lower a constructor application's payload atoms and build the [`Core::Ctor`]
    /// node (unbound — the caller binds or returns it).
    fn emit_ctor(
        &self,
        c: &CtorRef,
        args: &[Expr],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Core, LowerError> {
        let fields = args
            .iter()
            .map(|a| self.emit_atom(a, env, b))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ctor_node(c, fields))
    }

    /// Resolve the function atom and the effective argument list of a call,
    /// desugaring a method call `recv.m(args)` into free-function application
    /// `m(recv, args)` (`spec/02` §D).
    fn call_parts<'a>(
        &self,
        callee: &'a Expr,
        args: &'a [Expr],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<(Atom, Vec<&'a Expr>), LowerError> {
        match callee {
            Expr::Field(recv, method) => {
                let func = Atom::Global(symbol_hash(&self.qualify_value(method)));
                let mut eff: Vec<&Expr> = Vec::with_capacity(args.len() + 1);
                eff.push(recv);
                eff.extend(args.iter());
                Ok((func, eff))
            }
            _ => {
                let func = self.emit_atom(callee, env, b)?;
                let eff: Vec<&Expr> = if args.is_empty() {
                    vec![&UNIT_ARG]
                } else {
                    args.iter().collect()
                };
                Ok((func, eff))
            }
        }
    }

    /// The atom of the innermost local binding of `name`, or `None` if it is not
    /// a local (a global, constructor, or free name).
    fn resolve_local(&self, name: &str, env: &[Binding]) -> Option<Atom> {
        env.iter()
            .rev()
            .find(|x| x.name == name)
            .map(|x| x.atom.clone())
    }

    // ---- types & projection --------------------------------------------

    /// Lower a surface type. `generics` are the type-parameter names in scope
    /// (from the enclosing `fn`/`enum`); a bare name matching one becomes a
    /// [`Type::Var`] de Bruijn index (its position in the list).
    fn lower_type(&self, t: &SType, generics: &[String]) -> Type {
        match t {
            SType::Unit => Type::Unit,
            SType::Named(path) => {
                if path.len() == 1 {
                    if let Some(i) = generics.iter().position(|g| g == &path[0]) {
                        return Type::Var(i as u32);
                    }
                }
                self.lower_named(path, &[])
            }
            SType::Generic { path, args } => {
                let lowered: Vec<Type> =
                    args.iter().map(|a| self.lower_type(a, generics)).collect();
                self.lower_named(path, &lowered)
            }
            SType::Slice(inner) => Type::Slice(Box::new(self.lower_type(inner, generics))),
            SType::Ref { mutable, inner } => Type::Ref {
                mutable: *mutable,
                of: Box::new(self.lower_type(inner, generics)),
            },
        }
    }

    /// Resolve a named (possibly generic) type to a [`Type::Nominal`]. A
    /// single-segment name is module-qualified when it denotes an in-module
    /// struct or any in-scope enum, so a type reference and a constructor of that
    /// enum commit to the *same* nominal hash.
    fn lower_named(&self, path: &[String], args: &[Type]) -> Type {
        if path.len() == 1 {
            if args.is_empty() {
                if let Some(builtin) = builtin_type(&path[0]) {
                    return builtin;
                }
            }
            let qualified = if self.structs.contains_key(&path[0]) {
                format!("{}.{}", self.module_path, path[0])
            } else if let Some(q) = self.reg.enum_qualified(&path[0]) {
                q.clone()
            } else {
                path[0].clone()
            };
            return Type::Nominal {
                def: symbol_hash(&qualified),
                args: args.to_vec(),
            };
        }
        Type::Nominal {
            def: symbol_hash(&path.join(".")),
            args: args.to_vec(),
        }
    }

    /// Resolve a field projection `base.field` to a numeric index, using the
    /// best-effort surface type of `base`.
    fn resolve_proj(&self, base: &Expr, field: &str, env: &[Binding]) -> Result<u32, LowerError> {
        let bt = self
            .type_of_expr(base, env)
            .ok_or_else(|| LowerError::UnresolvedProjection {
                field: field.to_string(),
            })?;
        let sname = struct_name(&bt).ok_or_else(|| LowerError::UnresolvedProjection {
            field: field.to_string(),
        })?;
        let fields = self
            .structs
            .get(&sname)
            .ok_or_else(|| LowerError::UnresolvedProjection {
                field: field.to_string(),
            })?;
        fields
            .iter()
            .position(|f| f.name == field)
            .map(|i| i as u32)
            .ok_or_else(|| LowerError::UnknownField {
                ty: sname,
                field: field.to_string(),
            })
    }

    /// Best-effort surface type of an expression — enough to resolve projection
    /// indices on parameters, annotated bindings, field chains over in-module
    /// structs, and calls to in-module functions. Returns `None` when M1 cannot
    /// determine it without the type checker.
    fn type_of_expr(&self, e: &Expr, env: &[Binding]) -> Option<SType> {
        match e {
            Expr::Var(name) => env
                .iter()
                .rev()
                .find(|x| x.name == *name)
                .and_then(|x| x.ty.clone()),
            Expr::Field(base, field) => {
                let bt = self.type_of_expr(base, env)?;
                let sname = struct_name(&bt)?;
                let fields = self.structs.get(&sname)?;
                fields
                    .iter()
                    .find(|f| f.name == *field)
                    .map(|f| f.ty.clone())
            }
            Expr::Call(callee, _) => {
                if let Expr::Var(fname) = &**callee {
                    self.fn_rets.get(fname).cloned().flatten()
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Build a [`Core::Ctor`] for constructor reference `c` with already-lowered
/// payload `fields`. The variant's enum is committed by the *same* symbol hash a
/// nominal type reference to that enum uses, so the checker links them.
fn ctor_node(c: &CtorRef, fields: Vec<Atom>) -> Core {
    Core::Ctor {
        ty: symbol_hash(&c.enum_qual),
        tag: c.tag,
        fields,
    }
}

/// Fold an in-evaluation-order list of `let`-value computations into a
/// right-nested `Let` spine around `tail`.
fn fold_lets(lets: Vec<Core>, tail: Core) -> Core {
    lets.into_iter().rev().fold(tail, |body, value| Core::Let {
        value: Box::new(value),
        body: Box::new(body),
    })
}

/// Map a surface builtin type name to its Core type, or `None` if not a builtin.
fn builtin_type(name: &str) -> Option<Type> {
    Some(match name {
        "i8" => Type::Int(IntTy::I8),
        "i16" => Type::Int(IntTy::I16),
        "i32" => Type::Int(IntTy::I32),
        "i64" => Type::Int(IntTy::I64),
        "isize" => Type::Int(IntTy::Isize),
        "u8" => Type::Int(IntTy::U8),
        "u16" => Type::Int(IntTy::U16),
        "u32" => Type::Int(IntTy::U32),
        "u64" => Type::Int(IntTy::U64),
        "usize" => Type::Int(IntTy::Usize),
        "f32" => Type::Float(FloatTy::F32),
        "f64" => Type::Float(FloatTy::F64),
        "bool" => Type::Bool,
        "str" => Type::Str,
        "char" => Type::Char,
        _ => return None,
    })
}

/// The single-segment struct name a surface type ultimately refers to (peeling
/// references), or `None` if it is not a nominal single-name type.
fn struct_name(t: &SType) -> Option<String> {
    match t {
        SType::Named(path) if path.len() == 1 => Some(path[0].clone()),
        SType::Ref { inner, .. } => struct_name(inner),
        _ => None,
    }
}

/// Map a surface comparison operator to a contract [`CmpOp`], or `None` for a
/// non-comparison operator.
fn cmp_op(op: BinOp) -> Option<CmpOp> {
    Some(match op {
        BinOp::Eq => CmpOp::Eq,
        BinOp::Ne => CmpOp::Ne,
        BinOp::Lt => CmpOp::Lt,
        BinOp::Le => CmpOp::Le,
        BinOp::Gt => CmpOp::Gt,
        BinOp::Ge => CmpOp::Ge,
        _ => return None,
    })
}

/// Map a surface binary operator to its total Core primitive.
fn prim_op(op: BinOp) -> PrimOp {
    match op {
        BinOp::Add => PrimOp::Add,
        BinOp::Sub => PrimOp::Sub,
        BinOp::Mul => PrimOp::Mul,
        BinOp::Div => PrimOp::Div,
        BinOp::Rem => PrimOp::Rem,
        BinOp::Eq => PrimOp::Eq,
        BinOp::Ne => PrimOp::Ne,
        BinOp::Lt => PrimOp::Lt,
        BinOp::Le => PrimOp::Le,
        BinOp::Gt => PrimOp::Gt,
        BinOp::Ge => PrimOp::Ge,
        BinOp::And => PrimOp::And,
        BinOp::Or => PrimOp::Or,
    }
}

// ---- de Bruijn finalization --------------------------------------------

/// Rewrite a Core term built with de Bruijn *levels* in its `Atom::Var`s into one
/// using de Bruijn *indices*. `depth` is the number of binders in scope at this
/// node; a level `L` becomes the index `depth − 1 − L`.
fn to_indices(c: &Core, depth: u32) -> Core {
    match c {
        Core::Atom(a) => Core::Atom(atom_to_index(a, depth)),
        Core::Let { value, body } => Core::Let {
            value: Box::new(to_indices(value, depth)),
            body: Box::new(to_indices(body, depth + 1)),
        },
        Core::Lam {
            param,
            effects,
            body,
        } => Core::Lam {
            param: param.clone(),
            effects: effects.clone(),
            body: Box::new(to_indices(body, depth + 1)),
        },
        Core::App { func, arg } => Core::App {
            func: atom_to_index(func, depth),
            arg: atom_to_index(arg, depth),
        },
        Core::Ctor { ty, tag, fields } => Core::Ctor {
            ty: *ty,
            tag: *tag,
            fields: fields.iter().map(|a| atom_to_index(a, depth)).collect(),
        },
        Core::Proj { base, idx } => Core::Proj {
            base: atom_to_index(base, depth),
            idx: *idx,
        },
        Core::Match {
            scrutinee,
            branches,
        } => Core::Match {
            scrutinee: atom_to_index(scrutinee, depth),
            branches: branches
                .iter()
                .map(|br| Branch {
                    binds: br.binds,
                    body: to_indices(&br.body, depth + br.binds),
                })
                .collect(),
        },
        Core::Prim { op, args } => Core::Prim {
            op: *op,
            args: args.iter().map(|a| atom_to_index(a, depth)).collect(),
        },
        Core::Perform { cap, op, args } => Core::Perform {
            cap: atom_to_index(cap, depth),
            op: *op,
            args: args.iter().map(|a| atom_to_index(a, depth)).collect(),
        },
        Core::Raise { error, args } => Core::Raise {
            error: *error,
            args: args.iter().map(|a| atom_to_index(a, depth)).collect(),
        },
        Core::Loop {
            invariant,
            cond,
            body,
        } => Core::Loop {
            invariant: invariant.clone(),
            cond: Box::new(to_indices(cond, depth)),
            body: Box::new(to_indices(body, depth)),
        },
    }
}

fn atom_to_index(a: &Atom, depth: u32) -> Atom {
    match a {
        Atom::Var(level) => {
            debug_assert!(
                *level < depth,
                "de Bruijn level {level} not in scope at depth {depth} (free variable leaked)"
            );
            Atom::Var(depth - 1 - *level)
        }
        Atom::Global(_) | Atom::Lit(_) => a.clone(),
    }
}
