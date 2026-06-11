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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use marv_syntax::{
    generic_names, ArmBody, BinOp, Block, Else, EnumDecl, Expr, Field, FieldInit, FieldPat, FnDecl,
    IfExpr, Item, LValue, MatchExpr, Module, Pattern, Stmt, StructDecl, Tail, Type as SType, UnOp,
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
    /// A reference `Enum.Variant` (constructor call, nullary reference, or
    /// `match` pattern) whose `Enum` *is* a known enum/error but whose variant
    /// it does not declare — e.g. the typo `Option.Sum(x)`. Carries the declared
    /// variant names (declaration order) so the diagnostic can suggest the
    /// nearest one. Before MARV-37 this fell through to the method-call desugar
    /// and lowered to a semantically wrong `App` the checker accepted silently.
    UnknownEnumVariant {
        enum_name: String,
        variant: String,
        declared: Vec<String>,
    },
    /// A constructor with a payload referenced without being applied (a bare
    /// `Option.Some` where the variant takes arguments). Constructors are not
    /// first-class values; the reference must be a call.
    UnappliedConstructor { name: String, arity: usize },
    /// A constructor or `match` pattern heads on an *imported* name whose
    /// defining module is not part of the lowered set, so its variants/tags are
    /// unknown. Single-file lowering resolves an imported enum only when its
    /// module is lowered alongside (the CLI's `std` discovery, or
    /// [`lower_modules`]); anything else must fail loudly rather than fall
    /// through to a misleading projection error or a silently-wrong call.
    UnresolvedImportedEnum { name: String, module: String },
    /// A struct literal `Name { .. }` whose `Name` is not an in-module struct.
    UnknownStruct { name: String },
    /// A struct literal that omits a field the struct declares.
    MissingStructField { ty: String, field: String },
    /// Assignment to a binding that is not a `var` (a `let`, parameter, or
    /// pattern binding is immutable — `spec/01` §4).
    AssignToImmutable { name: String },
    /// Assignment to a name that is not a binding in scope.
    AssignToUndeclared { name: String },
    /// Index assignment `a[i] = e` whose base is not a fixed-length array. The
    /// functional element update (`spec/01` §4) unrolls over the array's static
    /// length, so a slice (`[]T`, no compile-time length) or a base whose type
    /// M1 cannot resolve is not yet supported.
    IndexAssignUnsupported,
    /// A loop body ends in a `return` tail — early function exit from inside a
    /// loop, which is not lowered yet (MARV-21 threads loop-carried `var`s through
    /// `if`/`match` branch joins, but `return` is out of scope).
    LoopBodyControlFlow,
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
            LowerError::UnknownEnumVariant {
                enum_name,
                variant,
                declared,
            } => {
                write!(
                    f,
                    "`{enum_name}` declares no variant `{variant}` (its variants are {})",
                    declared
                        .iter()
                        .map(|v| format!("`{v}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
                if let Some(s) = closest_name(variant, declared) {
                    write!(f, " — did you mean `{enum_name}.{s}`?")?;
                }
                Ok(())
            }
            LowerError::UnappliedConstructor { name, arity } => write!(
                f,
                "constructor `{name}` takes {arity} argument{} and must be applied \
                 (`{name}(…)`); a constructor is not a first-class value",
                if *arity == 1 { "" } else { "s" }
            ),
            LowerError::UnresolvedImportedEnum { name, module } => write!(
                f,
                "cannot resolve `{name}`: it is imported from `{module}`, but that module's \
                 source was not lowered with this one (an imported enum's constructors resolve \
                 only when its module is in the lowered set — for a `std` import, make the \
                 `std/` directory discoverable or set `MARV_STD`)"
            ),
            LowerError::UnknownStruct { name } => {
                write!(f, "no struct `{name}` is declared in this module")
            }
            LowerError::MissingStructField { ty, field } => {
                write!(f, "struct literal for `{ty}` is missing field `{field}`")
            }
            LowerError::AssignToImmutable { name } => write!(
                f,
                "cannot assign to `{name}`: it is immutable (only a `var` binding may be \
                 reassigned)"
            ),
            LowerError::AssignToUndeclared { name } => {
                write!(f, "cannot assign to `{name}`: no such binding is in scope")
            }
            LowerError::IndexAssignUnsupported => write!(
                f,
                "index assignment `a[i] = e` requires a fixed-length array base (`[N]T`); a slice \
                 (`[]T`) has no compile-time length to unroll the element update over yet"
            ),
            LowerError::LoopBodyControlFlow => write!(
                f,
                "a loop body cannot yet end in a `return` (early function exit from inside a loop \
                 is not lowered yet); an `if`/`match` branch join is supported"
            ),
        }
    }
}

impl std::error::Error for LowerError {}

/// The candidate closest to `name` by edit distance, for "did you mean"
/// suggestions — only when the distance is small relative to the name (a typo,
/// not an unrelated word). Ties keep the first candidate (declaration order),
/// so the suggestion is deterministic.
fn closest_name<'a>(name: &str, candidates: &'a [String]) -> Option<&'a String> {
    let best = candidates
        .iter()
        .map(|c| (edit_distance(name, c), c))
        .min_by_key(|(d, _)| *d)?;
    (best.0 <= 1 + name.len() / 3).then_some(best.1)
}

/// Levenshtein distance over chars (the candidate sets are a handful of short
/// variant names, so the simple O(n·m) row DP is plenty).
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0]; // row[i-1][j-1]
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = if ca == cb {
                prev
            } else {
                1 + prev.min(cur).min(row[j])
            };
            prev = cur;
        }
    }
    row[b.len()]
}

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

/// A whole module lowered to Core: its definitions in source order, plus the
/// generics/interface/impl metadata the checker uses for bound checking and
/// `marv/resolveImpl` reporting (`spec/01` §§3.3–3.4). The metadata lives
/// alongside the names-erased [`Def`]s because bound satisfaction, coherence, and
/// impl selection are properties of the *surface* program, not of any single
/// Core definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredModule {
    pub module: Vec<String>,
    pub defs: Vec<DefEntry>,
    /// Interfaces declared in this module (name + method names).
    pub interfaces: Vec<InterfaceInfo>,
    /// Coherent impls declared in this module: which interface, for which type,
    /// and the qualified names of the method defs the impl provides.
    pub impls: Vec<ImplInfo>,
    /// Generic-function instantiations requested from this module's code, each
    /// recording the concrete type argument(s) and the bound(s) that must hold
    /// (`spec/01` §3.4 — coherent, deterministic resolution). The checker
    /// validates each against the impl set and reports the selected impl.
    pub instantiations: Vec<Instantiation>,
}

/// Metadata for an `interface` declaration (`spec/01` §3.4): its name and the
/// method names it declares. Names survive here because the names-erased [`Def`]
/// cannot carry them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceInfo {
    pub name: String,
    /// The interface's method names, in declaration order.
    pub methods: Vec<String>,
    /// Whether this interface is a **capability** (`spec/01` §5). A capability
    /// interface is a *non-generic* interface: its method calls on a value of
    /// the interface type lower to [`Core::Perform`] / narrowing rather than to
    /// impl-dispatched ordinary calls. Generic interfaces (`Ord[T]`) are bounded
    /// polymorphism and stay `false`. This is the single signal the rest of the
    /// toolchain (`marv-types`' [`World`](../../marv_types/struct.World.html), the
    /// backends) uses to recognise a capability type.
    pub is_capability: bool,
    /// Per-method lowered signatures, in declaration order — the operand and
    /// result types a [`Core::Perform`] site is checked against. The first
    /// parameter of each is the receiver (`&Self`/`Self`); consumers drop it to
    /// recover the operation's operand types.
    pub method_sigs: Vec<InterfaceMethod>,
}

/// One method of an [`InterfaceInfo`], with its types lowered to Core
/// [`Type`]s. For a capability interface this is the source of an operation's
/// [`OpSig`](../../marv_types/struct.OpSig.html).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceMethod {
    pub name: String,
    /// Parameter types in declaration order, *including* the receiver as
    /// `params[0]`.
    pub params: Vec<Type>,
    /// The declared return type (a capability narrowing op returns another
    /// capability type; `()` / `!T` otherwise).
    pub ret: Type,
}

/// Metadata for a coherent `impl` (`spec/01` §3.4): the interface it implements,
/// a canonical key for the concrete type it implements it *for*, and the
/// qualified names of the method definitions it supplies (method → def name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplInfo {
    pub interface: String,
    /// Canonical type key the impl is for, e.g. `"i32"` (see [`type_key`]).
    pub type_key: String,
    /// Method name → fully-qualified def name of the impl method (so a caller can
    /// be told exactly which definition a call resolves to).
    pub methods: Vec<(String, String)>,
}

/// One requested instantiation of a generic function at concrete type arguments
/// (`spec/01` §3.3). Records what the checker needs to verify the interface
/// bounds and report the selected impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instantiation {
    /// The generic function's source name (e.g. `"max"`).
    pub generic: String,
    /// The specialized def's source name (e.g. `"max@i32"`).
    pub instance: String,
    /// One entry per generic type parameter, in order.
    pub args: Vec<TypeArg>,
}

/// A single resolved generic type parameter at an instantiation: the parameter
/// name, the canonical key of the concrete type bound to it, and the interface it
/// was required to satisfy (if the parameter carried a bound).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeArg {
    pub param: String,
    pub type_key: String,
    /// The interface the parameter is bounded by, if any (`T: Ord` → `"Ord"`).
    pub bound: Option<String>,
}

/// Lower a parsed module to Core, hashing each definition and **monomorphizing**
/// every generic-function instantiation its code requests (`spec/01` §§3.3–3.4).
///
/// Enum *constructor* and `match` resolution sees only the enums declared in this
/// module. To lower a module that constructs or matches an enum imported from
/// another file (e.g. `std/result.mv` using `Option`), lower them together with
/// [`lower_modules`], which shares one constructor/interface/impl registry across
/// the set — this is what the CLI does after resolving a file's `import std.*`
/// statements to their source modules. A constructor or `match` pattern that
/// heads on an imported name the registry does not know fails with the explicit
/// [`LowerError::UnresolvedImportedEnum`]; one that heads on a *known* enum
/// (local or resolved-imported) but names a variant it does not declare fails
/// with [`LowerError::UnknownEnumVariant`], and a payload variant referenced
/// without arguments with [`LowerError::UnappliedConstructor`] (never a
/// silently wrong lowering).
pub fn lower_module(m: &Module) -> Result<LoweredModule, LowerError> {
    Ok(lower_modules(std::slice::from_ref(m))?
        .into_iter()
        .next()
        .unwrap())
}

/// Lower several parsed modules that share a namespace (a prelude plus its
/// dependents). A single [`EnumReg`] and [`MonoReg`] are built from *all* of them
/// first, so a `match`, constructor, generic call, or interface method in one
/// module resolves declarations in another. Each module is lowered, then a
/// monomorphization fixpoint generates a specialized def for every distinct
/// generic instantiation requested anywhere in the set; each specialized def is
/// appended to the module that *defines* the generic (so its internal references
/// resolve). Modules are returned in input order.
pub fn lower_modules(ms: &[Module]) -> Result<Vec<LoweredModule>, LowerError> {
    let enum_reg = EnumReg::from_modules(ms);
    let mono = MonoReg::from_modules(ms);
    let pending = Rc::new(RefCell::new(Pending::default()));

    // Phase 1 — base lowering of every module (generic fns keep `Type::Var`;
    // `impl` methods become uniquely-named concrete defs; generic *call sites*
    // record an instantiation request and reference the specialized symbol).
    let mut lowered: Vec<LoweredModule> = Vec::with_capacity(ms.len());
    for m in ms {
        let lw = Lowerer::new(
            m,
            enum_reg.clone(),
            mono.clone(),
            pending.clone(),
            HashMap::new(),
            None,
        );
        lowered.push(lw.lower_base(m)?);
    }

    // Phase 2 — monomorphization fixpoint. Drain the request queue, generating one
    // specialized def per request by re-lowering the generic's declaration with
    // its type parameters bound to the concrete arguments and its interface-method
    // calls dispatched to the resolved coherent impl. Generating an instance may
    // request further instances (a generic that calls another generic).
    loop {
        let req = pending.borrow_mut().queue.pop();
        let Some(req) = req else { break };
        let gf = mono
            .generics
            .get(&req.generic)
            .expect("requested generic is in the registry");
        let gm = &ms[gf.module_index];
        let subst: HashMap<String, SType> = req.subst.iter().cloned().collect();
        let spec = SpecCtx {
            bounds: req
                .spec_bounds
                .iter()
                .map(|(_, iface, key)| (iface.clone(), key.clone()))
                .collect(),
        };
        let lw = Lowerer::new(
            gm,
            enum_reg.clone(),
            mono.clone(),
            pending.clone(),
            subst,
            Some(spec),
        );
        let def = lw.lower_fn(&gf.decl)?;
        let hash = def.content_hash();
        let target = lowered
            .iter_mut()
            .find(|l| l.module == gm.name)
            .expect("the generic's defining module was lowered");
        target.defs.push(DefEntry {
            name: req.instance_name.clone(),
            def,
            hash,
            enum_variants: None,
        });
        target.instantiations.push(Instantiation {
            generic: req.generic.clone(),
            instance: req.instance_name.clone(),
            args: req.args_meta.clone(),
        });
    }

    Ok(lowered)
}

