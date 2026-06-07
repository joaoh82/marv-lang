//! The declaration environment the checker resolves names against.
//!
//! The §E judgments are *open*: `(Global)` needs `def(h) : τ`, `(Perform)` needs
//! `op(c) : (τa → τr)`, and `(Match)` needs the variant list of an `enum`. None
//! of that lives in a single [`Def`] — it is the surrounding program. [`World`]
//! is that surrounding program: a lookup table from the content-addressed hashes
//! a Core term mentions to the signatures and declarations they denote.
//!
//! ## Keying: symbol hashes, not content hashes
//!
//! M1 lowering resolves a free reference to `Atom::Global(symbol_hash(name))`
//! and a named type to `Type::Nominal { def: symbol_hash(name), .. }` (see
//! `marv_core::symbol_hash` and its rationale — content hashes are cyclic for
//! recursive defs, so M1 keys on the resolved name and defers true content
//! identity to M7). The checker therefore resolves against the *same* symbol
//! hashes. [`World::from_module`] registers every definition under
//! `symbol_hash("<module>.<name>")`, exactly the key the bodies reference.
//!
//! ## Surface forms the front end does not yet emit
//!
//! Capabilities, errors, and enums have no M0 surface syntax, so
//! [`World::from_module`] cannot discover them — only `fn` and `struct` defs
//! exist to register. Tests that exercise the capability / error-set /
//! exhaustiveness rules build those declarations explicitly through
//! [`WorldBuilder`], pairing them with hand-written Core. This mirrors the rule
//! coverage split documented for M2: type/return/reference rules are reachable
//! from real `.mv` source; the rest are driven over constructed Core IR.

use std::collections::HashMap;

use marv_core::ir::*;
use marv_core::{symbol_hash, LoweredModule};

/// The signature of a capability method (`spec/02` §E `(Perform)`): its operand
/// types, its result type, and the errors performing it may raise. `Perform`'s
/// [`OpId`] indexes into a capability's `ops`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpSig {
    pub params: Vec<Type>,
    pub ret: Type,
    /// Errors this operation may raise (folded into the caller's error set).
    pub errors: Vec<Hash>,
}

/// A capability declaration: a display name (for fixes/messages) and its
/// operations, indexed by [`OpId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapDecl {
    pub name: String,
    pub ops: Vec<OpSig>,
}

/// An error declaration: a display name and the payload types each occurrence
/// carries (its arity is `payload.len()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDecl {
    pub name: String,
    pub payload: Vec<Type>,
}

/// One variant of an enum: a display name and its field types (the arity bound
/// into a covering `match` branch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantDecl {
    pub name: String,
    pub fields: Vec<Type>,
}

/// An enum declaration: a display name and its variants, *ordered by tag*
/// (`spec/02` §C — `Match` branches are ordered by variant tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDecl {
    pub name: String,
    pub variants: Vec<VariantDecl>,
}

/// A struct declaration: a display name, its field types in declaration order
/// (the order `Proj`/`Ctor` index into), and whether it is `linear`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    pub name: String,
    pub fields: Vec<Type>,
    pub linear: bool,
}

/// The declaration environment (see the module docs). Every map is keyed by the
/// symbol hash a Core term uses to reference the entity.
#[derive(Debug, Clone, Default)]
pub struct World {
    /// Top-level value signatures (functions, consts): symbol hash → type.
    globals: HashMap<Hash, Type>,
    /// Capability declarations, keyed by the cap's nominal symbol hash.
    caps: HashMap<Hash, CapDecl>,
    /// Error declarations, keyed by the error's symbol hash.
    errors: HashMap<Hash, ErrorDecl>,
    /// Enum declarations, keyed by the enum's nominal symbol hash.
    enums: HashMap<Hash, EnumDecl>,
    /// Struct declarations, keyed by the struct's nominal symbol hash.
    structs: HashMap<Hash, StructDecl>,
}

impl World {
    /// An empty world.
    pub fn new() -> Self {
        World::default()
    }

    /// Build a world from one lowered module (see [`World::from_modules`] for the
    /// cross-module form). Equivalent to `from_modules(std::slice::from_ref(m))`.
    pub fn from_module(m: &LoweredModule) -> Self {
        World::from_modules(std::slice::from_ref(m))
    }