/// A constructor reference resolved against the [`EnumReg`]: which enum it
/// belongs to (fully module-qualified), its tag (= declaration order), and its
/// arity (payload count).
#[derive(Debug, Clone)]
struct CtorRef {
    enum_qual: String,
    tag: u32,
    arity: usize,
    /// `true` when this "constructor" names a variant of an `error` declaration:
    /// referencing it raises (`Core::Raise`) rather than constructing a value
    /// (`Core::Ctor`). See [`ctor_node`].
    is_error: bool,
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
    /// Fully-qualified enum name → its declared variant names, in declaration
    /// order (for the unknown-variant diagnostic and its deterministic
    /// nearest-name suggestion).
    variant_names: HashMap<String, Vec<String>>,
}

impl EnumReg {
    fn from_modules(ms: &[Module]) -> Self {
        let mut reg = EnumReg::default();
        for m in ms {
            let mp = m.name.join(".");
            for item in &m.items {
                // Both `enum` and `error` declarations contribute tag-indexed
                // variants to the registry; an `error`'s variants raise rather
                // than construct (`is_error`). Normalize each into `(name,
                // variant_names, arities, is_error)`.
                let (name, variants, is_error): (&str, Vec<(&str, usize)>, bool) = match item {
                    Item::Enum(e) => (
                        &e.name,
                        e.variants
                            .iter()
                            .map(|v| (v.name.as_str(), v.fields.len()))
                            .collect(),
                        false,
                    ),
                    Item::Error(e) => (
                        &e.name,
                        e.variants.iter().map(|v| (v.as_str(), 0)).collect(),
                        true,
                    ),
                    _ => continue,
                };
                let qual = if mp.is_empty() {
                    name.to_string()
                } else {
                    format!("{mp}.{name}")
                };
                reg.enum_qual.insert(name.to_string(), qual.clone());
                reg.enum_qual.insert(qual.clone(), qual.clone());
                reg.variant_count.insert(qual.clone(), variants.len());
                reg.variant_names.insert(
                    qual.clone(),
                    variants.iter().map(|(v, _)| v.to_string()).collect(),
                );
                for (tag, (vname, arity)) in variants.iter().enumerate() {
                    let cref = CtorRef {
                        enum_qual: qual.clone(),
                        tag: tag as u32,
                        arity: *arity,
                        is_error,
                    };
                    reg.ctors.insert(format!("{name}.{vname}"), cref.clone());
                    reg.ctors.insert(format!("{qual}.{vname}"), cref.clone());
                    // The bare form is registered only while it stays unambiguous.
                    if reg.ambiguous_bare.contains(*vname) {
                        // already known-ambiguous
                    } else if reg.ctors.contains_key(*vname) {
                        reg.ambiguous_bare.insert(vname.to_string());
                        reg.ctors.remove(*vname);
                    } else {
                        reg.ctors.insert(vname.to_string(), cref);
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

/// The cross-module registry of generics, interfaces, and coherent impls that
/// drives monomorphization (`spec/01` §§3.3–3.4). Built once from every module in
/// the set before any lowering, then cloned into each [`Lowerer`].
#[derive(Debug, Clone, Default)]
struct MonoReg {
    /// Generic-function source name → where it is defined and its declaration
    /// (so a specialized copy can be re-lowered). Names are assumed unique across
    /// the module set.
    generics: HashMap<String, GenericFn>,
    /// Interface-method name → the interface that declares it (for dispatch).
    method_iface: HashMap<String, String>,
    /// The coherent impl table: (interface, concrete type key) → the impl entry.
    /// Coherence (one impl per interface-per-type) is *reported* by the checker
    /// from the full per-module [`ImplInfo`] list; this map keeps the last entry
    /// for dispatch.
    impls: HashMap<(String, String), ImplEntry>,
    /// Capability interfaces by name (`spec/01` §5): a *non-generic* interface
    /// whose method calls lower to [`Core::Perform`] / narrowing. Used to detect
    /// a capability receiver at a method-call site and to recover the operation's
    /// index and (narrowed) return type.
    cap_ifaces: HashMap<String, CapIface>,
}

/// A capability interface in the registry: its methods, in declaration order, so
/// a method name resolves to an [`OpId`] and a `let`-binding to a narrowing call
/// recovers the narrowed capability's type.
#[derive(Debug, Clone)]
struct CapIface {
    methods: Vec<CapMethod>,
}

/// One capability operation: its name (to find its [`OpId`] = position) and its
/// surface return type (for narrowing-result inference in [`Lowerer::type_of_expr`]).
#[derive(Debug, Clone)]
struct CapMethod {
    name: String,
    ret: Option<SType>,
}

/// A generic function the registry can re-lower for each instantiation.
#[derive(Debug, Clone)]
struct GenericFn {
    module_index: usize,
    module_path: String,
    decl: FnDecl,
}

/// A resolved impl in the dispatch table: the module that defines its methods and
/// the map from interface-method name to the mangled source name of the impl's
/// definition for that method.
#[derive(Debug, Clone)]
struct ImplEntry {
    module: String,
    methods: HashMap<String, String>,
}

impl MonoReg {
    fn from_modules(ms: &[Module]) -> Self {
        let mut reg = MonoReg::default();
        for (i, m) in ms.iter().enumerate() {
            let mp = m.name.join(".");
            for item in &m.items {
                match item {
                    Item::Fn(f) if !f.generics.is_empty() => {
                        reg.generics.insert(
                            f.name.clone(),
                            GenericFn {
                                module_index: i,
                                module_path: mp.clone(),
                                decl: f.clone(),
                            },
                        );
                    }
                    Item::Interface(iface) => {
                        if iface.generics.is_empty() {
                            // A capability interface (`spec/01` §5): record its
                            // operations so calls on a value of this type lower to
                            // `Perform`. It is *not* registered in `method_iface`
                            // (there is no impl to dispatch to).
                            reg.cap_ifaces.insert(
                                iface.name.clone(),
                                CapIface {
                                    methods: iface
                                        .methods
                                        .iter()
                                        .map(|m| CapMethod {
                                            name: m.name.clone(),
                                            ret: m.ret.clone(),
                                        })
                                        .collect(),
                                },
                            );
                        } else {
                            for meth in &iface.methods {
                                reg.method_iface
                                    .insert(meth.name.clone(), iface.name.clone());
                            }
                        }
                    }
                    Item::Impl(imp) => {
                        let iface = imp.interface.last().cloned().unwrap_or_default();
                        let key = type_key_args(&imp.args);
                        let mut methods = HashMap::new();
                        for meth in &imp.methods {
                            methods.insert(
                                meth.name.clone(),
                                impl_method_name(&meth.name, &iface, &key),
                            );
                        }
                        reg.impls.insert(
                            (iface, key),
                            ImplEntry {
                                module: mp.clone(),
                                methods,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }
        reg
    }
}

/// A monomorphization request: re-lower generic `generic` at the recorded
/// substitution, naming the result `instance_name` (deduplicated by the queue).
#[derive(Debug, Clone)]
struct InstanceReq {
    generic: String,
    instance_name: String,
    /// Generic parameter name → concrete surface type, in parameter order.
    subst: Vec<(String, SType)>,
    /// Only the *bounded* parameters: `(param, interface, type_key)`, used to
    /// dispatch interface-method calls in the specialized body.
    spec_bounds: Vec<(String, String, String)>,
    /// Per-parameter metadata for the checker's bound check (all parameters).
    args_meta: Vec<TypeArg>,
}

/// The shared, mutable monomorphization work queue: pending requests plus the set
/// of instance names already requested (so each distinct instantiation is
/// generated exactly once).
#[derive(Debug, Default)]
struct Pending {
    queue: Vec<InstanceReq>,
    seen: HashSet<String>,
}

/// The interface-dispatch context for lowering a specialized (monomorphic) body:
/// the bound interfaces and the concrete type key each is satisfied at, so a call
/// to an interface method resolves to the coherent impl.
#[derive(Debug, Clone, Default)]
struct SpecCtx {
    /// `(interface, type_key)` pairs from the instantiation's bounds.
    bounds: Vec<(String, String)>,
}

/// A local binding in scope during lowering: the atom it resolves to (with its
/// `Var` carrying a de Bruijn *level*) and its best-known surface type, used to
/// resolve field-projection indices.
#[derive(Debug, Clone)]
struct Binding {
    name: String,
    atom: Atom,
    ty: Option<SType>,
    /// Whether this binding may be reassigned: `true` for a `var`, `false` for a
    /// `let`, a parameter, or a `match`-bound field (`spec/01` §4 — only `var`
    /// bindings are mutable).
    mutable: bool,
    /// Whether this binding is part of a loop-carried variable's lineage: the
    /// loop-header binding, a reassignment of it, or its post-loop projection.
    /// A *fresh* declaration (`let`/`var`/param/pattern bind) is not, even when it
    /// shadows a carried var by the same name. [`Self::next_state_tuple`] resolves
    /// each carried var through this flag so a same-named body-local shadow inside
    /// a branch never hijacks the carried value (MARV-21).
    carried: bool,
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
    /// Imported item names → the dotted module path each was imported from
    /// (`import std.option (Option)` maps `Option` → `std.option`). Used to turn
    /// a constructor/pattern reference to an imported enum the registry does
    /// *not* know into a clear [`LowerError::UnresolvedImportedEnum`].
    imports: HashMap<String, String>,
    /// Struct declarations by source name, for projection-index resolution.
    structs: HashMap<String, Vec<Field>>,
    /// In-module function return types, for best-effort projection typing.
    fn_rets: HashMap<String, Option<SType>>,
    /// Constructor / enum registry (may span several modules; see [`EnumReg`]).
    reg: EnumReg,
    /// Generics / interfaces / impls registry, shared across modules.
    mono: MonoReg,
    /// The shared monomorphization work queue (interior-mutable so the immutable
    /// lowering methods can record instantiation requests as they descend).
    pending: Rc<RefCell<Pending>>,
    /// Active type-parameter substitution. Empty for ordinary (base) lowering;
    /// populated when re-lowering a specialized generic instance (`T` → concrete).
    subst: HashMap<String, SType>,
    /// Interface-dispatch context, set only when lowering a specialized body.
    spec: Option<SpecCtx>,
}

impl Lowerer {
    fn new(
        m: &Module,
        reg: EnumReg,
        mono: MonoReg,
        pending: Rc<RefCell<Pending>>,
        subst: HashMap<String, SType>,
        spec: Option<SpecCtx>,
    ) -> Self {
        let mut imports = HashMap::new();
        for imp in &m.imports {
            let module = imp.path.join(".");
            for n in imp.names.iter().flatten() {
                imports.insert(n.clone(), module.clone());
            }
        }
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
                Item::Enum(_) | Item::Error(_) | Item::Interface(_) | Item::Impl(_) => {}
            }
        }
        Lowerer {
            module_path: m.name.join("."),
            local_items,
            imports,
            structs,
            fn_rets,
            reg,
            mono,
            pending,
            subst,
            spec,
        }
    }

    /// Lower every item of a module's *base* form (no instances yet), collecting
    /// the interface/impl metadata the checker needs. Generic functions lower with
    /// `Type::Var` (kept so the generic body type-checks once); their concrete
    /// instances are generated later by the monomorphization fixpoint
    /// ([`lower_modules`]). `impl` methods become uniquely-named concrete defs.
    fn lower_base(&self, m: &Module) -> Result<LoweredModule, LowerError> {
        let mut defs = Vec::with_capacity(m.items.len());
        let mut interfaces = Vec::new();
        let mut impls = Vec::new();
        for item in &m.items {
            match item {
                Item::Fn(f) => {
                    let def = self.lower_fn(f)?;
                    push_def(&mut defs, f.name.clone(), def, None);
                }
                Item::Struct(s) => {
                    let def = self.lower_struct(s);
                    push_def(&mut defs, s.name.clone(), def, None);
                }
                Item::Enum(e) => {
                    let (def, variants) = self.lower_enum(e);
                    push_def(&mut defs, e.name.clone(), def, Some(variants));
                }
                Item::Error(e) => {
                    let (def, variants) = self.lower_error(e);
                    push_def(&mut defs, e.name.clone(), def, Some(variants));
                }
                Item::Interface(iface) => {
                    push_def(&mut defs, iface.name.clone(), lower_interface(), None);
                    // An interface's method signatures are lowered with its own
                    // type parameters in scope (generic interfaces keep
                    // `Type::Var`); a *non-generic* interface is a capability
                    // (`spec/01` §5), whose method calls become `Perform`.
                    let gnames = generic_names(&iface.generics);
                    let method_sigs = iface
                        .methods
                        .iter()
                        .map(|m| InterfaceMethod {
                            name: m.name.clone(),
                            params: m
                                .params
                                .iter()
                                .map(|p| self.lower_type(&p.ty, &gnames))
                                .collect(),
                            ret: m
                                .ret
                                .as_ref()
                                .map(|t| self.lower_type(t, &gnames))
                                .unwrap_or(Type::Unit),
                        })
                        .collect();
                    interfaces.push(InterfaceInfo {
                        name: iface.name.clone(),
                        methods: iface.methods.iter().map(|s| s.name.clone()).collect(),
                        is_capability: iface.generics.is_empty(),
                        method_sigs,
                    });
                }
                Item::Impl(imp) => {
                    let iface = imp.interface.last().cloned().unwrap_or_default();
                    let key = type_key_args(&imp.args);
                    let mut method_names = Vec::new();
                    for meth in &imp.methods {
                        let mangled = impl_method_name(&meth.name, &iface, &key);
                        let def = self.lower_fn(meth)?;
                        method_names
                            .push((meth.name.clone(), qualify_name(&self.module_path, &mangled)));
                        push_def(&mut defs, mangled, def, None);
                    }
                    impls.push(ImplInfo {
                        interface: iface,
                        type_key: key,
                        methods: method_names,
                    });
                }
            }
        }
        Ok(LoweredModule {
            module: m.name.clone(),
            defs,
            interfaces,
            impls,
            instantiations: Vec::new(),
        })
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

    // ---- monomorphization (`spec/01` §§3.3–3.4) -------------------------

    /// When lowering a specialized body, resolve an interface-method `name` to the
    /// global of the coherent impl selected for the bound it satisfies (`spec/01`
    /// §3.4). Returns `None` outside a specialized body, for a non-interface name,
    /// or when no matching impl exists (the checker reports the unsatisfied bound).
    fn spec_dispatch(&self, name: &str) -> Option<Atom> {
        let spec = self.spec.as_ref()?;
        let iface = self.mono.method_iface.get(name)?;
        for (bound_iface, key) in &spec.bounds {
            if bound_iface == iface {
                if let Some(entry) = self.mono.impls.get(&(iface.clone(), key.clone())) {
                    if let Some(src) = entry.methods.get(name) {
                        return Some(Atom::Global(symbol_hash(&qualify_name(&entry.module, src))));
                    }
                }
            }
        }
        None
    }

    /// If `name` is a generic function, infer its concrete type arguments from the
    /// call's argument types, record a monomorphization request (deduplicated), and
    /// return the global of the specialized instance (`spec/01` §3.3). Returns
    /// `None` when `name` is not generic or its type arguments cannot all be
    /// inferred (the call then lowers as an ordinary, un-specialized reference).
    fn resolve_generic_call(&self, name: &str, args: &[Expr], env: &[Binding]) -> Option<Atom> {
        let gf = self.mono.generics.get(name)?;
        if gf.decl.params.len() != args.len() {
            return None;
        }
        let gnames: HashSet<String> = generic_names(&gf.decl.generics).into_iter().collect();
        // Solve the substitution by unifying each declared parameter type with the
        // inferred argument type.
        let mut map: HashMap<String, SType> = HashMap::new();
        for (p, a) in gf.decl.params.iter().zip(args) {
            let at = self.infer_type(a, env)?;
            unify(&p.ty, &at, &gnames, &mut map);
        }
        // Every generic parameter must be solved to specialize.
        let mut subst = Vec::new();
        let mut keys = Vec::new();
        let mut spec_bounds = Vec::new();
        let mut args_meta = Vec::new();
        for g in &gf.decl.generics {
            let concrete = map.get(&g.name)?.clone();
            let key = type_key(&concrete);
            let bound = g
                .bound
                .as_ref()
                .map(|b| b.path.last().cloned().unwrap_or_default());
            if let Some(iface) = &bound {
                spec_bounds.push((g.name.clone(), iface.clone(), key.clone()));
            }
            args_meta.push(TypeArg {
                param: g.name.clone(),
                type_key: key.clone(),
                bound,
            });
            keys.push(key);
            subst.push((g.name.clone(), concrete));
        }
        let instance_name = format!("{}@{}", name, keys.join(","));
        let dedup = format!("{}::{}", gf.module_path, instance_name);
        {
            let mut pending = self.pending.borrow_mut();
            if pending.seen.insert(dedup) {
                pending.queue.push(InstanceReq {
                    generic: name.to_string(),
                    instance_name: instance_name.clone(),
                    subst,
                    spec_bounds,
                    args_meta,
                });
            }
        }
        Some(Atom::Global(symbol_hash(&qualify_name(
            &gf.module_path,
            &instance_name,
        ))))
    }

    /// Apply the active type-parameter substitution to a surface type (a no-op
    /// when no substitution is active). Used to give a specialized instance's
    /// parameters their concrete surface types.
    fn subst_surface(&self, t: &SType) -> SType {
        if self.subst.is_empty() {
            return t.clone();
        }
        match t {
            SType::Named(p) if p.len() == 1 => match self.subst.get(&p[0]) {
                Some(c) => self.subst_surface(c),
                None => t.clone(),
            },
            SType::Named(_) | SType::Unit => t.clone(),
            SType::Generic { path, args } => SType::Generic {
                path: path.clone(),
                args: args.iter().map(|a| self.subst_surface(a)).collect(),
            },
            SType::Slice(e) => SType::Slice(Box::new(self.subst_surface(e))),
            SType::Array { len, elem } => SType::Array {
                len: *len,
                elem: Box::new(self.subst_surface(elem)),
            },
            SType::Ref { mutable, inner } => SType::Ref {
                mutable: *mutable,
                inner: Box::new(self.subst_surface(inner)),
            },
            SType::ErrorUnion(o) => {
                SType::ErrorUnion(o.as_ref().map(|t| Box::new(self.subst_surface(t))))
            }
            SType::Optional(t) => SType::Optional(Box::new(self.subst_surface(t))),
        }
    }

    /// Best-effort surface type of a call argument, for generic type inference: the
    /// usual [`Self::type_of_expr`], falling back to the literal/`()` types so a
    /// call like `max(3, 7)` solves `T = i32`.
    fn infer_type(&self, e: &Expr, env: &[Binding]) -> Option<SType> {
        if let Some(t) = self.type_of_expr(e, env) {
            return Some(t);
        }
        Some(match e {
            // An unconstrained integer literal defaults to `i32` for inference.
            Expr::Int(_) => SType::Named(vec!["i32".to_string()]),
            Expr::Bool(_) => SType::Named(vec!["bool".to_string()]),
            Expr::Char(_) => SType::Named(vec!["char".to_string()]),
            Expr::Str(_) => SType::Named(vec!["str".to_string()]),
            Expr::Unit => SType::Unit,
            _ => return None,
        })
    }

    // ---- definitions ----------------------------------------------------

    fn lower_struct(&self, s: &StructDecl) -> Def {
        // Field *names* are not part of identity (`spec/02` §F): a struct's
        // content is its ordered field types. `linear` is captured by wrapping.
        // A generic struct's type parameters are in scope for its field types
        // (`struct Pair[T] { a: T, b: T }`), lowering to `Type::Var`.
        let gnames = generic_names(&s.generics);
        let field_tys: Vec<Type> = s
            .fields
            .iter()
            .map(|f| self.lower_type(&f.ty, &gnames))
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
        let gnames = generic_names(&e.generics);
        let mut variant_tys: Vec<Type> = Vec::with_capacity(e.variants.len());
        let mut info: Vec<VariantInfo> = Vec::with_capacity(e.variants.len());
        for v in &e.variants {
            let fields: Vec<Type> = v
                .fields
                .iter()
                .map(|t| self.lower_type(t, &gnames))
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

    /// Lower an `error` declaration to a [`DefKind::Error`] [`Def`] plus its
    /// variant metadata (`spec/01` §6). An error type is an enum-like sum of
    /// nullary variants; its identity is the ordered tuple of (empty) per-variant
    /// payload tuples, with the variant *names* travelling alongside as
    /// non-hashed [`VariantInfo`] so the checker can resolve and exhaustively
    /// match a caught error value and recover its display name.
    fn lower_error(&self, e: &marv_syntax::ErrorDecl) -> (Def, Vec<VariantInfo>) {
        let mut variant_tys: Vec<Type> = Vec::with_capacity(e.variants.len());
        let mut info: Vec<VariantInfo> = Vec::with_capacity(e.variants.len());
        for v in &e.variants {
            variant_tys.push(Type::Tuple(Vec::new()));
            info.push(VariantInfo {
                name: v.clone(),
                fields: Vec::new(),
            });
        }
        let def = Def {
            kind: DefKind::Error,
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
        let gnames = generic_names(&f.generics);
        let synth_unit = f.params.is_empty();
        let param_ctys: Vec<Type> = if synth_unit {
            vec![Type::Unit]
        } else {
            f.params
                .iter()
                .map(|p| self.lower_type(&p.ty, &gnames))
                .collect()
        };

        // Params occupy de Bruijn levels 0..n; the body lowers at depth n. When
        // re-lowering a specialized instance, the binding's surface type is the
        // *substituted* (concrete) one so nested generic calls infer the right
        // type arguments and projections resolve.
        let mut env: Vec<Binding> = Vec::new();
        if !synth_unit {
            for (i, p) in f.params.iter().enumerate() {
                env.push(Binding {
                    name: p.name.clone(),
                    atom: Atom::Var(i as u32),
                    ty: Some(self.subst_surface(&p.ty)),
                    // Parameters are passed by value and are not reassignable.
                    mutable: false,
                    carried: false,
                });
            }
        }
        let n = param_ctys.len() as u32;
        let body_core = self.lower_block(&f.body, &env, n)?;

        let ret_ty = f
            .ret
            .as_ref()
            .map(|t| self.lower_type(t, &gnames))
            .unwrap_or(Type::Unit);
        // The declared **capability** row is the set of capability-typed
        // parameters (`spec/01` §5 — power enters only through a capability
        // parameter). `pure fn` asserts the empty row regardless. The error set
        // stays inferred (a `@error-union` marker in the return type, not the
        // row), so only caps are recorded here. The innermost arrow/lambda
        // carries the row; partial-application arrows are pure.
        let mut effects = EffectRow::empty();
        if !f.is_pure {
            for p in &f.params {
                if let Some(cap) = self.cap_name_of(&self.subst_surface(&p.ty)) {
                    let h = symbol_hash(&cap);
                    if !effects.caps.contains(&h) {
                        effects.caps.push(h);
                    }
                }
            }
        }

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
            // `not p` is logical negation of a predicate (`spec/02` §B `unary`).
            Expr::Unary(UnOp::Not, inner) => Ok(Pred::Not(Box::new(self.lower_pred(
                inner,
                params,
                allow_result,
            )?))),
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
        self.lower_stmts(&block.stmts, &mut env, &mut b)?;
        let tail = self.lower_tail(&block.tail, &env, &mut b)?;
        Ok(fold_lets(b.lets, tail))
    }

    /// Lower a sequence of statements into `b`, threading bindings through `env`.
    /// Shared by [`Self::lower_block`] and the loop-body lowering (a loop body is a
    /// statement sequence whose tail value is discarded).
    fn lower_stmts(
        &self,
        stmts: &[Stmt],
        env: &mut Vec<Binding>,
        b: &mut Builder,
    ) -> Result<(), LowerError> {
        for stmt in stmts {
            match stmt {
                Stmt::Let { name, ty, value } | Stmt::Var { name, ty, value } => {
                    let mutable = matches!(stmt, Stmt::Var { .. });
                    // Best-effort surface type for the bound name (annotation
                    // first, else inferred from the value where M1 can).
                    let vty = ty.clone().or_else(|| self.type_of_expr(value, env));
                    let atom = self.emit_atom(value, env, b)?;
                    env.push(Binding {
                        name: name.clone(),
                        atom,
                        ty: vty,
                        mutable,
                        // A fresh `let`/`var` declaration starts no carried lineage,
                        // even if it shadows a carried var of the same name.
                        carried: false,
                    });
                }
                Stmt::Assign { target, value } => {
                    self.lower_assign(target, value, env, b)?;
                }
                Stmt::While {
                    cond,
                    invariants,
                    body,
                } => {
                    self.lower_while(cond, invariants, body, env, b)?;
                }
                Stmt::For { binder, iter, body } => {
                    self.lower_for(binder, iter, body, env, b)?;
                }
            }
        }
        Ok(())
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
        // The ordinary `match`: each arm's body lowers to its own value.
        self.lower_match_arms(m, env, b, |this, body, benv, depth| {
            this.lower_arm_body(body, benv, depth)
        })
    }

    /// Shared arm-dispatch for `match` lowering: resolve the enum from the first
    /// constructor pattern, place each arm in the slot of the tag it covers
    /// (lowering its body via `lower_body`), fill the gaps with the `_` arm, and
    /// return the covered-prefix [`Core::Match`]. Branches stop at the first
    /// uncovered tag, so a non-exhaustive `match` yields *fewer* branches than the
    /// enum has variants — exactly what the M2 checker counts to fire its
    /// exhaustiveness diagnostic (`spec/02` §C; arms may be written in any order
    /// and are reordered here).
    ///
    /// `lower_body` lowers an arm body at a given environment + base depth: the
    /// arm's ordinary value for [`Self::lower_match`], or the loop's next-state
    /// tuple for the branch-join variant ([`Self::lower_loop_match`], MARV-21).
    fn lower_match_arms<F>(
        &self,
        m: &MatchExpr,
        env: &[Binding],
        b: &mut Builder,
        mut lower_body: F,
    ) -> Result<Core, LowerError>
    where
        F: FnMut(&Self, &ArmBody, &[Binding], u32) -> Result<Core, LowerError>,
    {
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
                        wildcard = Some(lower_body(self, &arm.body, env, branch_depth)?);
                    }
                }
                Pattern::Ctor { path, fields } => {
                    let cref = self.pattern_ctor(path)?;
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
                                // Pattern-bound fields are immutable.
                                mutable: false,
                                carried: false,
                            });
                        }
                    }
                    let body = lower_body(self, &arm.body, &benv, branch_depth + binds)?;
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
                let cref = self.pattern_ctor(path)?;
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
            Expr::Char(c) => Ok(Atom::Lit(Literal::Char(*c))),
            // `e?` (`spec/02` §D): with errors modeled as an effect that
            // propagates by unwinding (a `Raise` aborts the computation), the
            // success value of a non-raising `e` *is* its value, so `?` lowers to
            // the operand's value. The propagated error joins the enclosing
            // function's inferred set through the callee's effect row (`App`), and
            // the checker types `e?` as the operand's success type.
            Expr::Try(inner) => self.emit_atom(inner, env, b),
            // `e as T` (`spec/01` §3.1) → a `Cast` carrying the target type.
            Expr::Cast(inner, ty) => {
                let value = self.emit_atom(inner, env, b)?;
                Ok(b.push(Core::Cast {
                    value,
                    to: self.lower_type(ty, &[]),
                }))
            }
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
            // A prefix unary (`spec/02` §B `unary`): `-e`/`not e` map to a unary
            // `Prim`; `&e`/`&mut e` map to `Core::Ref` (`spec/01` §4).
            Expr::Unary(op, operand) => {
                let a = self.emit_atom(operand, env, b)?;
                Ok(b.push(unary_core(*op, a)))
            }
            Expr::Field(base, field) => {
                // `Enum.Variant` (nullary, e.g. `Option.None`) is a constructor,
                // not a field projection.
                if let Some(c) = self.field_nullary_ctor(base, field, env) {
                    return Ok(b.push(ctor_node(&c, Vec::new())));
                }
                self.check_field_base(base, field, env)?;
                let ab = self.emit_atom(base, env, b)?;
                let idx = self.resolve_proj(base, field, env)?;
                Ok(b.push(Core::Proj { base: ab, idx }))
            }
            Expr::Index(base, index) => {
                let ab = self.emit_atom(base, env, b)?;
                let ai = self.emit_atom(index, env, b)?;
                Ok(b.push(Core::Prim {
                    op: PrimOp::Index,
                    args: vec![ab, ai],
                }))
            }
            Expr::Array(items) => {
                let node = self.emit_array(items, env, b)?;
                Ok(b.push(node))
            }
            Expr::Struct { path, fields } => {
                let node = self.emit_struct_lit(path, fields, env, b)?;
                Ok(b.push(node))
            }
            Expr::Call(callee, args) => {
                if let Some(node) = self.builtin_call(callee, args, env, b)? {
                    return Ok(b.push(node));
                }
                if let Some(c) = self.callee_ctor(callee, env) {
                    let node = self.emit_ctor(&c, args, env, b)?;
                    return Ok(b.push(node));
                }
                if let Expr::Field(base, method) = &**callee {
                    self.check_field_base(base, method, env)?;
                }
                if let Some(node) = self.perform_call(callee, args, env, b)? {
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

    /// Lower an array literal `[e0, e1, …]` to a [`Core::Array`] (`spec/02` §B
    /// `primary`). Elements are emitted left-to-right (ANF order); the carried
    /// element type is the inferred type of the first element (see
    /// [`Self::array_elem_stype`]).
    fn emit_array(
        &self,
        items: &[Expr],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Core, LowerError> {
        let elem = self.lower_type(&self.array_elem_stype(items, env), &[]);
        let atoms = items
            .iter()
            .map(|e| self.emit_atom(e, env, b))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Core::Array { elem, items: atoms })
    }

    /// Best-effort surface element type of an array literal: the inferred type of
    /// its first element, with a bare integer literal defaulting to `i64` (the
    /// runtime word width, and the width a flexible int literal unifies with) and
    /// an empty array to `()`. Lowering only needs *an* element type so `len` and
    /// `index` over the array resolve; the checker re-derives and enforces
    /// homogeneity from the elements themselves.
    fn array_elem_stype(&self, items: &[Expr], env: &[Binding]) -> SType {
        match items.first() {
            None => SType::Unit,
            Some(e) => self.type_of_expr(e, env).unwrap_or_else(|| match e {
                Expr::Int(_) => SType::Named(vec!["i64".to_string()]),
                Expr::Bool(_) => SType::Named(vec!["bool".to_string()]),
                Expr::Char(_) => SType::Named(vec!["char".to_string()]),
                Expr::Str(_) => SType::Named(vec!["str".to_string()]),
                _ => SType::Unit,
            }),
        }
    }

    /// Lower a call to a built-in that maps to a Core primitive rather than a
    /// function application. Today that is `len(x)` → [`PrimOp::Len`] (`spec/02`
    /// §C), the length of a slice/array/string. A local binding named `len`
    /// shadows the builtin (it is then an ordinary call). Returns `None` when the
    /// call is not a recognized builtin, leaving the normal call path to handle it.
    fn builtin_call(
        &self,
        callee: &Expr,
        args: &[Expr],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Option<Core>, LowerError> {
        if let Expr::Var(name) = callee {
            if name == "len" && args.len() == 1 && self.resolve_local(name, env).is_none() {
                let a = self.emit_atom(&args[0], env, b)?;
                return Ok(Some(Core::Prim {
                    op: PrimOp::Len,
                    args: vec![a],
                }));
            }
        }
        Ok(None)
    }

    /// Lower `e` as a block's **tail** computation: like [`Self::emit_atom`] but
    /// the final node is returned *unbound* (it is the block's result), avoiding
    /// a redundant trailing copy.
    fn emit_tail(&self, e: &Expr, env: &[Binding], b: &mut Builder) -> Result<Core, LowerError> {
        match e {
            Expr::Unit
            | Expr::Int(_)
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Char(_)
            | Expr::Var(_) => Ok(Core::Atom(self.emit_atom(e, env, b)?)),
            // `e?` at a tail position: lower the operand (see `emit_atom`).
            Expr::Try(inner) => self.emit_tail(inner, env, b),
            // `e as T` at a tail position: emit the `Cast` unbound.
            Expr::Cast(inner, ty) => {
                let value = self.emit_atom(inner, env, b)?;
                Ok(Core::Cast {
                    value,
                    to: self.lower_type(ty, &[]),
                })
            }
            Expr::Binary(l, op, r) => {
                let al = self.emit_atom(l, env, b)?;
                let ar = self.emit_atom(r, env, b)?;
                Ok(Core::Prim {
                    op: prim_op(*op),
                    args: vec![al, ar],
                })
            }
            // A prefix unary at a tail position: emit the unary node unbound.
            Expr::Unary(op, operand) => {
                let a = self.emit_atom(operand, env, b)?;
                Ok(unary_core(*op, a))
            }
            Expr::Index(base, index) => {
                let ab = self.emit_atom(base, env, b)?;
                let ai = self.emit_atom(index, env, b)?;
                Ok(Core::Prim {
                    op: PrimOp::Index,
                    args: vec![ab, ai],
                })
            }
            Expr::Array(items) => self.emit_array(items, env, b),
            Expr::Struct { path, fields } => self.emit_struct_lit(path, fields, env, b),
            Expr::Field(base, field) => {
                if let Some(c) = self.field_nullary_ctor(base, field, env) {
                    return Ok(ctor_node(&c, Vec::new()));
                }
                self.check_field_base(base, field, env)?;
                let ab = self.emit_atom(base, env, b)?;
                let idx = self.resolve_proj(base, field, env)?;
                Ok(Core::Proj { base: ab, idx })
            }
            Expr::Call(callee, args) => {
                if let Some(node) = self.builtin_call(callee, args, env, b)? {
                    return Ok(node);
                }
                if let Some(c) = self.callee_ctor(callee, env) {
                    return self.emit_ctor(&c, args, env, b);
                }
                if let Expr::Field(base, method) = &**callee {
                    self.check_field_base(base, method, env)?;
                }
                if let Some(node) = self.perform_call(callee, args, env, b)? {
                    return Ok(node);
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

    /// The clear error for `name` heading a constructor reference when `name`
    /// is an *import* the enum registry does not know: its defining module is
    /// not in the lowered set, so its variants cannot resolve. `None` when
    /// `name` is not an import or the registry knows it (resolution proceeds
    /// normally).
    fn unresolved_import(&self, name: &str) -> Option<LowerError> {
        if self.reg.enum_qualified(name).is_some() {
            return None;
        }
        self.imports
            .get(name)
            .map(|module| LowerError::UnresolvedImportedEnum {
                name: name.to_string(),
                module: module.clone(),
            })
    }

    /// Guard a `Base.field` reference (projection or call) whose base is a
    /// plain non-local name, after constructor resolution has already failed.
    /// Two cases must error here instead of falling through to the projection
    /// path (`UnresolvedProjection`) or the method-call desugar (a semantically
    /// wrong `App` the checker accepts silently):
    ///
    /// - the base is an *unresolvable import* (its module is not in the lowered
    ///   set) → [`LowerError::UnresolvedImportedEnum`] (MARV-18);
    /// - the base **is** a known enum/error, so `Base.field` can only be a
    ///   variant reference: an undeclared variant (the typo `Option.Sum`) →
    ///   [`LowerError::UnknownEnumVariant`], and a declared payload variant
    ///   referenced without arguments (a bare `Option.Some`) →
    ///   [`LowerError::UnappliedConstructor`] (MARV-37).
    ///
    /// A local binding always wins (checked first), so capability method calls
    /// (`io.fs()`) and the ordinary method-call desugar (`point.dist(other)`)
    /// are untouched.
    fn check_field_base(
        &self,
        base: &Expr,
        field: &str,
        env: &[Binding],
    ) -> Result<(), LowerError> {
        let Expr::Var(en) = base else { return Ok(()) };
        if self.resolve_local(en, env).is_some() {
            return Ok(());
        }
        if let Some(e) = self.unresolved_import(en) {
            return Err(e);
        }
        if let Some(qual) = self.reg.enum_qualified(en) {
            // Reached only when the ctor lookup failed or the arity-0 filter
            // rejected a payload variant — distinguish the two.
            if let Some(c) = self.reg.ctor(&format!("{en}.{field}")) {
                return Err(LowerError::UnappliedConstructor {
                    name: format!("{en}.{field}"),
                    arity: c.arity,
                });
            }
            return Err(self.unknown_variant(en, qual.clone(), field));
        }
        Ok(())
    }

    /// Build the unknown-variant error for `enum_name.variant`, carrying the
    /// enum's declared variant names (declaration order) for the suggestion.
    fn unknown_variant(&self, enum_name: &str, qual: String, variant: &str) -> LowerError {
        LowerError::UnknownEnumVariant {
            declared: self
                .reg
                .variant_names
                .get(&qual)
                .cloned()
                .unwrap_or_default(),
            enum_name: enum_name.to_string(),
            variant: variant.to_string(),
        }
    }

    /// Resolve a `match` constructor pattern's path against the registry,
    /// producing the import-aware error when the head names an unresolvable
    /// imported enum, and the unknown-variant error (with the declared
    /// variants) when the head names a *known* enum that does not declare the
    /// pattern's variant.
    fn pattern_ctor(&self, path: &[String]) -> Result<&CtorRef, LowerError> {
        let key = path.join(".");
        if let Some(c) = self.reg.ctor(&key) {
            return Ok(c);
        }
        if path.len() > 1 {
            if let Some(e) = self.unresolved_import(&path[0]) {
                return Err(e);
            }
            let (variant, head) = path.split_last().expect("len > 1");
            let head = head.join(".");
            if let Some(qual) = self.reg.enum_qualified(&head) {
                return Err(self.unknown_variant(&head, qual.clone(), variant));
            }
        }
        Err(LowerError::UnknownConstructor { name: key })
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

    // ---- construction & mutation (MARV-4) -------------------------------

    /// Lower a struct literal `Name { field: expr, ... }` to a [`Core::Ctor`]
    /// (products use tag 0, `spec/02` §C). Field initializers, written in any
    /// order, are reordered into the struct's declaration order — the order
    /// `Ctor`/`Proj` index into — and evaluated in that order.
    fn emit_struct_lit(
        &self,
        path: &[String],
        inits: &[FieldInit],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Core, LowerError> {
        let sname = path.join(".");
        let decl = self
            .structs
            .get(&sname)
            .ok_or_else(|| LowerError::UnknownStruct {
                name: sname.clone(),
            })?;
        // Reject any initializer naming a field the struct does not declare.
        for init in inits {
            if !decl.iter().any(|f| f.name == init.name) {
                return Err(LowerError::UnknownField {
                    ty: sname.clone(),
                    field: init.name.clone(),
                });
            }
        }
        // Emit one atom per declared field, in declaration order; every field
        // must be initialized exactly once.
        let field_names: Vec<String> = decl.iter().map(|f| f.name.clone()).collect();
        let mut fields = Vec::with_capacity(field_names.len());
        for fname in &field_names {
            let init = inits.iter().find(|i| &i.name == fname).ok_or_else(|| {
                LowerError::MissingStructField {
                    ty: sname.clone(),
                    field: fname.clone(),
                }
            })?;
            fields.push(self.emit_atom(&init.value, env, b)?);
        }
        Ok(Core::Ctor {
            ty: self.struct_ty_hash(&sname),
            tag: 0,
            fields,
        })
    }

    /// Lower an assignment `target = value` under mutable value semantics
    /// (`spec/01` §4). The value is evaluated once; the result is threaded into
    /// `target` by [`Self::assign_to`], which rebinds the root `var`.
    fn lower_assign(
        &self,
        target: &LValue,
        value: &Expr,
        env: &mut Vec<Binding>,
        b: &mut Builder,
    ) -> Result<(), LowerError> {
        let new_atom = self.emit_atom(value, env.as_slice(), b)?;
        self.assign_to(target, new_atom, env, b)
    }

    /// Thread a new value into an l-value. Mutable value semantics has no mutable
    /// cells in Core: a `var x = …` reassignment is modeled by *rebinding* `x`
    /// (a fresh [`Binding`] shadows the old one, so later references resolve to
    /// the new value); a field update `p.x = e` rebuilds the aggregate with that
    /// one field replaced (a `Ctor` over the other fields' projections) and
    /// rebinds the root. The recursion bottoms out at the root `var`.
    ///
    /// Rebinding is sound across the whole reachable surface because `if`/`match`
    /// appear only as terminal block *tails* (`spec/02` §B): a branch is the last
    /// thing in its block, so a `var` it mutates is never read again after the
    /// branch joins. The cross-iteration case — a loop body's mutation surviving
    /// to the next iteration — is the job of `Core::Loop` lowering (MARV-2).
    fn assign_to(
        &self,
        lv: &LValue,
        new_atom: Atom,
        env: &mut Vec<Binding>,
        b: &mut Builder,
    ) -> Result<(), LowerError> {
        match lv {
            LValue::Var(name) => {
                let pos = env
                    .iter()
                    .rposition(|x| &x.name == name)
                    .ok_or_else(|| LowerError::AssignToUndeclared { name: name.clone() })?;
                if !env[pos].mutable {
                    return Err(LowerError::AssignToImmutable { name: name.clone() });
                }
                let ty = env[pos].ty.clone();
                // A reassignment continues whatever lineage the binding it resolves
                // to belongs to: rebinding a loop-carried var stays carried; an
                // ordinary `var` stays non-carried.
                let carried = env[pos].carried;
                env.push(Binding {
                    name: name.clone(),
                    atom: new_atom,
                    ty,
                    mutable: true,
                    carried,
                });
                Ok(())
            }
            LValue::Field(base, field) => {
                let base_expr = lvalue_to_expr(base);
                // Resolve the base aggregate's struct (its name → field index of
                // the target and total arity), exactly as a projection would.
                let bt = self
                    .type_of_expr(&base_expr, env.as_slice())
                    .ok_or_else(|| LowerError::UnresolvedProjection {
                        field: field.clone(),
                    })?;
                let sname = struct_name(&bt).ok_or_else(|| LowerError::UnresolvedProjection {
                    field: field.clone(),
                })?;
                let decl =
                    self.structs
                        .get(&sname)
                        .ok_or_else(|| LowerError::UnresolvedProjection {
                            field: field.clone(),
                        })?;
                let idx = decl.iter().position(|f| &f.name == field).ok_or_else(|| {
                    LowerError::UnknownField {
                        ty: sname.clone(),
                        field: field.clone(),
                    }
                })? as u32;
                let n = decl.len();
                let ty_hash = self.struct_ty_hash(&sname);

                // The current value of the aggregate being updated.
                let base_atom = self.emit_atom(&base_expr, env.as_slice(), b)?;
                // Rebuild it: the target field takes the new value; every other
                // field is projected from the current aggregate.
                let mut new_fields = Vec::with_capacity(n);
                for j in 0..n as u32 {
                    if j == idx {
                        new_fields.push(new_atom.clone());
                    } else {
                        new_fields.push(b.push(Core::Proj {
                            base: base_atom.clone(),
                            idx: j,
                        }));
                    }
                }
                let rebuilt = b.push(Core::Ctor {
                    ty: ty_hash,
                    tag: 0,
                    fields: new_fields,
                });
                self.assign_to(base, rebuilt, env, b)
            }
            LValue::Index(base, index) => {
                // `a[i] = e` under mutable value semantics (`spec/01` §4): produce
                // a fresh collection equal to the old one except position `i` holds
                // the new value, then rebind the root.
                let base_expr = lvalue_to_expr(base);
                let bt = self
                    .type_of_expr(&base_expr, env.as_slice())
                    .ok_or(LowerError::IndexAssignUnsupported)?;
                let (len, elem_st) = match peel_ref_ty(bt) {
                    SType::Array { len, elem } => (len, *elem),
                    // A slice `[]T` has no compile-time length, so the static
                    // unroll below cannot express its store. Emit a runtime
                    // functional update ([`Core::IndexSet`], MARV-33): the backends
                    // allocate a fresh `[len, …]` block, copy it, and overwrite the
                    // one element. The result rebinds the root just like the array
                    // path.
                    SType::Slice(_) => {
                        let arr_atom = self.emit_atom(&base_expr, env.as_slice(), b)?;
                        let idx_atom = self.emit_atom(index, env.as_slice(), b)?;
                        let rebuilt = b.push(Core::IndexSet {
                            base: arr_atom,
                            index: idx_atom,
                            value: new_atom,
                        });
                        return self.assign_to(base, rebuilt, env, b);
                    }
                    _ => return Err(LowerError::IndexAssignUnsupported),
                };
                let elem = self.lower_type(&elem_st, &[]);
                // Evaluate the current array and the index once.
                let arr_atom = self.emit_atom(&base_expr, env.as_slice(), b)?;
                let idx_atom = self.emit_atom(index, env.as_slice(), b)?;
                // Rebuild element-by-element: position `j` is the new value when
                // `j == i`, else the old `a[j]`. With `len` known this unrolls into
                // `len` selects, reusing the array read + a two-arm `bool` `Match`
                // (no new backend machinery).
                let mut items = Vec::with_capacity(len as usize);
                for j in 0..len {
                    let jlit = Atom::Lit(Literal::Int(j as i64));
                    let old_j = b.push(Core::Prim {
                        op: PrimOp::Index,
                        args: vec![arr_atom.clone(), jlit.clone()],
                    });
                    let is_j = b.push(Core::Prim {
                        op: PrimOp::Eq,
                        args: vec![idx_atom.clone(), jlit],
                    });
                    let sel = b.push(Core::Match {
                        scrutinee: is_j,
                        branches: vec![
                            // tag 0 (false): keep the old element.
                            Branch {
                                binds: 0,
                                body: Core::Atom(old_j),
                            },
                            // tag 1 (true): take the new value.
                            Branch {
                                binds: 0,
                                body: Core::Atom(new_atom.clone()),
                            },
                        ],
                    });
                    items.push(sel);
                }
                let rebuilt = b.push(Core::Array { elem, items });
                self.assign_to(base, rebuilt, env, b)
            }
        }
    }

    // ---- loops (MARV-2) -------------------------------------------------

    /// Lower a `while cond { invariant e }* body` statement to a [`Core::Loop`]
    /// (`spec/02` §D). The loop-carried variables are the in-scope mutable `var`s
    /// the body reassigns; they are threaded as the loop's `state`, rebound at the
    /// loop header for `cond`/`body`/`invariant`, updated by the body, and rebound
    /// in the enclosing scope from the loop's final-state result. Mutable value
    /// semantics has no cells in Core (`spec/01` §4), so cross-iteration mutation
    /// is this functional state-threading.
    fn lower_while(
        &self,
        cond: &Expr,
        invariants: &[Expr],
        body: &Block,
        env: &mut Vec<Binding>,
        b: &mut Builder,
    ) -> Result<(), LowerError> {
        let carried = self.carried_vars(body, env);
        let k = carried.len() as u32;

        // Initial state: the current atom of each carried var, in the enclosing
        // scope. Their declared types travel along to rebind the finals.
        let state: Vec<Atom> = carried
            .iter()
            .map(|name| {
                self.resolve_local(name, env)
                    .expect("carried var resolved from env")
            })
            .collect();
        let carried_tys: Vec<Option<SType>> = carried
            .iter()
            .map(|name| {
                env.iter()
                    .rev()
                    .find(|x| &x.name == name)
                    .and_then(|x| x.ty.clone())
            })
            .collect();

        // The carried vars occupy de Bruijn levels [header_depth, header_depth+k)
        // inside `cond`/`body`/`invariant`. Build the loop-header environment that
        // rebinds them there (shadowing their enclosing bindings).
        let header_depth = b.depth();
        let mut loop_env = env.clone();
        for (j, name) in carried.iter().enumerate() {
            loop_env.push(Binding {
                name: name.clone(),
                atom: Atom::Var(header_depth + j as u32),
                ty: carried_tys[j].clone(),
                mutable: true,
                // The loop-header binding is the root of this carried var's lineage.
                carried: true,
            });
        }

        // Condition: its own spine opened just above the carried vars.
        let mut cb = Builder::new(header_depth + k);
        let cond_tail = self.emit_tail(cond, &loop_env, &mut cb)?;
        let cond_core = fold_lets(cb.lets, cond_tail);

        // Invariants → a conjoined `Pred` over the header environment (level atoms).
        let invariant = self.lower_loop_invariants(invariants, &loop_env)?;

        // Body: lower its statements (mutating a body-local environment), then
        // thread the carried vars through its tail into the next-state tuple. A
        // straight-line tail bundles them directly; an `if`/`match` tail recurses
        // so every branch yields the tuple at the join point (MARV-21).
        let mut body_env = loop_env.clone();
        let mut bb = Builder::new(header_depth + k);
        self.lower_stmts(&body.stmts, &mut body_env, &mut bb)?;
        let tail_core = self.lower_loop_body_tail(&body.tail, &carried, &body_env, &mut bb)?;
        let body_core = fold_lets(bb.lets, tail_core);

        // Emit the loop; its result is the final-state tuple. Rebind each carried
        // var in the enclosing scope from a projection of that tuple, so code after
        // the loop sees the final values.
        let loop_atom = b.push(Core::Loop {
            state,
            invariant: invariant.map(Box::new),
            cond: Box::new(cond_core),
            body: Box::new(body_core),
        });
        for (j, name) in carried.iter().enumerate() {
            let proj = b.push(Core::Proj {
                base: loop_atom.clone(),
                idx: j as u32,
            });
            env.push(Binding {
                name: name.clone(),
                atom: proj,
                ty: carried_tys[j].clone(),
                mutable: true,
                // The post-loop value continues this var's carried lineage, so an
                // enclosing loop that also carries it threads this projection.
                carried: true,
            });
        }
        Ok(())
    }

    /// The next-state tuple `Ctor` for a loop body: each carried var's current
    /// value in `env`, in carried order. Resolution follows the carried *lineage*
    /// ([`Binding::carried`]), not plain name lookup, so a body-local binding that
    /// shadows a carried var (e.g. a `let` of the same name inside one branch) is
    /// skipped — the carried var passes through unchanged there (MARV-21). A
    /// carried var the body did not reassign resolves to its loop-header atom.
    fn next_state_tuple(&self, carried: &[String], env: &[Binding]) -> Core {
        let next: Vec<Atom> = carried
            .iter()
            .map(|name| {
                env.iter()
                    .rev()
                    .find(|x| x.carried && &x.name == name)
                    .map(|x| x.atom.clone())
                    .expect("carried var resolved through its lineage in loop body")
            })
            .collect();
        Core::Ctor {
            ty: loop_tuple_hash(),
            tag: 0,
            fields: next,
        }
    }

    /// Thread the carried vars through a loop body's tail, yielding a `Core` that
    /// evaluates to the next-state tuple (MARV-21). A straight-line or expression
    /// tail bundles the carried vars after evaluating the tail for its effects; an
    /// `if`/`match` tail recurses, so every branch produces the tuple at the join
    /// — the resulting `Match` evaluates to the carried vars' next values, which
    /// the loop eval threads to the next iteration.
    fn lower_loop_body_tail(
        &self,
        tail: &Option<Tail>,
        carried: &[String],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Core, LowerError> {
        match tail {
            None => Ok(self.next_state_tuple(carried, env)),
            Some(Tail::Expr(e)) => {
                // A loop body's tail value is discarded; emit it for its effects.
                self.emit_atom(e, env, b)?;
                Ok(self.next_state_tuple(carried, env))
            }
            Some(Tail::If(ife)) => self.lower_loop_if(ife, carried, env, b),
            Some(Tail::Match(m)) => self.lower_loop_match(m, carried, env, b),
            // `return` inside a loop body is early function exit (out of scope);
            // keep the clear error for it.
            Some(Tail::Return(_)) => Err(LowerError::LoopBodyControlFlow),
        }
    }

    /// Lower a loop body's `Block` so it evaluates to the next-state tuple: run its
    /// statements (in a body-local environment), then thread the carried vars
    /// through its tail. Used for each `if`/`match` branch of a branch-join loop
    /// body (MARV-21).
    fn lower_loop_body_block(
        &self,
        block: &Block,
        carried: &[String],
        env_in: &[Binding],
        base_depth: u32,
    ) -> Result<Core, LowerError> {
        let mut env = env_in.to_vec();
        let mut b = Builder::new(base_depth);
        self.lower_stmts(&block.stmts, &mut env, &mut b)?;
        let tail = self.lower_loop_body_tail(&block.tail, carried, &env, &mut b)?;
        Ok(fold_lets(b.lets, tail))
    }

    /// [`Self::lower_if`] for a loop body's branch join: each branch yields the
    /// next-state tuple, so the resulting `bool` `Match` evaluates to the carried
    /// vars' next values (MARV-21). A missing `else` passes the carried vars
    /// through unchanged on the false branch.
    fn lower_loop_if(
        &self,
        ife: &IfExpr,
        carried: &[String],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Core, LowerError> {
        let scrutinee = self.emit_atom(&ife.cond, env, b)?;
        let branch_depth = b.depth();
        let then_core = self.lower_loop_body_block(&ife.then, carried, env, branch_depth)?;
        let else_core = match &ife.els {
            // No `else`: the carried vars are unchanged on the false branch.
            None => self.next_state_tuple(carried, env),
            Some(Else::Block(blk)) => {
                self.lower_loop_body_block(blk, carried, env, branch_depth)?
            }
            Some(Else::If(inner)) => {
                let mut ib = Builder::new(branch_depth);
                let m = self.lower_loop_if(inner, carried, env, &mut ib)?;
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

    /// [`Self::lower_match`] for a loop body's branch join: each arm yields the
    /// next-state tuple instead of its own value, so the resulting `Match`
    /// evaluates to the carried vars' next values (MARV-21).
    fn lower_loop_match(
        &self,
        m: &MatchExpr,
        carried: &[String],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Core, LowerError> {
        self.lower_match_arms(m, env, b, |this, body, benv, depth| {
            this.lower_loop_arm_body(body, carried, benv, depth)
        })
    }

    /// Lower a `match` arm body of a branch-join loop so it evaluates to the
    /// next-state tuple. An arm written as a bare expression has its value
    /// discarded (emitted for effects) before the tuple; a block arm threads its
    /// own tail like any loop body (MARV-21).
    fn lower_loop_arm_body(
        &self,
        body: &ArmBody,
        carried: &[String],
        env: &[Binding],
        base_depth: u32,
    ) -> Result<Core, LowerError> {
        match body {
            ArmBody::Expr(e) => {
                let mut bb = Builder::new(base_depth);
                self.emit_atom(e, env, &mut bb)?;
                let ns = self.next_state_tuple(carried, env);
                Ok(fold_lets(bb.lets, ns))
            }
            ArmBody::Block(blk) => self.lower_loop_body_block(blk, carried, env, base_depth),
        }
    }

    /// Lower a `for binder in iter body` statement (`spec/02` §D) by desugaring it
    /// to an index-driven `while` over `iter`:
    ///
    /// ```text
    /// var #for<d> = 0
    /// while #for<d> < len(iter) {
    ///     let binder = iter[#for<d>]
    ///     <body>
    ///     #for<d> = #for<d> + 1
    /// }
    /// ```
    ///
    /// The index name carries the builder depth so nested `for`s never collide,
    /// and `#` cannot start a source identifier, so it never shadows user names.
    /// `len`/index lower to the corresponding `Prim`s, which run on arrays
    /// (MARV-30) and runtime-length slices (MARV-33) across all three backends,
    /// so the desugared loop executes end to end (MARV-20).
    fn lower_for(
        &self,
        binder: &str,
        iter: &Expr,
        body: &Block,
        env: &mut Vec<Binding>,
        b: &mut Builder,
    ) -> Result<(), LowerError> {
        let idx_name = format!("#for{}", b.depth());
        let idx_var = Expr::Var(idx_name.clone());
        let len_call = Expr::Call(Box::new(Expr::Var("len".into())), vec![iter.clone()]);
        let cond = Expr::Binary(Box::new(idx_var.clone()), BinOp::Lt, Box::new(len_call));
        let elem = Expr::Index(Box::new(iter.clone()), Box::new(idx_var.clone()));

        let mut stmts = Vec::with_capacity(body.stmts.len() + 2);
        stmts.push(Stmt::Let {
            name: binder.to_string(),
            ty: None,
            value: elem,
        });
        stmts.extend(body.stmts.iter().cloned());
        stmts.push(Stmt::Assign {
            target: LValue::Var(idx_name.clone()),
            value: Expr::Binary(Box::new(idx_var), BinOp::Add, Box::new(Expr::Int(1))),
        });
        let while_body = Block {
            stmts,
            tail: body.tail.clone(),
        };

        // Declare the index var in the enclosing scope, then lower the `while`.
        let zero = self.emit_atom(&Expr::Int(0), env, b)?;
        env.push(Binding {
            name: idx_name,
            atom: zero,
            ty: Some(SType::Named(vec!["usize".to_string()])),
            mutable: true,
            // A fresh declaration; the desugared `while` re-binds it as carried.
            carried: false,
        });
        self.lower_while(&cond, &[], &while_body, env, b)
    }

    /// The in-scope mutable `var`s a loop body reassigns — its loop-carried state,
    /// in enclosing declaration order. An assignment that resolves to a body-local
    /// shadow (a `let`/`var`/pattern/`for`-binder in scope *at that assignment
    /// site*) does not carry the enclosing binding; only assignments reaching past
    /// every shadow do. Shadowing is tracked per lexical scope, so a `let x` in one
    /// branch never suppresses an outer `x` assigned in another (MARV-21).
    fn carried_vars(&self, body: &Block, env: &[Binding]) -> Vec<String> {
        let mut outer_assigned: Vec<String> = Vec::new();
        let mut shadowed: HashSet<String> = HashSet::new();
        collect_outer_assigned_block(body, &mut shadowed, &mut outer_assigned);
        let mut result: Vec<String> = Vec::new();
        for binding in env {
            if binding.mutable
                && outer_assigned.contains(&binding.name)
                && !result.contains(&binding.name)
            {
                result.push(binding.name.clone());
            }
        }
        result
    }

    /// Lower a loop's `invariant` clauses to a single conjoined [`Pred`] over the
    /// loop-header environment (or `None` when there are none).
    fn lower_loop_invariants(
        &self,
        invariants: &[Expr],
        loop_env: &[Binding],
    ) -> Result<Option<Pred>, LowerError> {
        let mut acc: Option<Pred> = None;
        for e in invariants {
            let p = self.lower_loop_pred(e, loop_env)?;
            acc = Some(match acc {
                None => p,
                Some(prev) => Pred::And(Box::new(prev), Box::new(p)),
            });
        }
        Ok(acc)
    }

    /// Lower a loop invariant expression to a [`Pred`]. Unlike a `requires`/
    /// `ensures` contract (which uses a flat parameter convention,
    /// [`Self::lower_pred`]), an invariant's atoms are resolved against the loop
    /// environment as de Bruijn *levels*, so a comparison can mention both
    /// parameters and the loop-carried variables.
    fn lower_loop_pred(&self, e: &Expr, env: &[Binding]) -> Result<Pred, LowerError> {
        match e {
            Expr::Bool(true) => Ok(Pred::True),
            Expr::Bool(false) => Ok(Pred::False),
            Expr::Binary(l, op, r) => match op {
                BinOp::And => Ok(Pred::And(
                    Box::new(self.lower_loop_pred(l, env)?),
                    Box::new(self.lower_loop_pred(r, env)?),
                )),
                BinOp::Or => Ok(Pred::Or(
                    Box::new(self.lower_loop_pred(l, env)?),
                    Box::new(self.lower_loop_pred(r, env)?),
                )),
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let cmp = cmp_op(*op).ok_or(LowerError::ContractNotPredicate)?;
                    let la = self.lower_loop_pred_atom(l, env)?;
                    let ra = self.lower_loop_pred_atom(r, env)?;
                    Ok(Pred::Cmp(cmp, la, ra))
                }
                _ => Err(LowerError::ContractNotPredicate),
            },
            _ => Err(LowerError::ContractNotPredicate),
        }
    }

    /// Lower a loop-invariant comparison operand to an [`Atom`] resolved against
    /// the loop environment. Only atomic operands (a variable, the loop-carried
    /// vars, or a literal) are expressible — `Pred::Cmp` compares atoms.
    fn lower_loop_pred_atom(&self, e: &Expr, env: &[Binding]) -> Result<Atom, LowerError> {
        match e {
            Expr::Int(n) => Ok(Atom::Lit(Literal::Int(*n))),
            Expr::Bool(b) => Ok(Atom::Lit(Literal::Bool(*b))),
            Expr::Var(name) => self
                .resolve_local(name, env)
                .ok_or_else(|| LowerError::UnknownContractVar { name: name.clone() }),
            _ => Err(LowerError::ContractOperandNotAtomic),
        }
    }

    /// The nominal content hash of an in-module struct by source name — the same
    /// hash [`Self::lower_named`] commits to, so a literal/field-rebuild `Ctor`
    /// and a type reference to the struct agree.
    fn struct_ty_hash(&self, sname: &str) -> Hash {
        let qualified = if self.structs.contains_key(sname) {
            format!("{}.{}", self.module_path, sname)
        } else {
            sname.to_string()
        };
        symbol_hash(&qualified)
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
                // A method call `recv.m(args)` desugars to `m(recv, args)`. If `m`
                // is an interface method and we are lowering a specialized body,
                // dispatch it to the resolved coherent impl.
                let func = self
                    .spec_dispatch(method)
                    .unwrap_or_else(|| Atom::Global(symbol_hash(&self.qualify_value(method))));
                let mut eff: Vec<&Expr> = Vec::with_capacity(args.len() + 1);
                eff.push(recv);
                eff.extend(args.iter());
                Ok((func, eff))
            }
            Expr::Var(name) if self.resolve_local(name, env).is_none() => {
                // A free-function call to a bare name. Two monomorphization hooks
                // fire here (`spec/01` §§3.3–3.4), before the ordinary path:
                //   1. an interface method, when lowering a specialized body, is
                //      dispatched to the resolved coherent impl;
                //   2. a generic function is instantiated at the inferred concrete
                //      type arguments, recording a request and referencing the
                //      specialized symbol.
                let func = if let Some(d) = self.spec_dispatch(name) {
                    d
                } else if let Some(g) = self.resolve_generic_call(name, args, env) {
                    g
                } else {
                    self.emit_atom(callee, env, b)?
                };
                let eff: Vec<&Expr> = if args.is_empty() {
                    vec![&UNIT_ARG]
                } else {
                    args.iter().collect()
                };
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

    // ---- capabilities (`spec/01` §5) -----------------------------------

    /// The capability interface name a surface type denotes, if it is one. A
    /// reference to a capability (`&Fs`) still authorizes that capability, so any
    /// `&`/`&mut` is peeled first.
    fn cap_name_of(&self, ty: &SType) -> Option<String> {
        match peel_ref_ty(ty.clone()) {
            SType::Named(p) if p.len() == 1 && self.mono.cap_ifaces.contains_key(&p[0]) => {
                Some(p[0].clone())
            }
            _ => None,
        }
    }

    /// Resolve a method call `recv.method(..)` whose receiver has surface type
    /// `recv_ty` to a capability operation: the capability's name, the operation
    /// index ([`OpId`]), and the operation's surface return type (for narrowing
    /// inference). `None` when the receiver is not a capability or the method is
    /// not one of its operations.
    fn cap_method(&self, recv_ty: &SType, method: &str) -> Option<(String, u32, Option<SType>)> {
        let cap = self.cap_name_of(recv_ty)?;
        let iface = self.mono.cap_ifaces.get(&cap)?;
        let idx = iface.methods.iter().position(|m| m.name == method)?;
        Some((cap, idx as u32, iface.methods[idx].ret.clone()))
    }

    /// Lower a capability method call `recv.op(args)` to a [`Core::Perform`]
    /// (`spec/01` §5): the receiver is the capability value, the remaining
    /// arguments are the operands, and a narrowing op (`io.fs()`) yields a
    /// narrowed capability value. Returns `None` when `callee` is not a
    /// capability method call, leaving the ordinary call path to handle it.
    fn perform_call(
        &self,
        callee: &Expr,
        args: &[Expr],
        env: &[Binding],
        b: &mut Builder,
    ) -> Result<Option<Core>, LowerError> {
        let Expr::Field(recv, method) = callee else {
            return Ok(None);
        };
        let Some(recv_ty) = self.type_of_expr(recv, env) else {
            return Ok(None);
        };
        let Some((_cap, op, _ret)) = self.cap_method(&recv_ty, method) else {
            return Ok(None);
        };
        let cap_atom = self.emit_atom(recv, env, b)?;
        let mut arg_atoms = Vec::with_capacity(args.len());
        for a in args {
            arg_atoms.push(self.emit_atom(a, env, b)?);
        }
        Ok(Some(Core::Perform {
            cap: cap_atom,
            op: OpId(op),
            args: arg_atoms,
        }))
    }

    // ---- types & projection --------------------------------------------

    /// Lower a surface type. `generics` are the type-parameter names in scope
    /// (from the enclosing `fn`/`enum`); a bare name matching one becomes a
    /// [`Type::Var`] de Bruijn index (its position in the list). When a
    /// type-parameter substitution is active (re-lowering a specialized instance),
    /// a bound parameter resolves to its *concrete* type instead, so the instance
    /// carries no `Type::Var`.
    fn lower_type(&self, t: &SType, generics: &[String]) -> Type {
        match t {
            SType::Unit => Type::Unit,
            SType::Named(path) => {
                if path.len() == 1 {
                    if let Some(concrete) = self.subst.get(&path[0]) {
                        return self.lower_type(concrete, generics);
                    }
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
            SType::Array { len, elem } => {
                Type::Array(Box::new(self.lower_type(elem, generics)), *len)
            }
            SType::Ref { mutable, inner } => Type::Ref {
                mutable: *mutable,
                of: Box::new(self.lower_type(inner, generics)),
            },
            // `!T` → `Result[T, error-union(E)]` (`spec/02` §D). The error set `E`
            // is inferred from the body and reported via `marv/errorSet`, not
            // embedded in the type, so the second argument is a fixed
            // `@error-union` marker; the success type is the first argument. Bare
            // `!` is the union over `()`.
            SType::ErrorUnion(payload) => {
                let success = payload
                    .as_ref()
                    .map(|t| self.lower_type(t, generics))
                    .unwrap_or(Type::Unit);
                Type::Nominal {
                    def: symbol_hash("Result"),
                    args: vec![success, error_union_marker()],
                }
            }
            // `?T` → `Option[T]` (`spec/02` §D).
            SType::Optional(inner) => Type::Nominal {
                def: symbol_hash("Option"),
                args: vec![self.lower_type(inner, generics)],
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
            Expr::Call(callee, _) => match &**callee {
                Expr::Var(fname) => self.fn_rets.get(fname).cloned().flatten(),
                // A capability narrowing op (`io.fs()`) has the operation's
                // declared return type, so `let fs = io.fs()` types `fs` as the
                // narrowed capability and its later method calls also lower to
                // `Perform`.
                Expr::Field(recv, method) => {
                    let rt = self.type_of_expr(recv, env)?;
                    self.cap_method(&rt, method).and_then(|(_, _, ret)| ret)
                }
                _ => None,
            },
            // A struct literal has the named struct's type, so a binding to it
            // (`let p = Point { .. }`) resolves field projections on `p`.
            Expr::Struct { path, .. } => Some(SType::Named(path.clone())),
            // Indexing a slice or array yields its element type. The base may be
            // a second-class reference to the collection (`sales: &[]Sale`), so
            // peel any `&`/`&mut` before matching.
            Expr::Index(base, _) => match peel_ref_ty(self.type_of_expr(base, env)?) {
                SType::Slice(inner) => Some(*inner),
                SType::Array { elem, .. } => Some(*elem),
                _ => None,
            },
            // An array literal has a fixed-length array type `[N]T`, where `N` is
            // the element count and `T` the inferred element type. This pins the
            // binder type of `let a = [1, 2, 3]` (so `a[i]`/`len(a)`/`a[i] = e`
            // resolve) without needing the checker.
            Expr::Array(items) => Some(SType::Array {
                len: items.len() as u64,
                elem: Box::new(self.array_elem_stype(items, env)),
            }),
            // A prefix unary's type: `-e` keeps the operand's type, `not e` is
            // `bool`, and `&e`/`&mut e` wrap the operand's type in a reference.
            Expr::Unary(op, operand) => match op {
                UnOp::Neg => self.type_of_expr(operand, env),
                UnOp::Not => Some(SType::Named(vec!["bool".to_string()])),
                UnOp::Ref => self.type_of_expr(operand, env).map(|t| SType::Ref {
                    mutable: false,
                    inner: Box::new(t),
                }),
                UnOp::RefMut => self.type_of_expr(operand, env).map(|t| SType::Ref {
                    mutable: true,
                    inner: Box::new(t),
                }),
            },
            _ => None,
        }
    }
}

/// Reinterpret an l-value as the equivalent expression, so the read-side
/// machinery ([`Lowerer::emit_atom`], [`Lowerer::resolve_proj`],
/// [`Lowerer::type_of_expr`]) can be reused for a field-update's base.
fn lvalue_to_expr(lv: &LValue) -> Expr {
    match lv {
        LValue::Var(name) => Expr::Var(name.clone()),
        LValue::Field(base, field) => Expr::Field(Box::new(lvalue_to_expr(base)), field.clone()),
        LValue::Index(base, index) => Expr::Index(Box::new(lvalue_to_expr(base)), index.clone()),
    }
}

/// A synthetic, deterministic content hash for an anonymous loop-state tuple (the
/// bundle of a loop's carried variables). `@loop-state` cannot be a real
/// qualified name (identifiers never start with `@`), so it never collides with a
/// struct/enum hash; the interpreter and backends treat the loop tuple
/// structurally and ignore this hash, while the checker leaves an unresolved
/// nominal as `Unknown` (it gives the loop an exact `Tuple` type itself).
fn loop_tuple_hash() -> Hash {
    symbol_hash("@loop-state")
}

/// Collect the root names a block assigns that resolve *past* every body-local
/// shadow — the enclosing bindings it actually reassigns. `shadowed` holds the
/// names currently masked by an in-scope `let`/`var`/pattern/`for`-binder; a
/// block opens a fresh scope, adding its own declarations and removing them on
/// exit, so a `let x` in one branch never leaks to a sibling branch (MARV-21).
/// Recurses through nested loop bodies and `if`/`match` branch tails.
fn collect_outer_assigned_block(
    block: &Block,
    shadowed: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    // Names this block newly shadows (to undo on scope exit). A name already
    // shadowed by an outer scope is not re-added here, so it stays masked.
    let mut introduced: Vec<String> = Vec::new();
    for s in &block.stmts {
        match s {
            Stmt::Let { name, .. } | Stmt::Var { name, .. } => {
                if shadowed.insert(name.clone()) {
                    introduced.push(name.clone());
                }
            }
            Stmt::Assign { target, .. } => {
                let root = lvalue_root(target);
                if !shadowed.contains(&root) && !out.contains(&root) {
                    out.push(root);
                }
            }
            Stmt::While { body, .. } => {
                collect_outer_assigned_block(body, shadowed, out);
            }
            Stmt::For { binder, body, .. } => {
                // The `for` binder shadows the enclosing scope within the body.
                let added = shadowed.insert(binder.clone());
                collect_outer_assigned_block(body, shadowed, out);
                if added {
                    shadowed.remove(binder);
                }
            }
        }
    }
    collect_outer_assigned_tail(&block.tail, shadowed, out);
    for n in introduced {
        shadowed.remove(&n);
    }
}

/// Collect outer-assigned roots reachable through a block's tail — an `if`/`match`
/// branch join whose branch blocks can reassign carried vars (MARV-21). Each
/// branch is its own scope; a bare-expression tail holds no assignments.
fn collect_outer_assigned_tail(
    tail: &Option<Tail>,
    shadowed: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    match tail {
        Some(Tail::If(ife)) => collect_outer_assigned_if(ife, shadowed, out),
        Some(Tail::Match(m)) => {
            for arm in &m.arms {
                // A constructor pattern's field binds shadow within the arm only.
                let mut introduced: Vec<String> = Vec::new();
                if let Pattern::Ctor { fields, .. } = &arm.pat {
                    for fp in fields {
                        if let FieldPat::Bind(name) = fp {
                            if shadowed.insert(name.clone()) {
                                introduced.push(name.clone());
                            }
                        }
                    }
                }
                if let ArmBody::Block(blk) = &arm.body {
                    collect_outer_assigned_block(blk, shadowed, out);
                }
                for n in introduced {
                    shadowed.remove(&n);
                }
            }
        }
        None | Some(Tail::Expr(_)) | Some(Tail::Return(_)) => {}
    }
}

/// Collect outer-assigned roots across an `if`/`else if` chain used as a
/// branch-join tail; each arm block opens its own scope (MARV-21).
fn collect_outer_assigned_if(ife: &IfExpr, shadowed: &mut HashSet<String>, out: &mut Vec<String>) {
    collect_outer_assigned_block(&ife.then, shadowed, out);
    match &ife.els {
        None => {}
        Some(Else::Block(blk)) => collect_outer_assigned_block(blk, shadowed, out),
        Some(Else::If(inner)) => collect_outer_assigned_if(inner, shadowed, out),
    }
}

/// The root binding name of an l-value (`a`, `a.x`, `a[i]` all have root `a`).
fn lvalue_root(lv: &LValue) -> String {
    match lv {
        LValue::Var(name) => name.clone(),
        LValue::Field(base, _) => lvalue_root(base),
        LValue::Index(base, _) => lvalue_root(base),
    }
}

/// Build the Core node for a resolved constructor reference `c` with
/// already-lowered payload `fields`. A regular enum variant becomes a
/// [`Core::Ctor`]; an `error` variant becomes a [`Core::Raise`] (`spec/01` §6,
/// `spec/02` §D — referencing an error variant raises it into the error union).
/// In both cases the enum/error type is committed by the *same* symbol hash a
/// nominal type reference to it uses, so the checker links them.
fn ctor_node(c: &CtorRef, fields: Vec<Atom>) -> Core {
    if c.is_error {
        Core::Raise {
            error: symbol_hash(&c.enum_qual),
            args: fields,
        }
    } else {
        Core::Ctor {
            ty: symbol_hash(&c.enum_qual),
            tag: c.tag,
            fields,
        }
    }
}

/// The fixed nominal marker used for the inferred-error-set slot of a lowered
/// `!T` error union (`spec/02` §D `Result[T, error-union(E)]`). The concrete set
/// `E` is inferred and surfaced via `marv/errorSet` rather than embedded in the
/// type; `@error-union` cannot be a real qualified name, so it never collides.
fn error_union_marker() -> Type {
    Type::Nominal {
        def: symbol_hash("@error-union"),
        args: Vec::new(),
    }
}

/// Push a lowered definition onto a base module's def list, computing its hash.
fn push_def(
    defs: &mut Vec<DefEntry>,
    name: String,
    def: Def,
    enum_variants: Option<Vec<VariantInfo>>,
) {
    let hash = def.content_hash();
    defs.push(DefEntry {
        name,
        def,
        hash,
        enum_variants,
    });
}

/// Lower an `interface` declaration to a [`DefKind::Interface`] [`Def`]. An
/// interface declares abstract signatures only (`spec/01` §3.4); it carries no
/// runnable body, and its method *types* are not part of any value's identity, so
/// the Def is a minimal placeholder. Bound checking and impl resolution work over
/// the [`InterfaceInfo`]/[`ImplInfo`] metadata, not this Def.
fn lower_interface() -> Def {
    Def {
        kind: DefKind::Interface,
        ty: Type::Unit,
        requires: Vec::new(),
        ensures: Vec::new(),
        body: None,
    }
}

/// The mangled source name of an impl method: `method$Interface$typekey` (e.g.
/// `cmp$Ord$i32`). `$` cannot appear in a source identifier, so a mangled name
/// never collides with a user definition. The matching dispatch site
/// ([`Lowerer::spec_dispatch`]) and the registry ([`MonoReg`]) compute the same
/// name, so a call resolves to exactly this def.
fn impl_method_name(method: &str, interface: &str, type_key: &str) -> String {
    format!("{method}${interface}${type_key}")
}

/// Module-qualify a name: `module.name`, or just `name` at the empty root module.
fn qualify_name(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}.{name}")
    }
}

/// A canonical, deterministic key for a surface type, used both to mangle
/// instance/impl names and to match an instantiation's type argument against the
/// impl table (`spec/01` §3.4 — deterministic resolution). The exact spelling is
/// internal; only its stability and injectivity matter.
fn type_key(t: &SType) -> String {
    match t {
        SType::Unit => "()".to_string(),
        SType::Named(p) => p.join("."),
        SType::Generic { path, args } => {
            format!("{}[{}]", path.join("."), type_key_args(args))
        }
        SType::Slice(e) => format!("[]{}", type_key(e)),
        SType::Array { len, elem } => format!("[{len}]{}", type_key(elem)),
        SType::Ref { mutable, inner } => {
            let kw = if *mutable { "&mut " } else { "&" };
            format!("{kw}{}", type_key(inner))
        }
        SType::ErrorUnion(Some(t)) => format!("!{}", type_key(t)),
        SType::ErrorUnion(None) => "!".to_string(),
        SType::Optional(t) => format!("?{}", type_key(t)),
    }
}

/// The comma-joined [`type_key`]s of a type-argument list (an `impl`'s concrete
/// types, or a generic instantiation's arguments).
fn type_key_args(args: &[SType]) -> String {
    args.iter().map(type_key).collect::<Vec<_>>().join(",")
}

/// Solve a generic type-parameter substitution by structurally matching a
/// declared parameter type `pat` against an inferred argument type `arg`,
/// recording each generic name's binding in `map`. References are peeled on both
/// sides so `&T` against `&i32` (or against `i32`) still solves `T = i32`.
/// Unsolvable positions are simply skipped; the caller treats any unsolved
/// generic parameter as "cannot specialize".
fn unify(pat: &SType, arg: &SType, generics: &HashSet<String>, map: &mut HashMap<String, SType>) {
    match pat {
        SType::Named(p) if p.len() == 1 && generics.contains(&p[0]) => {
            map.entry(p[0].clone())
                .or_insert_with(|| peel_ref_ty(arg.clone()));
        }
        SType::Ref { inner, .. } => unify(inner, &peel_ref_ty(arg.clone()), generics, map),
        SType::Slice(pe) => {
            if let SType::Slice(ae) = peel_ref_ty(arg.clone()) {
                unify(pe, &ae, generics, map);
            }
        }
        SType::Array { elem: pe, .. } => {
            if let SType::Array { elem: ae, .. } = peel_ref_ty(arg.clone()) {
                unify(pe, &ae, generics, map);
            }
        }
        SType::Generic { args: pargs, .. } => {
            if let SType::Generic { args: aargs, .. } = peel_ref_ty(arg.clone()) {
                for (pa, aa) in pargs.iter().zip(&aargs) {
                    unify(pa, aa, generics, map);
                }
            }
        }
        SType::Optional(pe) => {
            if let SType::Optional(ae) = peel_ref_ty(arg.clone()) {
                unify(pe, &ae, generics, map);
            }
        }
        // Concrete leaves (`Named` non-generic, `Unit`, error unions) constrain
        // nothing.
        _ => {}
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

/// Strip outer `&`/`&mut` references from a surface type, so projection / index
/// resolution sees through a second-class reference to the underlying
/// collection or aggregate (e.g. indexing `sales: &[]Sale`).
fn peel_ref_ty(t: SType) -> SType {
    match t {
        SType::Ref { inner, .. } => peel_ref_ty(*inner),
        other => other,
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

/// Build the Core node for a prefix unary operator over an already-lowered
/// operand atom (`spec/02` §B `unary`): `-`/`not` are unary [`Core::Prim`]s,
/// `&`/`&mut` are [`Core::Ref`]s.
fn unary_core(op: UnOp, operand: Atom) -> Core {
    match op {
        UnOp::Neg => Core::Prim {
            op: PrimOp::Neg,
            args: vec![operand],
        },
        UnOp::Not => Core::Prim {
            op: PrimOp::Not,
            args: vec![operand],
        },
        UnOp::Ref => Core::Ref {
            mutable: false,
            of: operand,
        },
        UnOp::RefMut => Core::Ref {
            mutable: true,
            of: operand,
        },
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
        Core::Array { elem, items } => Core::Array {
            elem: elem.clone(),
            items: items.iter().map(|a| atom_to_index(a, depth)).collect(),
        },
        Core::IndexSet { base, index, value } => Core::IndexSet {
            base: atom_to_index(base, depth),
            index: atom_to_index(index, depth),
            value: atom_to_index(value, depth),
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
        Core::Cast { value, to } => Core::Cast {
            value: atom_to_index(value, depth),
            to: to.clone(),
        },
        Core::Ref { mutable, of } => Core::Ref {
            mutable: *mutable,
            of: atom_to_index(of, depth),
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
            state,
            invariant,
            cond,
            body,
        } => {
            // The loop binds `state.len()` carried variables as the innermost
            // slots of `invariant`/`cond`/`body`; `state` itself is evaluated in
            // the enclosing scope (at `depth`).
            let k = state.len() as u32;
            Core::Loop {
                state: state.iter().map(|a| atom_to_index(a, depth)).collect(),
                invariant: invariant
                    .as_ref()
                    .map(|p| Box::new(pred_to_index(p, depth + k))),
                cond: Box::new(to_indices(cond, depth + k)),
                body: Box::new(to_indices(body, depth + k)),
            }
        }
    }
}

/// Rewrite a [`Pred`]'s de Bruijn *level* atoms into *indices* at binder depth
/// `depth`, mirroring [`to_indices`] for the contract/invariant predicate
/// language. Loop invariants are built with level atoms (resolved from the
/// lowering environment); other predicates already use a flat convention and pass
/// through unchanged because their atoms are not `Var` levels into this scope.
fn pred_to_index(p: &Pred, depth: u32) -> Pred {
    match p {
        Pred::True => Pred::True,
        Pred::False => Pred::False,
        Pred::Cmp(op, l, r) => Pred::Cmp(*op, atom_to_index(l, depth), atom_to_index(r, depth)),
        Pred::And(l, r) => Pred::And(
            Box::new(pred_to_index(l, depth)),
            Box::new(pred_to_index(r, depth)),
        ),
        Pred::Or(l, r) => Pred::Or(
            Box::new(pred_to_index(l, depth)),
            Box::new(pred_to_index(r, depth)),
        ),
        Pred::Not(inner) => Pred::Not(Box::new(pred_to_index(inner, depth))),
        Pred::Forall { domain, body } => Pred::Forall {
            domain: (
                atom_to_index(&domain.0, depth),
                atom_to_index(&domain.1, depth),
            ),
            body: Box::new(pred_to_index(body, depth)),
        },
        Pred::Exists { domain, body } => Pred::Exists {
            domain: (
                atom_to_index(&domain.0, depth),
                atom_to_index(&domain.1, depth),
            ),
            body: Box::new(pred_to_index(body, depth)),
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