    /// Build a world from several lowered modules that share a namespace (a
    /// prelude plus its dependents): register every `fn`/`const` under its symbol
    /// hash, every `struct` and `enum` as a nominal declaration. Keyed on
    /// `symbol_hash` (see the module docs).
    ///
    /// Enum variants are recovered from the [`enum_variants`](marv_core::DefEntry::enum_variants)
    /// metadata the lowerer carries alongside the names-erased [`Def`] — the only
    /// place variant *names* survive.
    pub fn from_modules(ms: &[LoweredModule]) -> Self {
        let mut w = World::new();
        for m in ms {
            // Capability interfaces (`spec/01` §5): a non-generic interface whose
            // method calls lower to `Perform`. Each becomes a [`CapDecl`] keyed by
            // its *bare* name's symbol hash — exactly the hash a `Type::Nominal`
            // reference to the interface lowers to (interface names are never
            // module-qualified) — so `synth_perform` resolves it. The receiver
            // (`params[0]`) is dropped: an operation's operands are the remaining
            // parameters.
            for iface in &m.interfaces {
                if !iface.is_capability {
                    continue;
                }
                let ops = iface
                    .method_sigs
                    .iter()
                    .map(|s| OpSig {
                        params: s.params.iter().skip(1).cloned().collect(),
                        ret: s.ret.clone(),
                        errors: Vec::new(),
                    })
                    .collect();
                w.caps.insert(
                    symbol_hash(&iface.name),
                    CapDecl {
                        name: iface.name.clone(),
                        ops,
                    },
                );
            }
            let prefix = m.module.join(".");
            for entry in &m.defs {
                let qualified = if prefix.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{prefix}.{}", entry.name)
                };
                let h = symbol_hash(&qualified);
                match entry.def.kind {
                    DefKind::Struct => {
                        let (fields, linear) = struct_fields(&entry.def.ty);
                        w.structs.insert(
                            h,
                            StructDecl {
                                name: entry.name.clone(),
                                fields,
                                linear,
                            },
                        );
                    }
                    DefKind::Enum => {
                        let variants = entry
                            .enum_variants
                            .as_ref()
                            .map(|vs| {
                                vs.iter()
                                    .map(|v| VariantDecl {
                                        name: v.name.clone(),
                                        fields: v.fields.clone(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        w.enums.insert(
                            h,
                            EnumDecl {
                                name: entry.name.clone(),
                                variants,
                            },
                        );
                    }
                    DefKind::Error => {
                        // An `error` decl is registered twice under its hash: as an
                        // `ErrorDecl` (so `error_name`/error-set reporting resolve
                        // it) and as an enum-like sum (so an exhaustive `match`
                        // over a caught error value is checked). Its variants are
                        // nullary, so the payload is empty.
                        let variants: Vec<VariantDecl> = entry
                            .enum_variants
                            .as_ref()
                            .map(|vs| {
                                vs.iter()
                                    .map(|v| VariantDecl {
                                        name: v.name.clone(),
                                        fields: v.fields.clone(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        w.errors.insert(
                            h,
                            ErrorDecl {
                                name: entry.name.clone(),
                                payload: Vec::new(),
                            },
                        );
                        w.enums.insert(
                            h,
                            EnumDecl {
                                name: entry.name.clone(),
                                variants,
                            },
                        );
                    }
                    // An `interface` declares only abstract signatures — not a
                    // callable value, so it registers nothing. Its method bodies
                    // live in `impl`s, which lower to ordinary `Fn` defs (caught by
                    // the `_` arm below as globals).
                    DefKind::Interface => {}
                    _ => {
                        w.globals.insert(h, entry.def.ty.clone());
                    }
                }
            }
        }
        w
    }

    /// The type of a top-level value reference, or `None` if the world does not
    /// know it (an import or builtin the checker treats opaquely).
    pub fn global(&self, h: &Hash) -> Option<&Type> {
        self.globals.get(h)
    }

    /// The capability declaration for a nominal hash, if it is a known cap.
    pub fn cap(&self, h: &Hash) -> Option<&CapDecl> {
        self.caps.get(h)
    }

    /// Whether `h` names a known capability type.
    pub fn is_cap(&self, h: &Hash) -> bool {
        self.caps.contains_key(h)
    }

    /// The set of capabilities a holder of `declared` is authorized to exercise:
    /// the declared capabilities themselves plus everything reachable from them by
    /// **narrowing** (`spec/01` §5 — attenuation). An operation whose result type
    /// is itself a capability (`io.fs() -> Fs`) is a narrowing edge, so holding
    /// `Io` authorizes the `Fs`/`Net`/… it can narrow to (transitively). Used by
    /// the effect-row subsumption check so a body that narrows a held capability
    /// is not flagged for a capability it never received ambiently.
    pub fn authorized_caps(&self, declared: &[Hash]) -> std::collections::HashSet<Hash> {
        let mut reachable: std::collections::HashSet<Hash> = std::collections::HashSet::new();
        let mut stack: Vec<Hash> = declared.to_vec();
        while let Some(h) = stack.pop() {
            if !reachable.insert(h) {
                continue;
            }
            if let Some(decl) = self.caps.get(&h) {
                for op in &decl.ops {
                    if let Type::Nominal { def, .. } = &op.ret {
                        if self.caps.contains_key(def) && !reachable.contains(def) {
                            stack.push(*def);
                        }
                    }
                }
            }
        }
        reachable
    }

    /// If capability `cap_name`'s operation `op` is a **narrowing** op — its
    /// result type is itself a capability — return that narrowed capability's
    /// display name. Used by the interpreter to give `let fs = io.fs()` a narrowed
    /// capability value (`spec/01` §5) rather than a unit. `None` for an ordinary
    /// (non-narrowing) operation or an unknown cap/op.
    pub fn cap_op_narrows(&self, cap_name: &str, op: u32) -> Option<String> {
        let decl = self.caps.values().find(|c| c.name == cap_name)?;
        let sig = decl.ops.get(op as usize)?;
        if let Type::Nominal { def, .. } = &sig.ret {
            if let Some(narrowed) = self.caps.get(def) {
                return Some(narrowed.name.clone());
            }
        }
        None
    }

    /// The error declaration for a hash, if known.
    pub fn error(&self, h: &Hash) -> Option<&ErrorDecl> {
        self.errors.get(h)
    }

    /// A display name for an error hash (its declared name, or a hash prefix).
    pub fn error_name(&self, h: &Hash) -> String {
        self.errors
            .get(h)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("error#{}", &h.to_hex()[..8]))
    }

    /// A display name for a capability hash.
    pub fn cap_name(&self, h: &Hash) -> String {
        self.caps
            .get(h)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("cap#{}", &h.to_hex()[..8]))
    }

    /// The enum declaration for a nominal hash, if it is a known enum.
    pub fn enum_decl(&self, h: &Hash) -> Option<&EnumDecl> {
        self.enums.get(h)
    }

    /// The struct declaration for a nominal hash, if it is a known struct.
    pub fn struct_decl(&self, h: &Hash) -> Option<&StructDecl> {
        self.structs.get(h)
    }

    /// Start an additive builder seeded with this world's contents.
    pub fn builder(self) -> WorldBuilder {
        WorldBuilder { world: self }
    }
}

/// A small fluent builder for assembling a [`World`] in tests, where caps,
/// errors, and enums must be declared by hand (the front end emits none).
///
/// All registration is keyed by `symbol_hash(name)`, so a Core term built with
/// `Atom::Global(symbol_hash("Fs"))` or `Type::Nominal { def: symbol_hash("Fs") }`
/// resolves against the matching declaration.
#[derive(Debug, Clone, Default)]
pub struct WorldBuilder {
    world: World,
}

impl WorldBuilder {
    /// A fresh builder.
    pub fn new() -> Self {
        WorldBuilder::default()
    }

    /// Register a top-level value signature under `symbol_hash(name)`.
    pub fn global(mut self, name: &str, ty: Type) -> Self {
        self.world.globals.insert(symbol_hash(name), ty);
        self
    }

    /// Register a capability `name` with the given operations, under
    /// `symbol_hash(name)`.
    pub fn cap(mut self, name: &str, ops: Vec<OpSig>) -> Self {
        self.world.caps.insert(
            symbol_hash(name),
            CapDecl {
                name: name.to_string(),
                ops,
            },
        );
        self
    }

    /// Register an error `name` carrying the given payload types.
    pub fn error(mut self, name: &str, payload: Vec<Type>) -> Self {
        self.world.errors.insert(
            symbol_hash(name),
            ErrorDecl {
                name: name.to_string(),
                payload,
            },
        );
        self
    }

    /// Register an enum `name` with variants in tag order: `(variant_name,
    /// field_types)`.
    pub fn enum_decl(mut self, name: &str, variants: Vec<(&str, Vec<Type>)>) -> Self {
        let variants = variants
            .into_iter()
            .map(|(n, fields)| VariantDecl {
                name: n.to_string(),
                fields,
            })
            .collect();
        self.world.enums.insert(
            symbol_hash(name),
            EnumDecl {
                name: name.to_string(),
                variants,
            },
        );
        self
    }

    /// Register a struct `name` with the given field types.
    pub fn struct_decl(mut self, name: &str, fields: Vec<Type>, linear: bool) -> Self {
        self.world.structs.insert(
            symbol_hash(name),
            StructDecl {
                name: name.to_string(),
                fields,
                linear,
            },
        );
        self
    }

    /// Finish, yielding the assembled [`World`].
    pub fn build(self) -> World {
        self.world
    }
}

/// Peel a lowered struct type into `(field_types, is_linear)`. M1 lowers a
/// struct to `Tuple(field_tys)`, wrapped in `Linear(_)` when declared `linear`
/// (see `marv_core::lower::lower_struct`).
fn struct_fields(ty: &Type) -> (Vec<Type>, bool) {
    match ty {
        Type::Linear(inner) => {
            let (fields, _) = struct_fields(inner);
            (fields, true)
        }
        Type::Tuple(fields) => (fields.clone(), false),
        // A single-field or otherwise non-tuple struct body: treat as one field.
        other => (vec![other.clone()], false),
    }
}
