//! # marv-codegen-wasm — WebAssembly backend (milestone M5)
//!
//! Compiles the canonical **Core IR** (`marv-core`) to a WebAssembly module with
//! `wasm-encoder`. It is the third backend behind the interpreter (`marv-interp`,
//! the oracle) and the Cranelift backend (`marv-codegen-cl`); all three consume
//! the same Core, so "they agree" is a checkable statement (`spec/01` §9).
//!
//! ## Capabilities are host imports (the sandbox)
//!
//! This is the property `spec/01` §9 hangs the web story on: a `perform` of a
//! capability lowers to a **call to an imported function**, one import per
//! `(capability, operation)`. A *pure* module performs nothing, so it **imports
//! nothing** — there is no slot through which a host could hand it authority. A
//! module that wants the network imports `("Net", "op0")`; the host (a wasmtime
//! embedding, or a browser page) decides whether to satisfy that import. Withhold
//! it and the module cannot be instantiated, let alone reach the network. The
//! import list is the capability manifest, statically inspectable
//! (`WebAssembly.Module.imports`).
//!
//! ## What it compiles
//!
//! The same subset the Cranelift backend handles — arithmetic and comparison
//! [`PrimOp`]s, `if`/`else` (a two-arm `bool` [`Core::Match`]), `let`, curried
//! cross-function calls and recursion, boxed aggregates/enums over a linear-memory
//! heap (MARV-9), and arrays with `len`/index/store (MARV-30; an array boxes to
//! `[len, e0, …]` with the element count in the header word) — plus
//! [`Core::Perform`] lowered to a host-import call. Every scalar is an `i64`,
//! matching the oracle's semantics, so the differential test is meaningful.
//! Constructs with no surface form yet (first-class closures, floats, string-typed
//! capability operands) return [`WasmError::Unsupported`].
//!
//! ## Currying without heap closures
//!
//! Application is curried and ANF-sequenced. The translator accumulates argument
//! operands across the `App` spine in a [`Slot::Partial`] (resolved to absolute
//! locals/constants at collection time, so de Bruijn depth never bites) and
//! lowers a saturated call to a single direct WebAssembly `call`.

use std::collections::HashMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, EntityType, ExportKind, ExportSection, Function,
    FunctionSection, GlobalSection, GlobalType, ImportSection, Instruction, MemArg, MemorySection,
    MemoryType, Module, TypeSection, ValType,
};

use marv_core::ir::*;
use marv_core::symbol_hash;
use marv_types::{layout, World};

/// Bytes per aggregate slot — every field (and the tag) is one `i64` machine
/// word (MARV-9).
const SLOT: u64 = 8;

/// Alignment exponent for an 8-byte (`i64`) access (`log2(8)`).
const ALIGN8: u32 = 3;

/// The linear-memory index (this backend declares exactly one memory).
const MEM: u32 = 0;

/// The global index of the bump-allocation pointer (this backend declares
/// exactly one global).
const BUMP: u32 = 0;

/// Where the bump allocator starts handing out memory. The low bytes are left
/// unused so a zero pointer never names a live aggregate.
const HEAP_START: i64 = 1024;

/// WebAssembly page size, in bytes.
const PAGE_SIZE: i64 = 64 * 1024;

/// Initial linear-memory size, in 64 KiB pages. Allocation sites grow memory on
/// demand and scalar-carried loops rewind the heap pointer to reclaim boxes whose
/// lifetime is bounded by one iteration.
const MEM_PAGES: u64 = 1;

/// An 8-byte aligned access at `offset` into the single linear memory.
fn memarg(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: ALIGN8,
        memory_index: MEM,
    }
}

/// A backend failure — never a type error (the M2 checker has already run).
#[derive(Debug, Clone)]
pub enum WasmError {
    /// A Core construct this backend does not lower yet.
    Unsupported(String),
    /// A referenced global is not a known function in this program.
    UnknownGlobal(Hash),
    /// A `perform` whose capability could not be resolved to a declared cap
    /// (e.g. a capability narrowed through a `let`, whose type is not tracked).
    UnresolvedCapability,
    /// The requested entry point does not exist.
    NoSuchEntry(String),
    /// The emitted module failed validation (carries the validator's message).
    Invalid(String),
}

impl std::fmt::Display for WasmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WasmError::Unsupported(d) => write!(f, "wasm backend: unsupported: {d}"),
            WasmError::UnknownGlobal(h) => write!(f, "wasm backend: unknown global {}", h.to_b3()),
            WasmError::UnresolvedCapability => write!(
                f,
                "wasm backend: could not resolve the capability of a `perform` to a declared cap"
            ),
            WasmError::NoSuchEntry(e) => write!(f, "wasm backend: no entry point `{e}`"),
            WasmError::Invalid(m) => write!(f, "wasm backend: emitted an invalid module: {m}"),
        }
    }
}

impl std::error::Error for WasmError {}

/// One capability operation the module imports from its host — the unit of
/// granted authority (`spec/01` §9). The import's `(module, name)` pair is
/// `(cap, "op<op>")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapImport {
    pub cap: String,
    pub op: u32,
    /// Number of (non-unit, scalar) operands the operation takes.
    pub params: usize,
    /// Whether the operation returns a value (`false` for a unit result).
    pub returns_value: bool,
}

/// An exported function and the number of `i64` arguments it takes (its non-unit
/// parameters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportInfo {
    pub name: String,
    pub arity: usize,
}

/// A compiled WebAssembly module plus its capability manifest.
#[derive(Debug, Clone)]
pub struct WasmArtifact {
    /// The encoded `.wasm` bytes (a core module).
    pub bytes: Vec<u8>,
    /// The capabilities the module requires from its host, in import order.
    pub imports: Vec<CapImport>,
    /// The functions the module exports.
    pub exports: Vec<ExportInfo>,
}

/// Per-function compile metadata: curried arity and which parameters carry no
/// wasm ABI slot. A parameter has no slot when it is `Unit` (zero-sized) or a
/// **capability** — a capability has no runtime representation here; the
/// authority it conveys is the host *import* a `perform` calls, not a value
/// threaded through the ABI (`spec/01` §9).
struct FnMeta {
    arity: usize,
    no_slot: Vec<bool>,
}

impl FnMeta {
    fn abi_param_count(&self) -> usize {
        self.no_slot.iter().filter(|s| !**s).count()
    }
}

/// Whether a parameter of this type carries no wasm ABI slot (unit or capability).
fn is_no_slot(t: &Type, world: &World) -> bool {
    match t {
        Type::Unit => true,
        Type::Nominal { def, .. } => world.is_cap(def),
        _ => false,
    }
}

/// Codegen options (the debug/release distinction, `spec/01` §7).
#[derive(Debug, Clone)]
pub struct Options {
    /// Emit the Tier-1 bounds check on every runtime element read/store
    /// (MARV-34). On by default (debug builds); release builds switch it off,
    /// keeping their in-bounds codegen byte-identical to the pre-MARV-34
    /// output. The abort is a wasm `unreachable` trap — a host-imported abort
    /// would put an import in *pure* modules, breaking the "a pure module
    /// imports nothing" sandbox manifest, so the trap carries no message.
    pub bounds_checks: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            bounds_checks: true,
        }
    }
}

/// Compile a set of definitions to a WebAssembly module.
///
/// Functions are keyed (and called) by `symbol_hash("<module>.<name>")` and
/// exported under their qualified names. Capability `perform`s become host
/// imports; a module that performs nothing imports nothing.
pub fn compile(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
) -> Result<WasmArtifact, WasmError> {
    compile_with(module_path, defs, world, &Options::default())
}

/// [`compile`] with explicit [`Options`] — the entry release builds use to omit
/// the Tier-1 bounds checks (MARV-34). Compiles the **whole module**; audit
/// flows and the differential corpus want every definition.
pub fn compile_with(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
    opts: &Options,
) -> Result<WasmArtifact, WasmError> {
    compile_inner(module_path, defs, world, opts, None)
}

/// [`compile_with`], but compile (and export) only the definitions **reachable
/// from `entry`** (MARV-8): a sibling function using a construct this backend
/// does not lower yet no longer blocks the build, as long as the entry never
/// references it. `entry` resolves the way the other backends resolve one
/// (explicit name, else `main`, else the sole function); when it resolves to
/// nothing the whole module is compiled.
pub fn compile_reachable(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
    opts: &Options,
    entry: &str,
) -> Result<WasmArtifact, WasmError> {
    compile_inner(module_path, defs, world, opts, Some(entry))
}

/// Compile definitions whose references have already been rewritten to
/// content dag hashes by `marv-store`.
pub fn compile_hashed_reachable(
    defs: &[(Hash, String, Def)],
    aliases: &[(String, Hash)],
    world: &World,
    opts: &Options,
    entry: &str,
) -> Result<WasmArtifact, WasmError> {
    compile_hashed_inner(defs, aliases, world, opts, Some(entry))
}

fn compile_inner(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
    opts: &Options,
    entry: Option<&str>,
) -> Result<WasmArtifact, WasmError> {
    // With an entry to prune from, only its transitive dependency closure is
    // compiled and exported (MARV-8).
    let mask = entry.map(|e| marv_core::reach::reachable_mask(module_path, defs, e));

    // ---- gather functions, in definition order --------------------------
    // Skip generic templates (signatures mentioning a `Type::Var`): they have no
    // concrete ABI and are never called directly — only their monomorphizations
    // (`max@i64`, …) are. Their bodies reference interface methods (`cmp`) that
    // resolve only in a specialized, impl-dispatched context (`spec/01` §§3.3–3.4).
    let fns: Vec<(Hash, &str, &Def)> = defs
        .iter()
        .enumerate()
        .filter(|(idx, (_, d))| {
            d.kind == DefKind::Fn && !d.ty.is_polymorphic() && mask.as_ref().is_none_or(|m| m[*idx])
        })
        .map(|(_, (name, d))| (symbol_hash(&qualify(module_path, name)), name.as_str(), d))
        .collect();

    let mut metas: HashMap<Hash, FnMeta> = HashMap::new();
    for (h, _, def) in &fns {
        let param_tys = peel_param_types(&def.ty);
        let no_slot = param_tys.iter().map(|t| is_no_slot(t, world)).collect();
        metas.insert(
            *h,
            FnMeta {
                arity: param_tys.len(),
                no_slot,
            },
        );
    }

    // ---- collect capability imports (function index space 0..k) ---------
    let imports = collect_imports(&fns, world)?;
    let mut import_index: HashMap<(String, u32), u32> = HashMap::new();
    for (i, imp) in imports.iter().enumerate() {
        import_index.insert((imp.cap.clone(), imp.op), i as u32);
    }
    let import_count = imports.len() as u32;

    // ---- assign defined-function indices (k..) --------------------------
    let mut fn_index: HashMap<Hash, u32> = HashMap::new();
    for (pos, (h, _, _)) in fns.iter().enumerate() {
        fn_index.insert(*h, import_count + pos as u32);
    }

    // ---- encode sections ------------------------------------------------
    let mut types = TypeSection::new();
    let mut type_count: u32 = 0;
    let mut import_sec = ImportSection::new();
    for imp in &imports {
        let params = vec![ValType::I64; imp.params];
        let results: Vec<ValType> = if imp.returns_value {
            vec![ValType::I64]
        } else {
            vec![]
        };
        types.ty().function(params, results);
        import_sec.import(
            &imp.cap,
            &format!("op{}", imp.op),
            EntityType::Function(type_count),
        );
        type_count += 1;
    }

    let mut func_sec = FunctionSection::new();
    let mut export_sec = ExportSection::new();
    let mut exports = Vec::new();
    for (h, name, _def) in &fns {
        let meta = &metas[h];
        let params = vec![ValType::I64; meta.abi_param_count()];
        types.ty().function(params, vec![ValType::I64]);
        func_sec.function(type_count);
        type_count += 1;

        let qualified = qualify(module_path, name);
        export_sec.export(&qualified, ExportKind::Func, fn_index[h]);
        exports.push(ExportInfo {
            name: qualified,
            arity: meta.abi_param_count(),
        });
    }

    let mut code_sec = CodeSection::new();
    for (h, _, def) in &fns {
        let func = compile_fn(*h, def, world, &metas, &fn_index, &import_index, opts)?;
        code_sec.function(&func);
    }

    // The aggregate heap (MARV-9): one linear memory plus a mutable bump pointer
    // the boxing sites advance. Both are module-internal — a *pure* module still
    // imports nothing, so the capability manifest is unchanged.
    let mut mem_sec = MemorySection::new();
    mem_sec.memory(MemoryType {
        minimum: MEM_PAGES,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    let mut global_sec = GlobalSection::new();
    global_sec.global(
        GlobalType {
            val_type: ValType::I64,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i64_const(HEAP_START),
    );
    // Export the memory so a host/embedding can inspect the heap (harmless for a
    // pure module; the import manifest — the sandbox boundary — is untouched).
    export_sec.export("memory", ExportKind::Memory, MEM);

    // Section order is fixed by the spec: type, import, function, memory, global,
    // export, code.
    let mut module = Module::new();
    module
        .section(&types)
        .section(&import_sec)
        .section(&func_sec)
        .section(&mem_sec)
        .section(&global_sec)
        .section(&export_sec)
        .section(&code_sec);
    let bytes = module.finish();

    // Validate before handing the bytes back, so a backend bug surfaces here
    // rather than as an opaque trap in wasmtime / the browser.
    wasmparser::validate(&bytes).map_err(|e| WasmError::Invalid(e.to_string()))?;

    Ok(WasmArtifact {
        bytes,
        imports,
        exports,
    })
}

fn compile_hashed_inner(
    defs: &[(Hash, String, Def)],
    aliases: &[(String, Hash)],
    world: &World,
    opts: &Options,
    entry: Option<&str>,
) -> Result<WasmArtifact, WasmError> {
    let mask = entry.map(|e| hashed_reachable_mask(defs, aliases, e));

    let fns: Vec<(Hash, &str, &Def)> = defs
        .iter()
        .enumerate()
        .filter(|(idx, (_, _, d))| {
            d.kind == DefKind::Fn && !d.ty.is_polymorphic() && mask.as_ref().is_none_or(|m| m[*idx])
        })
        .map(|(_, (h, name, d))| (*h, name.as_str(), d))
        .collect();

    let mut metas: HashMap<Hash, FnMeta> = HashMap::new();
    for (h, _, def) in &fns {
        let param_tys = peel_param_types(&def.ty);
        let no_slot = param_tys.iter().map(|t| is_no_slot(t, world)).collect();
        metas.insert(
            *h,
            FnMeta {
                arity: param_tys.len(),
                no_slot,
            },
        );
    }

    let imports = collect_imports(&fns, world)?;
    let mut import_index: HashMap<(String, u32), u32> = HashMap::new();
    for (i, imp) in imports.iter().enumerate() {
        import_index.insert((imp.cap.clone(), imp.op), i as u32);
    }
    let import_count = imports.len() as u32;

    let mut fn_index: HashMap<Hash, u32> = HashMap::new();
    for (pos, (h, _, _)) in fns.iter().enumerate() {
        fn_index.insert(*h, import_count + pos as u32);
    }

    let mut types = TypeSection::new();
    let mut type_count: u32 = 0;
    let mut import_sec = ImportSection::new();
    for imp in &imports {
        let params = vec![ValType::I64; imp.params];
        let results: Vec<ValType> = if imp.returns_value {
            vec![ValType::I64]
        } else {
            vec![]
        };
        types.ty().function(params, results);
        import_sec.import(
            &imp.cap,
            &format!("op{}", imp.op),
            EntityType::Function(type_count),
        );
        type_count += 1;
    }

    let mut func_sec = FunctionSection::new();
    let mut export_sec = ExportSection::new();
    let mut exports = Vec::new();
    for (h, name, _def) in &fns {
        let meta = &metas[h];
        let params = vec![ValType::I64; meta.abi_param_count()];
        types.ty().function(params, vec![ValType::I64]);
        func_sec.function(type_count);
        type_count += 1;

        let export_name = aliases
            .iter()
            .find_map(|(alias, ah)| (ah == h).then_some(alias.as_str()))
            .unwrap_or(name);
        export_sec.export(export_name, ExportKind::Func, fn_index[h]);
        exports.push(ExportInfo {
            name: export_name.to_string(),
            arity: meta.abi_param_count(),
        });
    }

    let mut code_sec = CodeSection::new();
    for (h, _, def) in &fns {
        let func = compile_fn(*h, def, world, &metas, &fn_index, &import_index, opts)?;
        code_sec.function(&func);
    }

    let mut mem_sec = MemorySection::new();
    mem_sec.memory(MemoryType {
        minimum: MEM_PAGES,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    let mut global_sec = GlobalSection::new();
    global_sec.global(
        GlobalType {
            val_type: ValType::I64,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i64_const(HEAP_START),
    );

    let mut module = Module::new();
    module.section(&types);
    if !imports.is_empty() {
        module.section(&import_sec);
    }
    module.section(&func_sec);
    module.section(&mem_sec);
    module.section(&global_sec);
    module.section(&export_sec);
    module.section(&code_sec);
    let bytes = module.finish();
    wasmparser::Validator::new()
        .validate_all(&bytes)
        .map_err(|e| WasmError::Invalid(e.to_string()))?;

    Ok(WasmArtifact {
        bytes,
        imports,
        exports,
    })
}

fn hashed_reachable_mask(
    defs: &[(Hash, String, Def)],
    aliases: &[(String, Hash)],
    entry: &str,
) -> Vec<bool> {
    let n = defs.len();
    let Some(start) = resolve_hashed_entry(defs, aliases, entry) else {
        return vec![true; n];
    };
    let idx_by_hash: HashMap<Hash, usize> = defs
        .iter()
        .enumerate()
        .map(|(idx, (h, _, _))| (*h, idx))
        .collect();
    let Some(&start_idx) = idx_by_hash.get(&start) else {
        return vec![true; n];
    };
    let mut mask = vec![false; n];
    mask[start_idx] = true;
    let mut queue = vec![start_idx];
    while let Some(i) = queue.pop() {
        let mut syms = Vec::new();
        marv_core::reach::collect_global_syms(&defs[i].2, &mut syms);
        for s in syms {
            if let Some(&j) = idx_by_hash.get(&s) {
                if !mask[j] {
                    mask[j] = true;
                    queue.push(j);
                }
            }
        }
    }
    mask
}

fn resolve_hashed_entry(
    defs: &[(Hash, String, Def)],
    aliases: &[(String, Hash)],
    entry: &str,
) -> Option<Hash> {
    let concrete_fn = |def: &Def| def.kind == DefKind::Fn && !def.ty.is_polymorphic();
    if !entry.is_empty() {
        return aliases
            .iter()
            .find_map(|(name, h)| (name == entry).then_some(*h))
            .or_else(|| {
                defs.iter()
                    .find_map(|(h, name, def)| (name == entry && concrete_fn(def)).then_some(*h))
            });
    }
    if let Some((_, h)) = aliases.iter().find(|(name, _)| name == "main") {
        return Some(*h);
    }
    if let Some((h, _, _)) = defs
        .iter()
        .find(|(_, name, def)| name == "main" && concrete_fn(def))
    {
        return Some(*h);
    }
    let mut fns = defs
        .iter()
        .filter(|(_, _, def)| concrete_fn(def))
        .map(|(h, _, _)| *h);
    match (fns.next(), fns.next()) {
        (Some(h), None) => Some(h),
        _ => None,
    }
}

// ---- capability-import collection --------------------------------------

/// Walk every function body, resolving each `perform`'s capability and
/// operation to a host import. Deduplicated, in first-seen order.
fn collect_imports(fns: &[(Hash, &str, &Def)], world: &World) -> Result<Vec<CapImport>, WasmError> {
    let mut seen: Vec<(String, u32)> = Vec::new();
    let mut out: Vec<CapImport> = Vec::new();
    for (_, _, def) in fns {
        if let Some(body) = &def.body {
            let mut tys: Vec<Option<Type>> = Vec::new();
            walk_caps(body, &mut tys, world, &mut |cap_def, op| {
                let key = (world.cap_name(&cap_def), op);
                if seen.contains(&key) {
                    return Ok(());
                }
                let sig = world
                    .cap(&cap_def)
                    .and_then(|c| c.ops.get(op as usize))
                    .ok_or(WasmError::UnresolvedCapability)?;
                let mut params = 0usize;
                for p in &sig.params {
                    match p {
                        Type::Int(_) | Type::Bool | Type::Str => params += 1,
                        Type::Unit => {}
                        _ => {
                            return Err(WasmError::Unsupported(format!(
                                "capability operand type `{}` (only i64/bool/str/unit operands \
                                 are supported in wasm today)",
                                show_type(p)
                            )))
                        }
                    }
                }
                let returns_value = match &sig.ret {
                    Type::Unit => false,
                    Type::Int(_) | Type::Bool => true,
                    other => {
                        return Err(WasmError::Unsupported(format!(
                            "capability result type `{}`",
                            show_type(other)
                        )))
                    }
                };
                seen.push(key.clone());
                out.push(CapImport {
                    cap: key.0,
                    op: key.1,
                    params,
                    returns_value,
                });
                Ok(())
            })?;
        }
    }
    Ok(out)
}

/// Recurse through a Core term tracking binder *types* (only parameters carry a
/// known type — enough to resolve a capability `perform`d on a parameter), and
/// invoke `f(cap_def_hash, op)` at every `perform`.
fn walk_caps(
    c: &Core,
    tys: &mut Vec<Option<Type>>,
    world: &World,
    f: &mut dyn FnMut(Hash, u32) -> Result<(), WasmError>,
) -> Result<(), WasmError> {
    match c {
        Core::Lam { param, body, .. } => {
            tys.push(Some(param.clone()));
            walk_caps(body, tys, world, f)?;
            tys.pop();
        }
        Core::Let { value, body } => {
            walk_caps(value, tys, world, f)?;
            tys.push(None);
            walk_caps(body, tys, world, f)?;
            tys.pop();
        }
        Core::Match { branches, .. } => {
            for br in branches {
                for _ in 0..br.binds {
                    tys.push(None);
                }
                walk_caps(&br.body, tys, world, f)?;
                for _ in 0..br.binds {
                    tys.pop();
                }
            }
        }
        Core::Loop {
            state, cond, body, ..
        } => {
            // The carried vars are the innermost binders inside `cond`/`body`;
            // push a placeholder type per carried slot so de Bruijn cap resolution
            // inside the loop stays aligned. (`state` atoms carry no `perform`.)
            for _ in state {
                tys.push(None);
            }
            walk_caps(cond, tys, world, f)?;
            walk_caps(body, tys, world, f)?;
            for _ in state {
                tys.pop();
            }
        }
        Core::Perform { cap, op, .. } => {
            let def = resolve_cap_def(cap, tys, world).ok_or(WasmError::UnresolvedCapability)?;
            f(def, op.0)?;
        }
        // Atom / App / Ctor / Proj / Prim / Cast / Ref / Raise carry only atomic
        // children (no nested `perform`).
        _ => {}
    }
    Ok(())
}

/// The capability declaration hash a capability atom denotes, if it is a
/// parameter of known capability type.
fn resolve_cap_def(cap: &Atom, tys: &[Option<Type>], world: &World) -> Option<Hash> {
    let Atom::Var(idx) = cap else { return None };
    let d = tys.len();
    let i = (*idx as usize) + 1;
    if i > d {
        return None;
    }
    match &tys[d - i] {
        Some(Type::Nominal { def, .. }) if world.is_cap(def) => Some(*def),
        _ => None,
    }
}

// ---- per-function body translation -------------------------------------

/// What a translated Core term left behind.
enum Out {
    /// One `i64` was pushed onto the wasm operand stack.
    Stack,
    /// A unit value: nothing pushed.
    Unit,
    /// The current path already emitted a function return.
    Returned,
    /// A partially-applied function (compile-time only): nothing pushed.
    Partial { func: Hash, args: Vec<ArgVal> },
    /// A compile-time aggregate (a product/struct `Ctor`, an enum-variant `Ctor`
    /// with tag `tag`, or a `Loop`'s final carried state): its fields live in
    /// locals; nothing is pushed. A `Proj` selects one; it is **boxed** into
    /// linear memory lazily — only when it must become a single word (MARV-9).
    Tuple { tag: u32, fields: Vec<Slot> },
}

enum LoopBody {
    Continue,
    Returned,
}

/// An argument resolved to something re-emittable at the saturating call site,
/// independent of de Bruijn depth.
#[derive(Clone)]
enum ArgVal {
    Local(u32),
    Const(i64),
    Unit,
    /// An aggregate argument: boxed into linear memory at the call site, passing
    /// the pointer (MARV-9). Fields are themselves resolved arguments.
    Boxed {
        tag: u32,
        fields: Vec<ArgVal>,
    },
}

/// A binding in scope: an absolute local, a unit, a pending partial call, or a
/// compile-time aggregate whose fields are themselves slots.
#[derive(Clone)]
enum Slot {
    Local(u32),
    Unit,
    Partial { func: Hash, args: Vec<ArgVal> },
    Tuple { tag: u32, fields: Vec<Slot> },
}

#[allow(clippy::too_many_arguments)]
fn compile_fn(
    h: Hash,
    def: &Def,
    world: &World,
    metas: &HashMap<Hash, FnMeta>,
    fn_index: &HashMap<Hash, u32>,
    import_index: &HashMap<(String, u32), u32>,
    opts: &Options,
) -> Result<Function, WasmError> {
    let meta = &metas[&h];
    let body = def
        .body
        .as_ref()
        .ok_or_else(|| WasmError::Unsupported("function without a body".into()))?;
    let inner = peel_lams(body);

    // Bind parameters: non-unit params take wasm locals 0..n; unit params none.
    let n_params = meta.abi_param_count() as u32;
    let mut env: Vec<Slot> = Vec::with_capacity(meta.arity);
    let mut tys: Vec<Option<Type>> = Vec::with_capacity(meta.arity);
    let param_tys = peel_param_types(&def.ty);
    let mut local = 0u32;
    for pty in &param_tys {
        if is_no_slot(pty, world) {
            // Unit or capability: no runtime value, no wasm local.
            env.push(Slot::Unit);
        } else {
            env.push(Slot::Local(local));
            local += 1;
        }
        tys.push(Some(pty.clone()));
    }

    let mut t = Trans {
        world,
        metas,
        fn_index,
        import_index,
        bounds_checks: opts.bounds_checks,
        insts: Vec::new(),
        env,
        tys,
        next_local: n_params,
    };
    let out = t.eval(inner)?;
    if !matches!(out, Out::Returned) {
        t.coerce_to_word(out)?;
    }

    let extra = t.next_local - n_params;
    let mut func = Function::new(if extra > 0 {
        vec![(extra, ValType::I64)]
    } else {
        vec![]
    });
    for inst in &t.insts {
        func.instruction(inst);
    }
    func.instruction(&Instruction::End);
    Ok(func)
}

struct Trans<'a> {
    world: &'a World,
    metas: &'a HashMap<Hash, FnMeta>,
    fn_index: &'a HashMap<Hash, u32>,
    import_index: &'a HashMap<(String, u32), u32>,
    /// Whether to emit the Tier-1 bounds check on runtime element reads/stores
    /// (MARV-34): on in debug builds, off in release.
    bounds_checks: bool,
    insts: Vec<Instruction<'static>>,
    env: Vec<Slot>,
    tys: Vec<Option<Type>>,
    next_local: u32,
}

impl Trans<'_> {
    fn emit(&mut self, i: Instruction<'static>) {
        self.insts.push(i);
    }

    fn alloc_local(&mut self) -> u32 {
        let l = self.next_local;
        self.next_local += 1;
        l
    }

    fn const_local(&mut self, value: i64) -> u32 {
        let l = self.alloc_local();
        self.emit(Instruction::I64Const(value));
        self.emit(Instruction::LocalSet(l));
        l
    }

    fn eval(&mut self, c: &Core) -> Result<Out, WasmError> {
        match c {
            Core::Atom(a) => self.eval_atom(a),

            Core::Let { value, body } => {
                let out = self.eval(value)?;
                let slot = match out {
                    Out::Stack => {
                        let l = self.alloc_local();
                        self.emit(Instruction::LocalSet(l));
                        Slot::Local(l)
                    }
                    Out::Unit => Slot::Unit,
                    Out::Returned => return Ok(Out::Returned),
                    Out::Partial { func, args } => Slot::Partial { func, args },
                    Out::Tuple { tag, fields } => Slot::Tuple { tag, fields },
                };
                // Record the bound value's type so a `Match`/`Proj` over it knows
                // whether it is a scalar or a boxed aggregate (MARV-9).
                let world = self.world;
                let t = layout::type_of(world, value, &mut self.tys);
                self.env.push(slot);
                self.tys.push(t);
                let r = self.eval(body);
                self.env.pop();
                self.tys.pop();
                r
            }

            Core::App { func, arg } => self.eval_app(func, arg),

            Core::Prim { op, args } => self.eval_prim(*op, args),

            Core::Cast { value, to } => self.eval_cast(value, to),

            Core::Match {
                scrutinee,
                branches,
            } => self.eval_match(scrutinee, branches),

            Core::Perform { cap, op, args } => self.eval_perform(cap, *op, args),

            Core::Ctor { tag, fields, .. } => {
                // A product/enum value as a compile-time tuple of slots. Each
                // field is materialized into its own local so the bundle is stable
                // regardless of later stores (e.g. a loop's carried-state copy).
                let mut slots = Vec::with_capacity(fields.len());
                for a in fields {
                    slots.push(self.atom_to_local_slot(a)?);
                }
                Ok(Out::Tuple {
                    tag: *tag,
                    fields: slots,
                })
            }

            // An array literal: a compile-time tuple whose header word (the tag)
            // is the element **count**, so once boxed the block is `[len, e0, …]`.
            // `len` then reads word 0 and `index` loads `[i + 1]` (`eval_prim`).
            Core::Array { items, .. } => {
                let mut slots = Vec::with_capacity(items.len());
                for a in items {
                    slots.push(self.atom_to_local_slot(a)?);
                }
                Ok(Out::Tuple {
                    tag: items.len() as u32,
                    fields: slots,
                })
            }

            Core::IndexSet { base, index, value } => self.eval_index_set(base, index, value),
            Core::ListNew { capacity, .. } => self.eval_list_new(capacity),
            Core::ListPush { list, value, .. } => self.eval_list_push(list, value),
            Core::ListPop { list } => self.eval_list_pop(list),
            Core::ListSet { list, index, value } => self.eval_list_set(list, index, value),

            Core::Proj { base, idx } => self.eval_proj(base, *idx),

            Core::Loop {
                state, cond, body, ..
            } => self.eval_loop(state, cond, body),

            // A second-class reference has no runtime cell (mutable value
            // semantics, `spec/01` §4); it evaluates to its referent's value.
            Core::Ref { of, .. } => self.eval_atom(of),

            Core::Lam { .. } => Err(WasmError::Unsupported("first-class lambda".into())),
            Core::Raise { .. } => Err(WasmError::Unsupported("raise".into())),
            Core::Return { value } => {
                let out = self.eval_atom(value)?;
                self.coerce_to_word(out)?;
                self.emit(Instruction::Return);
                Ok(Out::Returned)
            }
        }
    }

    /// Lower a [`Core::Loop`] (`spec/02` §C `Loop`). The loop-carried state lives
    /// in mutable wasm locals (mutable value semantics has no cells in Core, so
    /// cross-iteration state is realized as locals here). The shape is
    /// `block { loop { <cond>; i64.eqz; br_if 1; <body → store carried>; br 0 } }`.
    /// Invariants are Tier-1/Tier-2 obligations checked elsewhere, not emitted.
    fn eval_loop(&mut self, state: &[Atom], cond: &Core, body: &Core) -> Result<Out, WasmError> {
        let k = state.len();
        // Resolve the initial carried values against the enclosing scope first,
        // then allocate the carried locals and store into them.
        let inits: Vec<ArgVal> = state
            .iter()
            .map(|a| self.resolve_arg(a))
            .collect::<Result<_, _>>()?;
        let carried: Vec<u32> = (0..k).map(|_| self.alloc_local()).collect();
        for (l, av) in carried.iter().zip(&inits) {
            self.push_argval(av);
            self.emit(Instruction::LocalSet(*l));
        }
        // The carried vars are the innermost env slots for `cond`/`body`; record
        // their types so a match over a carried aggregate pointer stays correct.
        let carried_tys: Vec<Option<Type>> = state
            .iter()
            .map(|a| layout::atom_type(self.world, a, &self.tys))
            .collect();
        let can_reset_heap = carried_tys
            .iter()
            .all(|t| t.as_ref().is_some_and(|t| !layout::is_boxed(self.world, t)));
        let heap_mark = if can_reset_heap {
            let l = self.alloc_local();
            self.emit(Instruction::GlobalGet(BUMP));
            self.emit(Instruction::LocalSet(l));
            Some(l)
        } else {
            None
        };
        for (l, t) in carried.iter().zip(carried_tys) {
            self.env.push(Slot::Local(*l));
            self.tys.push(t);
        }

        self.emit(Instruction::Block(BlockType::Empty));
        self.emit(Instruction::Loop(BlockType::Empty));
        // Test the condition; break out of the block when it is false.
        let c = self.eval(cond)?;
        self.coerce_to_word(c)?;
        self.emit(Instruction::I64Eqz);
        self.emit(Instruction::BrIf(1));
        // Body computes the next state directly into the carried locals — even
        // through an `if`/`match` branch join, where each arm writes its own
        // k-tuple into them (MARV-21), so the carried state stays in locals and is
        // never boxed (the alloc-free-loops property, MARV-9).
        if let LoopBody::Continue = self.eval_loop_body(body, &carried)? {
            if let Some(mark) = heap_mark {
                self.emit(Instruction::LocalGet(mark));
                self.emit(Instruction::GlobalSet(BUMP));
            }
            self.emit(Instruction::Br(0));
        }
        self.emit(Instruction::End); // loop
        self.emit(Instruction::End); // block
        if let Some(mark) = heap_mark {
            self.emit(Instruction::LocalGet(mark));
            self.emit(Instruction::GlobalSet(BUMP));
        }

        for _ in 0..k {
            self.env.pop();
            self.tys.pop();
        }
        // The loop evaluates to its final carried state.
        Ok(Out::Tuple {
            tag: 0,
            fields: carried.into_iter().map(Slot::Local).collect(),
        })
    }

    /// Emit a loop body so its next-state values land in the carried locals
    /// `dest`, *without* boxing the carried tuple (the alloc-free-loops property,
    /// MARV-9). A straight-line body's terminal is a tuple `Ctor` we copy into
    /// `dest`; a branch-join body (MARV-21) terminates in a `Match` whose every
    /// arm copies its own k-tuple into `dest`, so the carried state stays in
    /// locals. `Let` bindings in the spine are emitted in order, exactly as
    /// [`Self::eval`] would.
    fn eval_loop_body(&mut self, body: &Core, dest: &[u32]) -> Result<LoopBody, WasmError> {
        match body {
            Core::Let { value, body } => {
                let out = self.eval(value)?;
                let slot = match out {
                    Out::Stack => {
                        let l = self.alloc_local();
                        self.emit(Instruction::LocalSet(l));
                        Slot::Local(l)
                    }
                    Out::Unit => Slot::Unit,
                    Out::Returned => return Ok(LoopBody::Returned),
                    Out::Partial { func, args } => Slot::Partial { func, args },
                    Out::Tuple { tag, fields } => Slot::Tuple { tag, fields },
                };
                let t = layout::type_of(self.world, value, &mut self.tys);
                self.env.push(slot);
                self.tys.push(t);
                let r = self.eval_loop_body(body, dest);
                self.env.pop();
                self.tys.pop();
                r
            }
            Core::Match {
                scrutinee,
                branches,
            } => self.eval_loop_match(scrutinee, branches, dest),
            // Straight-line terminal: the next-state tuple, its fields already
            // materialized into locals; copy them into the carried locals.
            _ => {
                let out = self.eval(body)?;
                let next_slots = match out {
                    Out::Tuple { fields, .. } => fields,
                    Out::Returned => return Ok(LoopBody::Returned),
                    Out::Unit if dest.is_empty() => Vec::new(),
                    _ => {
                        return Err(WasmError::Unsupported(
                            "loop body did not produce its carried state".into(),
                        ))
                    }
                };
                self.copy_into_carried(dest, &next_slots)?;
                Ok(LoopBody::Continue)
            }
        }
    }

    /// Copy a loop body's next-state slots into the carried locals `dest`. Each
    /// field is a local (or a unit, which occupies no slot); a non-scalar field
    /// cannot be a carried word. The sequential copy cannot clobber a value a
    /// later field still needs: `Core::Ctor` already materialized *every* field —
    /// including a carried var that passes through unchanged — into its own fresh
    /// local (`atom_to_local_slot`) before this runs, so no `next_slots` entry
    /// aliases a `dest` carried local.
    fn copy_into_carried(&mut self, dest: &[u32], next_slots: &[Slot]) -> Result<(), WasmError> {
        for (l, s) in dest.iter().zip(next_slots) {
            match s {
                Slot::Local(src) => {
                    self.emit(Instruction::LocalGet(*src));
                    self.emit(Instruction::LocalSet(*l));
                }
                Slot::Unit => {}
                Slot::Partial { .. } | Slot::Tuple { .. } => {
                    return Err(WasmError::Unsupported(
                        "non-scalar loop-carried value".into(),
                    ))
                }
            }
        }
        Ok(())
    }

    /// A loop body whose terminal is a `Match` (MARV-21 branch join): dispatch like
    /// [`Self::eval_match`], but each arm writes its own k-tuple into the carried
    /// locals `dest` and the arm blocks carry no stack result (`BlockType::Empty`)
    /// — the carried state stays in locals, never boxed.
    fn eval_loop_match(
        &mut self,
        scrutinee: &Atom,
        branches: &[Branch],
        dest: &[u32],
    ) -> Result<LoopBody, WasmError> {
        let scrut_ty = layout::atom_type(self.world, scrutinee, &self.tys);
        let boxed = scrut_ty
            .as_ref()
            .map(|t| layout::is_boxed(self.world, t))
            .unwrap_or(false);
        if boxed {
            if branches.is_empty() {
                return Err(WasmError::Unsupported("match with no branches".into()));
            }
            let scrut_ty = scrut_ty.unwrap();
            // Materialize the scrutinee pointer and its tag into locals (the
            // pointer is reloaded in every arm to bind fields).
            let ptr = self.alloc_local();
            let av = self.resolve_arg(scrutinee)?;
            self.push_argval(&av);
            self.emit(Instruction::LocalSet(ptr));
            let tag = self.alloc_local();
            self.emit(Instruction::LocalGet(ptr));
            self.emit(Instruction::I32WrapI64);
            self.emit(Instruction::I64Load(memarg(0)));
            self.emit(Instruction::LocalSet(tag));
            if self.emit_loop_match_arm(branches, 0, ptr, tag, &scrut_ty, dest)? {
                Ok(LoopBody::Continue)
            } else {
                Ok(LoopBody::Returned)
            }
        } else {
            // Scalar (`bool`/`if`) path: two arms, no bound fields (`spec/02` §D).
            if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
                return Err(WasmError::Unsupported(
                    "loop branch join on a value whose layout could not be determined".into(),
                ));
            }
            let cond = self.resolve_arg(scrutinee)?;
            if matches!(cond, ArgVal::Unit) {
                return Err(WasmError::Unsupported("unit match scrutinee".into()));
            }
            self.push_argval(&cond);
            self.emit(Instruction::I64Const(0));
            self.emit(Instruction::I64Ne); // i32: 1 if nonzero (true)
            self.emit(Instruction::If(BlockType::Empty));
            // `then` = the true arm (branch tag 1).
            let then_continues = matches!(
                self.eval_loop_body(&branches[1].body, dest)?,
                LoopBody::Continue
            );
            self.emit(Instruction::Else);
            // `else` = the false arm (branch tag 0).
            let else_continues = matches!(
                self.eval_loop_body(&branches[0].body, dest)?,
                LoopBody::Continue
            );
            self.emit(Instruction::End);
            if then_continues || else_continues {
                Ok(LoopBody::Continue)
            } else {
                Ok(LoopBody::Returned)
            }
        }
    }

    /// Emit the dispatch chain for a branch-join loop's boxed `Match`, arm `t`
    /// onward (MARV-21): test the tag, run arm `t`'s body into `dest` in the
    /// `then`, recurse into the `else`. The final arm needs no test (an
    /// exhaustively-checked match always lands on a covered tag). Mirrors
    /// [`Self::emit_match_arm`] but the arms carry no stack result.
    fn emit_loop_match_arm(
        &mut self,
        branches: &[Branch],
        t: usize,
        ptr: u32,
        tag: u32,
        scrut_ty: &Type,
        dest: &[u32],
    ) -> Result<bool, WasmError> {
        if t + 1 == branches.len() {
            return self
                .emit_loop_bind_and_body(&branches[t], t, ptr, scrut_ty, dest)
                .map(|r| matches!(r, LoopBody::Continue));
        }
        self.emit(Instruction::LocalGet(tag));
        self.emit(Instruction::I64Const(t as i64));
        self.emit(Instruction::I64Eq);
        self.emit(Instruction::If(BlockType::Empty));
        let then_continues = matches!(
            self.emit_loop_bind_and_body(&branches[t], t, ptr, scrut_ty, dest)?,
            LoopBody::Continue
        );
        self.emit(Instruction::Else);
        let else_continues = self.emit_loop_match_arm(branches, t + 1, ptr, tag, scrut_ty, dest)?;
        self.emit(Instruction::End);
        Ok(then_continues || else_continues)
    }

    /// Bind variant `tag`'s fields from the payload (`[i + 1]`) into fresh locals,
    /// then emit the arm body into the carried locals `dest` (MARV-21). Mirrors
    /// [`Self::emit_bind_and_body`] but leaves no word on the stack.
    fn emit_loop_bind_and_body(
        &mut self,
        br: &Branch,
        tag: usize,
        ptr: u32,
        scrut_ty: &Type,
        dest: &[u32],
    ) -> Result<LoopBody, WasmError> {
        let field_tys =
            layout::variant_fields(self.world, scrut_ty, tag as u32).unwrap_or_default();
        let pushed = br.binds as usize;
        for i in 0..pushed {
            let l = self.alloc_local();
            self.emit(Instruction::LocalGet(ptr));
            self.emit(Instruction::I32WrapI64);
            self.emit(Instruction::I64Load(memarg((i as u64 + 1) * SLOT)));
            self.emit(Instruction::LocalSet(l));
            self.env.push(Slot::Local(l));
            self.tys.push(field_tys.get(i).cloned());
        }
        let result = self.eval_loop_body(&br.body, dest)?;
        for _ in 0..pushed {
            self.env.pop();
            self.tys.pop();
        }
        Ok(result)
    }

    /// Lower a runtime element store `s[i] = e` ([`Core::IndexSet`], MARV-33).
    /// A slice's length is only known at run time, so (unlike the array store,
    /// which unrolls over a static length) this reads the element count from the
    /// header, bump-allocates a fresh `[len, …]` block, copies every word into it
    /// with a `loop`, then overwrites element `i`. The result is the new block
    /// pointer, which the surface store rebinds the root to (mutable value
    /// semantics — the source block is untouched, `spec/01` §4).
    fn eval_index_set(
        &mut self,
        base: &Atom,
        index: &Atom,
        value: &Atom,
    ) -> Result<Out, WasmError> {
        // Each operand into a word local: the base pointer, the index, the value.
        let ptr = self.atom_to_word_local(base)?;
        let i = self.atom_to_word_local(index)?;
        let v = self.atom_to_word_local(value)?;

        // Tier-1 bounds check (debug builds, MARV-34): trap unless the
        // subscript falls inside `0..len`, before any allocation or copying.
        self.emit_bounds_check(ptr, i);

        // The header word holds the element count; the block is `len + 1` words.
        let total = self.alloc_local();
        self.emit(Instruction::LocalGet(ptr));
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::I64Load(memarg(0)));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(total));
        let newptr = self.bump_alloc_dyn(total);

        // Copy loop: `for k in 0..total { new[k] = old[k] }` (header + elements).
        let k = self.alloc_local();
        self.emit(Instruction::I64Const(0));
        self.emit(Instruction::LocalSet(k));
        self.emit(Instruction::Block(BlockType::Empty));
        self.emit(Instruction::Loop(BlockType::Empty));
        // Break out of the block once `k >= total`.
        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::LocalGet(total));
        self.emit(Instruction::I64LtU);
        self.emit(Instruction::I32Eqz);
        self.emit(Instruction::BrIf(1));
        // new[k] = old[k]: push dst address, then the source word, then store.
        self.emit(Instruction::LocalGet(newptr));
        self.push_word_offset(k);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::LocalGet(ptr));
        self.push_word_offset(k);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::I64Load(memarg(0)));
        self.emit(Instruction::I64Store(memarg(0)));
        // k += 1
        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(k));
        self.emit(Instruction::Br(0));
        self.emit(Instruction::End); // loop
        self.emit(Instruction::End); // block

        // Overwrite the one element at `[i + 1]` with the new value.
        self.emit(Instruction::LocalGet(newptr));
        self.emit(Instruction::LocalGet(i));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I64Const(SLOT as i64));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::LocalGet(v));
        self.emit(Instruction::I64Store(memarg(0)));

        // The store's value is the fresh block pointer.
        self.emit(Instruction::LocalGet(newptr));
        Ok(Out::Stack)
    }

    fn eval_list_new(&mut self, capacity: &Atom) -> Result<Out, WasmError> {
        let cap = self.atom_to_word_local(capacity)?;
        let total = self.alloc_local();
        self.emit(Instruction::LocalGet(cap));
        self.emit(Instruction::I64Const(2));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(total));
        let ptr = self.bump_alloc_dyn(total);
        self.store_word(ptr, 0, |t| {
            t.emit(Instruction::I64Const(0));
            Ok(())
        })?;
        self.store_word(ptr, SLOT, |t| {
            t.emit(Instruction::LocalGet(cap));
            Ok(())
        })?;
        self.emit(Instruction::LocalGet(ptr));
        Ok(Out::Stack)
    }

    fn eval_list_push(&mut self, list: &Atom, value: &Atom) -> Result<Out, WasmError> {
        let ptr = self.atom_to_word_local(list)?;
        let val = self.atom_to_word_local(value)?;
        let len = self.load_word_local(ptr, 0);
        let cap = self.load_word_local(ptr, SLOT);
        let result = self.alloc_local();

        self.emit(Instruction::LocalGet(len));
        self.emit(Instruction::LocalGet(cap));
        self.emit(Instruction::I64LtU);
        self.emit(Instruction::If(BlockType::Empty));

        self.store_word(ptr, 0, |t| {
            t.emit(Instruction::LocalGet(len));
            t.emit(Instruction::I64Const(1));
            t.emit(Instruction::I64Add);
            Ok(())
        })?;
        self.emit(Instruction::LocalGet(ptr));
        self.emit(Instruction::LocalGet(len));
        self.emit(Instruction::I64Const(2));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I64Const(SLOT as i64));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::LocalGet(val));
        self.emit(Instruction::I64Store(memarg(0)));
        self.emit(Instruction::LocalGet(ptr));
        self.emit(Instruction::LocalSet(result));

        self.emit(Instruction::Else);

        let new_cap = self.alloc_local();

        self.emit(Instruction::LocalGet(cap));
        self.emit(Instruction::I64Const(2));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::LocalTee(new_cap));
        self.emit(Instruction::I64Const(4));
        self.emit(Instruction::I64LtU);
        self.emit(Instruction::If(BlockType::Result(ValType::I64)));
        self.emit(Instruction::I64Const(4));
        self.emit(Instruction::Else);
        self.emit(Instruction::LocalGet(new_cap));
        self.emit(Instruction::End);
        self.emit(Instruction::LocalSet(new_cap));

        let total = self.alloc_local();
        self.emit(Instruction::LocalGet(new_cap));
        self.emit(Instruction::I64Const(2));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(total));
        let newptr = self.bump_alloc_dyn(total);

        let copy_total = self.alloc_local();
        self.emit(Instruction::LocalGet(len));
        self.emit(Instruction::I64Const(2));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(copy_total));
        self.copy_words(ptr, newptr, copy_total);

        self.store_word(newptr, 0, |t| {
            t.emit(Instruction::LocalGet(len));
            t.emit(Instruction::I64Const(1));
            t.emit(Instruction::I64Add);
            Ok(())
        })?;
        self.store_word(newptr, SLOT, |t| {
            t.emit(Instruction::LocalGet(new_cap));
            Ok(())
        })?;
        self.emit(Instruction::LocalGet(newptr));
        self.emit(Instruction::LocalGet(len));
        self.emit(Instruction::I64Const(2));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I64Const(SLOT as i64));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::LocalGet(val));
        self.emit(Instruction::I64Store(memarg(0)));

        self.emit(Instruction::LocalGet(newptr));
        self.emit(Instruction::LocalSet(result));
        self.emit(Instruction::End);

        self.emit(Instruction::LocalGet(result));
        Ok(Out::Stack)
    }

    fn eval_list_pop(&mut self, list: &Atom) -> Result<Out, WasmError> {
        let ptr = self.atom_to_word_local(list)?;
        let zero = self.alloc_local();
        self.emit(Instruction::I64Const(0));
        self.emit(Instruction::LocalSet(zero));
        self.emit_bounds_check(ptr, zero);
        let len = self.load_word_local(ptr, 0);
        self.store_word(ptr, 0, |t| {
            t.emit(Instruction::LocalGet(len));
            t.emit(Instruction::I64Const(1));
            t.emit(Instruction::I64Sub);
            Ok(())
        })?;
        self.emit(Instruction::LocalGet(ptr));
        Ok(Out::Stack)
    }

    fn eval_list_set(&mut self, list: &Atom, index: &Atom, value: &Atom) -> Result<Out, WasmError> {
        let ptr = self.atom_to_word_local(list)?;
        let idx = self.atom_to_word_local(index)?;
        let val = self.atom_to_word_local(value)?;
        self.emit_bounds_check(ptr, idx);
        self.emit(Instruction::LocalGet(ptr));
        self.emit(Instruction::LocalGet(idx));
        self.emit(Instruction::I64Const(2));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I64Const(SLOT as i64));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::LocalGet(val));
        self.emit(Instruction::I64Store(memarg(0)));
        self.emit(Instruction::LocalGet(ptr));
        Ok(Out::Stack)
    }

    fn copy_words(&mut self, src: u32, dst: u32, total: u32) {
        let k = self.alloc_local();
        self.emit(Instruction::I64Const(0));
        self.emit(Instruction::LocalSet(k));
        self.emit(Instruction::Block(BlockType::Empty));
        self.emit(Instruction::Loop(BlockType::Empty));
        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::LocalGet(total));
        self.emit(Instruction::I64LtU);
        self.emit(Instruction::I32Eqz);
        self.emit(Instruction::BrIf(1));
        self.emit(Instruction::LocalGet(dst));
        self.push_word_offset(k);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::LocalGet(src));
        self.push_word_offset(k);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::I64Load(memarg(0)));
        self.emit(Instruction::I64Store(memarg(0)));
        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(k));
        self.emit(Instruction::Br(0));
        self.emit(Instruction::End);
        self.emit(Instruction::End);
    }

    fn copy_word_range(&mut self, src: u32, src_start: u32, dst: u32, dst_start: u32, count: u32) {
        let k = self.alloc_local();
        self.emit(Instruction::I64Const(0));
        self.emit(Instruction::LocalSet(k));
        self.emit(Instruction::Block(BlockType::Empty));
        self.emit(Instruction::Loop(BlockType::Empty));
        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::LocalGet(count));
        self.emit(Instruction::I64LtU);
        self.emit(Instruction::I32Eqz);
        self.emit(Instruction::BrIf(1));

        self.emit(Instruction::LocalGet(dst));
        self.emit(Instruction::LocalGet(dst_start));
        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I64Const(SLOT as i64));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I32WrapI64);

        self.emit(Instruction::LocalGet(src));
        self.emit(Instruction::LocalGet(src_start));
        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I64Const(SLOT as i64));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::I64Load(memarg(0)));
        self.emit(Instruction::I64Store(memarg(0)));

        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(k));
        self.emit(Instruction::Br(0));
        self.emit(Instruction::End);
        self.emit(Instruction::End);
    }

    fn load_word_local(&mut self, base: u32, offset: u64) -> u32 {
        let out = self.alloc_local();
        self.emit(Instruction::LocalGet(base));
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::I64Load(memarg(offset)));
        self.emit(Instruction::LocalSet(out));
        out
    }

    /// Evaluate an atom and stash its word (boxing an aggregate to its pointer)
    /// into a fresh local, returning the local index. Used by [`Self::eval_index_set`]
    /// so each operand can be re-read across the copy loop.
    fn atom_to_word_local(&mut self, a: &Atom) -> Result<u32, WasmError> {
        let out = self.eval_atom(a)?;
        self.coerce_to_word(out)?;
        let l = self.alloc_local();
        self.emit(Instruction::LocalSet(l));
        Ok(l)
    }

    /// Push the byte offset `k * SLOT` of a word-count local `k` as an `i64`, then
    /// fold it onto the address already on the stack (`base + k * SLOT`).
    fn push_word_offset(&mut self, k: u32) {
        self.emit(Instruction::LocalGet(k));
        self.emit(Instruction::I64Const(SLOT as i64));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::I64Add);
    }

    fn atom_is_list(&self, a: &Atom) -> bool {
        layout::atom_type(self.world, a, &self.tys)
            .as_ref()
            .is_some_and(is_list_type)
    }

    fn atom_is_str(&self, a: &Atom) -> bool {
        layout::atom_type(self.world, a, &self.tys).as_ref() == Some(&Type::Str)
    }

    /// Bump-allocate a block of `total_words` words (a runtime count) and return
    /// the local holding its base address (MARV-33: a slice store's size is known
    /// only at run time). Memory grows before the bump pointer is committed.
    fn bump_alloc_dyn(&mut self, total_words: u32) -> u32 {
        let base = self.alloc_local();
        let end = self.alloc_local();
        self.emit(Instruction::GlobalGet(BUMP));
        self.emit(Instruction::LocalTee(base));
        self.emit(Instruction::LocalGet(total_words));
        self.emit(Instruction::I64Const(SLOT as i64));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(end));
        self.ensure_heap_fits(end);
        self.emit(Instruction::LocalGet(end));
        self.emit(Instruction::GlobalSet(BUMP));
        base
    }

    /// Ensure the linear memory contains byte offset `end`. If not, grow by the
    /// minimum number of pages and trap on grow failure.
    fn ensure_heap_fits(&mut self, end: u32) {
        let current_pages = self.alloc_local();
        let current_bytes = self.alloc_local();
        let needed_pages = self.alloc_local();

        self.emit(Instruction::MemorySize(MEM));
        self.emit(Instruction::I64ExtendI32U);
        self.emit(Instruction::LocalSet(current_pages));
        self.emit(Instruction::LocalGet(current_pages));
        self.emit(Instruction::I64Const(PAGE_SIZE));
        self.emit(Instruction::I64Mul);
        self.emit(Instruction::LocalSet(current_bytes));

        self.emit(Instruction::LocalGet(end));
        self.emit(Instruction::LocalGet(current_bytes));
        self.emit(Instruction::I64GtU);
        self.emit(Instruction::If(BlockType::Empty));

        self.emit(Instruction::LocalGet(end));
        self.emit(Instruction::I64Const(PAGE_SIZE - 1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::I64Const(PAGE_SIZE));
        self.emit(Instruction::I64DivU);
        self.emit(Instruction::LocalSet(needed_pages));

        self.emit(Instruction::LocalGet(needed_pages));
        self.emit(Instruction::LocalGet(current_pages));
        self.emit(Instruction::I64Sub);
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::MemoryGrow(MEM));
        self.emit(Instruction::I32Const(-1));
        self.emit(Instruction::I32Eq);
        self.emit(Instruction::If(BlockType::Empty));
        self.emit(Instruction::Unreachable);
        self.emit(Instruction::End);

        self.emit(Instruction::End);
    }

    /// Emit the Tier-1 debug bounds check around a runtime element access
    /// (`spec/01` §7, MARV-34), with the block pointer and the subscript in the
    /// locals `ptr`/`i`: load the length from the header word and trap
    /// (`unreachable`) unless `0 <= i < len`. One *unsigned* comparison covers
    /// both ends (a negative `i64` is a huge `u64`). The trap carries no
    /// message — an abort hook would be a host import, which would break the
    /// "a pure module imports nothing" sandbox manifest. No-op when
    /// `bounds_checks` is off (release builds), like the Cranelift twin —
    /// though the `Index` call site still gates externally, because its
    /// operand-stashing stack shuffle must be skipped along with the check.
    fn emit_bounds_check(&mut self, ptr: u32, i: u32) {
        if !self.bounds_checks {
            return;
        }
        self.emit(Instruction::LocalGet(i));
        self.emit(Instruction::LocalGet(ptr));
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::I64Load(memarg(0))); // len (the header word)
        self.emit(Instruction::I64GeU);
        self.emit(Instruction::If(BlockType::Empty));
        self.emit(Instruction::Unreachable);
        self.emit(Instruction::End);
    }

    /// Project field `idx`. A compile-time tuple selects the field directly; a
    /// boxed aggregate (a word that is a pointer) loads from `[idx + 1]` — word 0
    /// is the tag (MARV-9).
    fn eval_proj(&mut self, base: &Atom, idx: u32) -> Result<Out, WasmError> {
        match base {
            Atom::Var(i) => match self.slot(*i)? {
                Slot::Tuple { fields, .. } => {
                    let field = fields
                        .into_iter()
                        .nth(idx as usize)
                        .ok_or_else(|| WasmError::Unsupported("projection out of range".into()))?;
                    Ok(self.slot_to_out(field))
                }
                Slot::Local(l) => {
                    // A boxed-aggregate pointer in a local: load `[idx + 1]`.
                    self.emit(Instruction::LocalGet(l));
                    self.emit(Instruction::I32WrapI64);
                    self.emit(Instruction::I64Load(memarg((idx as u64 + 1) * SLOT)));
                    Ok(Out::Stack)
                }
                _ => Err(WasmError::Unsupported(
                    "projection of a non-aggregate".into(),
                )),
            },
            _ => Err(WasmError::Unsupported(
                "projection of a non-aggregate base".into(),
            )),
        }
    }

    /// Materialize an atom's value into a fresh local and return it as a slot —
    /// used to bundle `Ctor` fields so the aggregate is stable under later stores.
    fn atom_to_local_slot(&mut self, a: &Atom) -> Result<Slot, WasmError> {
        match self.eval_atom(a)? {
            Out::Stack => {
                let l = self.alloc_local();
                self.emit(Instruction::LocalSet(l));
                Ok(Slot::Local(l))
            }
            Out::Unit => Ok(Slot::Unit),
            Out::Returned => Err(WasmError::Unsupported(
                "early return used as a value".into(),
            )),
            Out::Partial { func, args } => Ok(Slot::Partial { func, args }),
            Out::Tuple { tag, fields } => Ok(Slot::Tuple { tag, fields }),
        }
    }

    /// Re-materialize a slot as an [`Out`] (the dual of the `Let` binding step).
    fn slot_to_out(&mut self, s: Slot) -> Out {
        match s {
            Slot::Local(l) => {
                self.emit(Instruction::LocalGet(l));
                Out::Stack
            }
            Slot::Unit => Out::Unit,
            Slot::Partial { func, args } => Out::Partial { func, args },
            Slot::Tuple { tag, fields } => Out::Tuple { tag, fields },
        }
    }

    fn eval_atom(&mut self, a: &Atom) -> Result<Out, WasmError> {
        match a {
            Atom::Lit(Literal::Unit) => Ok(Out::Unit),
            Atom::Lit(Literal::Int(n)) => {
                self.emit(Instruction::I64Const(*n));
                Ok(Out::Stack)
            }
            Atom::Lit(Literal::Bool(b)) => {
                self.emit(Instruction::I64Const(*b as i64));
                Ok(Out::Stack)
            }
            Atom::Lit(Literal::Float(_)) => Err(WasmError::Unsupported("float literal".into())),
            Atom::Lit(Literal::Str(s)) => {
                let fields = s
                    .chars()
                    .map(|c| Slot::Local(self.const_local(c as i64)))
                    .collect();
                Ok(Out::Tuple {
                    tag: s.chars().count() as u32,
                    fields,
                })
            }
            // A `char` is its Unicode code point as an `i64` — the same scalar
            // the interpreter and Cranelift compute (`spec/01` §3.1).
            Atom::Lit(Literal::Char(c)) => {
                self.emit(Instruction::I64Const(*c as i64));
                Ok(Out::Stack)
            }
            Atom::Var(idx) => Ok(self.slot_to_out(self.slot(*idx)?)),
            Atom::Global(h) => {
                if self.fn_index.contains_key(h) {
                    Ok(Out::Partial {
                        func: *h,
                        args: Vec::new(),
                    })
                } else {
                    Err(WasmError::UnknownGlobal(*h))
                }
            }
        }
    }

    fn slot(&self, idx: u32) -> Result<Slot, WasmError> {
        let d = self.env.len();
        let i = (idx as usize) + 1;
        if i > d {
            return Err(WasmError::Unsupported(format!(
                "de Bruijn index {idx} out of scope at depth {d}"
            )));
        }
        Ok(self.env[d - i].clone())
    }

    fn eval_app(&mut self, func: &Atom, arg: &Atom) -> Result<Out, WasmError> {
        let (h, mut args) = match func {
            Atom::Global(h) if self.fn_index.contains_key(h) => (*h, Vec::new()),
            Atom::Var(idx) => match self.slot(*idx)? {
                Slot::Partial { func, args } => (func, args),
                _ => {
                    return Err(WasmError::Unsupported(
                        "application of a non-function value".into(),
                    ))
                }
            },
            _ => {
                return Err(WasmError::Unsupported(
                    "application of a non-function".into(),
                ))
            }
        };
        args.push(self.resolve_arg(arg)?);
        let arity = self.metas[&h].arity;
        if args.len() < arity {
            return Ok(Out::Partial { func: h, args });
        }
        for av in &args {
            self.push_argval(av);
        }
        let fidx = self.fn_index[&h];
        self.emit(Instruction::Call(fidx));
        Ok(Out::Stack)
    }

    /// Resolve an argument atom to a depth-independent re-emittable value.
    fn resolve_arg(&self, a: &Atom) -> Result<ArgVal, WasmError> {
        match a {
            Atom::Lit(Literal::Int(n)) => Ok(ArgVal::Const(*n)),
            Atom::Lit(Literal::Bool(b)) => Ok(ArgVal::Const(*b as i64)),
            Atom::Lit(Literal::Char(c)) => Ok(ArgVal::Const(*c as i64)),
            Atom::Lit(Literal::Str(s)) => Ok(ArgVal::Boxed {
                tag: s.chars().count() as u32,
                fields: s.chars().map(|c| ArgVal::Const(c as i64)).collect(),
            }),
            Atom::Lit(Literal::Unit) => Ok(ArgVal::Unit),
            Atom::Var(idx) => self.slot_to_argval(&self.slot(*idx)?),
            Atom::Lit(_) => Err(WasmError::Unsupported("non-scalar literal argument".into())),
            Atom::Global(_) => Err(WasmError::Unsupported(
                "passing a function as a value argument".into(),
            )),
        }
    }

    /// Resolve a slot to a depth-independent re-emittable argument. An aggregate
    /// becomes a [`ArgVal::Boxed`] — boxed into linear memory at the point it is
    /// pushed, so the pointer is what crosses the call boundary (MARV-9).
    fn slot_to_argval(&self, s: &Slot) -> Result<ArgVal, WasmError> {
        match s {
            Slot::Local(l) => Ok(ArgVal::Local(*l)),
            Slot::Unit => Ok(ArgVal::Unit),
            Slot::Partial { .. } => Err(WasmError::Unsupported(
                "passing a partially-applied function as an argument".into(),
            )),
            Slot::Tuple { tag, fields } => {
                let fields = fields
                    .iter()
                    .map(|f| self.slot_to_argval(f))
                    .collect::<Result<_, _>>()?;
                Ok(ArgVal::Boxed { tag: *tag, fields })
            }
        }
    }

    /// Push a resolved argument (unit arguments occupy no slot; an aggregate is
    /// boxed into linear memory and its pointer pushed).
    fn push_argval(&mut self, av: &ArgVal) {
        match av {
            ArgVal::Local(l) => self.emit(Instruction::LocalGet(*l)),
            ArgVal::Const(n) => self.emit(Instruction::I64Const(*n)),
            ArgVal::Unit => {}
            ArgVal::Boxed { tag, fields } => self.box_argvals(*tag, fields),
        }
    }

    fn eval_prim(&mut self, op: PrimOp, args: &[Atom]) -> Result<Out, WasmError> {
        use PrimOp::*;
        if op == Add && args.first().is_some_and(|a| self.atom_is_str(a)) {
            return self.eval_string_concat(&args[0], &args[1]);
        }
        if op == Slice {
            return self.eval_string_slice(&args[0], &args[1], &args[2]);
        }
        if op == FromChars {
            return self.eval_string_from_chars(&args[1]);
        }
        let resolved: Vec<ArgVal> = args
            .iter()
            .map(|a| self.resolve_arg(a))
            .collect::<Result<_, _>>()?;
        for av in &resolved {
            if matches!(av, ArgVal::Unit) {
                return Err(WasmError::Unsupported("unit operand to a primitive".into()));
            }
            self.push_argval(av);
        }
        match op {
            Add => self.emit(Instruction::I64Add),
            Sub => self.emit(Instruction::I64Sub),
            Mul => self.emit(Instruction::I64Mul),
            Div => self.emit(Instruction::I64DivS),
            Rem => self.emit(Instruction::I64RemS),
            Eq => self.cmp(Instruction::I64Eq),
            Ne => self.cmp(Instruction::I64Ne),
            Lt => self.cmp(Instruction::I64LtS),
            Le => self.cmp(Instruction::I64LeS),
            Gt => self.cmp(Instruction::I64GtS),
            Ge => self.cmp(Instruction::I64GeS),
            And => self.emit(Instruction::I64And),
            Or => self.emit(Instruction::I64Or),
            // `not` on a 0/1 word flips the low bit.
            Not => {
                self.emit(Instruction::I64Const(1));
                self.emit(Instruction::I64Xor);
            }
            // `-x` — wasm has no integer-negate, so multiply by -1.
            Neg => {
                self.emit(Instruction::I64Const(-1));
                self.emit(Instruction::I64Mul);
            }
            // `len(a)` / `a[i]` over a boxed array (`[len, e0, …]`, MARV-30). The
            // operands were pushed above: for `len` the stack is `[ptr]`, for
            // `index` it is `[ptr, i]`. Boxing wrote the element count into the
            // header (word 0) and element `i` at word `i + 1`.
            Len => {
                self.emit(Instruction::I32WrapI64);
                self.emit(Instruction::I64Load(memarg(0)));
            }
            Index => {
                let header_words = if args.first().is_some_and(|a| self.atom_is_list(a)) {
                    2
                } else {
                    1
                };
                // Tier-1 bounds check (debug builds, MARV-34): stash the operands
                // ([ptr, i] on the stack) into locals, trap unless `0 <= i < len`,
                // then restore the stack for the address math below.
                if self.bounds_checks {
                    let li = self.alloc_local();
                    let lp = self.alloc_local();
                    self.emit(Instruction::LocalSet(li)); // pops i
                    self.emit(Instruction::LocalTee(lp)); // ptr stays on the stack
                    self.emit_bounds_check(lp, li);
                    self.emit(Instruction::LocalGet(li)); // stack is [ptr, i] again
                }
                // addr = ptr + (i + 1) * SLOT, then load the element word. Stack on
                // entry is [ptr, i]; fold it down to a single address.
                self.emit(Instruction::I64Const(SLOT as i64));
                self.emit(Instruction::I64Mul); // i * SLOT
                self.emit(Instruction::I64Add); // ptr + i * SLOT
                self.emit(Instruction::I64Const((header_words * SLOT) as i64));
                self.emit(Instruction::I64Add); // skip header word(s)
                self.emit(Instruction::I32WrapI64);
                self.emit(Instruction::I64Load(memarg(0)));
            }
            Slice | FromChars => unreachable!("handled before generic primitive emission"),
        }
        Ok(Out::Stack)
    }

    fn eval_string_concat(&mut self, left: &Atom, right: &Atom) -> Result<Out, WasmError> {
        let left = self.atom_to_word_local(left)?;
        let right = self.atom_to_word_local(right)?;
        let llen = self.load_word_local(left, 0);
        let rlen = self.load_word_local(right, 0);
        let total = self.alloc_local();
        self.emit(Instruction::LocalGet(llen));
        self.emit(Instruction::LocalGet(rlen));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(total));
        let words = self.alloc_local();
        self.emit(Instruction::LocalGet(total));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(words));
        let out = self.bump_alloc_dyn(words);
        self.store_word(out, 0, |t| {
            t.emit(Instruction::LocalGet(total));
            Ok(())
        })?;
        let one = self.const_local(1);
        self.copy_word_range(left, one, out, one, llen);
        let dst_right = self.alloc_local();
        self.emit(Instruction::LocalGet(llen));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(dst_right));
        self.copy_word_range(right, one, out, dst_right, rlen);
        self.emit(Instruction::LocalGet(out));
        Ok(Out::Stack)
    }

    fn eval_string_slice(
        &mut self,
        base: &Atom,
        start: &Atom,
        end: &Atom,
    ) -> Result<Out, WasmError> {
        let ptr = self.atom_to_word_local(base)?;
        let lo = self.atom_to_word_local(start)?;
        let hi = self.atom_to_word_local(end)?;
        let len = self.load_word_local(ptr, 0);
        if self.bounds_checks {
            self.emit(Instruction::LocalGet(lo));
            self.emit(Instruction::LocalGet(len));
            self.emit(Instruction::I64GtU);
            self.emit(Instruction::LocalGet(hi));
            self.emit(Instruction::LocalGet(len));
            self.emit(Instruction::I64GtU);
            self.emit(Instruction::I32Or);
            self.emit(Instruction::LocalGet(lo));
            self.emit(Instruction::LocalGet(hi));
            self.emit(Instruction::I64GtU);
            self.emit(Instruction::I32Or);
            self.emit(Instruction::If(BlockType::Empty));
            self.emit(Instruction::Unreachable);
            self.emit(Instruction::End);
        }
        let count = self.alloc_local();
        self.emit(Instruction::LocalGet(hi));
        self.emit(Instruction::LocalGet(lo));
        self.emit(Instruction::I64Sub);
        self.emit(Instruction::LocalSet(count));
        let words = self.alloc_local();
        self.emit(Instruction::LocalGet(count));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(words));
        let out = self.bump_alloc_dyn(words);
        self.store_word(out, 0, |t| {
            t.emit(Instruction::LocalGet(count));
            Ok(())
        })?;
        let src_start = self.alloc_local();
        self.emit(Instruction::LocalGet(lo));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(src_start));
        let dst_start = self.const_local(1);
        self.copy_word_range(ptr, src_start, out, dst_start, count);
        self.emit(Instruction::LocalGet(out));
        Ok(Out::Stack)
    }

    fn eval_string_from_chars(&mut self, chars: &Atom) -> Result<Out, WasmError> {
        let list = self.atom_to_word_local(chars)?;
        let len = self.load_word_local(list, 0);
        let words = self.alloc_local();
        self.emit(Instruction::LocalGet(len));
        self.emit(Instruction::I64Const(1));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(words));
        let out = self.bump_alloc_dyn(words);
        self.store_word(out, 0, |t| {
            t.emit(Instruction::LocalGet(len));
            Ok(())
        })?;
        let src_start = self.const_local(2);
        let dst_start = self.const_local(1);
        self.copy_word_range(list, src_start, out, dst_start, len);
        self.emit(Instruction::LocalGet(out));
        Ok(Out::Stack)
    }

    /// Emit an `as` cast (`spec/01` §3.1): integer targets truncate/wrap to their
    /// width, `char` is the code-point identity, and `bool` maps nonzero→true —
    /// matching the interpreter and Cranelift backend so the three agree. Float
    /// targets are not yet supported (the backend is integer-only).
    fn eval_cast(&mut self, value: &Atom, to: &Type) -> Result<Out, WasmError> {
        let av = self.resolve_arg(value)?;
        if matches!(av, ArgVal::Unit) {
            return Err(WasmError::Unsupported("unit operand to a cast".into()));
        }
        self.push_argval(&av);
        match to {
            Type::Int(width) => self.wrap_int(*width),
            // A `char` shares the integer representation (its code point): no-op.
            Type::Char => {}
            Type::Bool => {
                self.emit(Instruction::I64Const(0));
                self.cmp(Instruction::I64Ne);
            }
            Type::Float(_) => {
                return Err(WasmError::Unsupported(
                    "float cast (the backend is integer-only)".into(),
                ))
            }
            _ => return Err(WasmError::Unsupported("cast to a non-scalar type".into())),
        }
        Ok(Out::Stack)
    }

    /// Truncate/wrap the top-of-stack `i64` to a narrower integer width by
    /// shifting the significant bits up and back down — arithmetic shift for
    /// signed widths (sign-extending), logical for unsigned (zero-extending). The
    /// 64-bit widths are the identity. Mirrors the interpreter's `wrap_int`.
    fn wrap_int(&mut self, ty: IntTy) {
        let (bits, signed) = match ty {
            IntTy::I8 => (8, true),
            IntTy::I16 => (16, true),
            IntTy::I32 => (32, true),
            IntTy::U8 => (8, false),
            IntTy::U16 => (16, false),
            IntTy::U32 => (32, false),
            IntTy::I64 | IntTy::Isize | IntTy::U64 | IntTy::Usize => return,
        };
        let shift = (64 - bits) as i64;
        self.emit(Instruction::I64Const(shift));
        self.emit(Instruction::I64Shl);
        self.emit(Instruction::I64Const(shift));
        if signed {
            self.emit(Instruction::I64ShrS);
        } else {
            self.emit(Instruction::I64ShrU);
        }
    }

    /// A wasm comparison yields `i32`; widen it to the uniform `i64` boolean.
    fn cmp(&mut self, op: Instruction<'static>) {
        self.emit(op);
        self.emit(Instruction::I64ExtendI32U);
    }

    /// Lower a `Match`. A `bool` scrutinee (the `if`/`else` desugaring) takes the
    /// two-arm scalar path; a boxed `enum`/`struct` scrutinee takes the runtime
    /// path — load the tag from word 0 and dispatch, binding each variant's fields
    /// by loading them from the payload (MARV-9).
    fn eval_match(&mut self, scrutinee: &Atom, branches: &[Branch]) -> Result<Out, WasmError> {
        let scrut_ty = layout::atom_type(self.world, scrutinee, &self.tys);
        let boxed = scrut_ty
            .as_ref()
            .map(|t| layout::is_boxed(self.world, t))
            .unwrap_or(false);
        if boxed {
            return self.eval_match_boxed(scrutinee, branches, &scrut_ty.unwrap());
        }

        // Scalar path: the front end emits only the two-armed `bool` match
        // (`if`/`else`): branch 0 = false, branch 1 = true (`spec/02` §D).
        if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
            return Err(WasmError::Unsupported(
                "match on a value whose layout could not be determined".into(),
            ));
        }
        // Push the scrutinee (i64) and reduce it to an i32 condition for `if`.
        let cond = self.resolve_arg(scrutinee)?;
        if matches!(cond, ArgVal::Unit) {
            return Err(WasmError::Unsupported("unit match scrutinee".into()));
        }
        self.push_argval(&cond);
        self.emit(Instruction::I64Const(0));
        self.emit(Instruction::I64Ne); // i32: 1 if nonzero (true)

        self.emit(Instruction::If(BlockType::Result(ValType::I64)));
        // `then` = the true arm (branch tag 1).
        let t = self.eval(&branches[1].body)?;
        self.coerce_to_word(t)?;
        self.emit(Instruction::Else);
        // `else` = the false arm (branch tag 0).
        let e = self.eval(&branches[0].body)?;
        self.coerce_to_word(e)?;
        self.emit(Instruction::End);
        Ok(Out::Stack)
    }

    /// The runtime enum/struct `Match` (MARV-9): box the scrutinee to a pointer,
    /// load the tag from word 0, and dispatch over the branches with a chain of
    /// tag-equality `if`/`else`s. Each arm binds its variant's fields by loading
    /// `[i + 1]` from the payload, then evaluates its body; the whole chain leaves
    /// the branch's result on the stack.
    fn eval_match_boxed(
        &mut self,
        scrutinee: &Atom,
        branches: &[Branch],
        scrut_ty: &Type,
    ) -> Result<Out, WasmError> {
        if branches.is_empty() {
            return Err(WasmError::Unsupported("match with no branches".into()));
        }
        // Materialize the scrutinee pointer and its tag into locals (the pointer
        // is reloaded in every arm to bind fields).
        let ptr = self.alloc_local();
        let av = self.resolve_arg(scrutinee)?;
        self.push_argval(&av);
        self.emit(Instruction::LocalSet(ptr));
        let tag = self.alloc_local();
        self.emit(Instruction::LocalGet(ptr));
        self.emit(Instruction::I32WrapI64);
        self.emit(Instruction::I64Load(memarg(0)));
        self.emit(Instruction::LocalSet(tag));

        self.emit_match_arm(branches, 0, ptr, tag, scrut_ty)?;
        Ok(Out::Stack)
    }

    /// Emit the dispatch chain for branch `t` onward: test the tag, run arm `t` in
    /// the `then`, recurse into the `else`. The final arm needs no test (an
    /// exhaustively-checked match always lands on a covered tag).
    fn emit_match_arm(
        &mut self,
        branches: &[Branch],
        t: usize,
        ptr: u32,
        tag: u32,
        scrut_ty: &Type,
    ) -> Result<(), WasmError> {
        if t + 1 == branches.len() {
            return self.emit_bind_and_body(&branches[t], t, ptr, scrut_ty);
        }
        self.emit(Instruction::LocalGet(tag));
        self.emit(Instruction::I64Const(t as i64));
        self.emit(Instruction::I64Eq);
        self.emit(Instruction::If(BlockType::Result(ValType::I64)));
        self.emit_bind_and_body(&branches[t], t, ptr, scrut_ty)?;
        self.emit(Instruction::Else);
        self.emit_match_arm(branches, t + 1, ptr, tag, scrut_ty)?;
        self.emit(Instruction::End);
        Ok(())
    }

    /// Bind variant `tag`'s fields from the payload (`[i + 1]`) into fresh locals,
    /// evaluate the arm body, and leave its word on the stack.
    fn emit_bind_and_body(
        &mut self,
        br: &Branch,
        tag: usize,
        ptr: u32,
        scrut_ty: &Type,
    ) -> Result<(), WasmError> {
        let field_tys =
            layout::variant_fields(self.world, scrut_ty, tag as u32).unwrap_or_default();
        let pushed = br.binds as usize;
        for i in 0..pushed {
            let l = self.alloc_local();
            self.emit(Instruction::LocalGet(ptr));
            self.emit(Instruction::I32WrapI64);
            self.emit(Instruction::I64Load(memarg((i as u64 + 1) * SLOT)));
            self.emit(Instruction::LocalSet(l));
            self.env.push(Slot::Local(l));
            self.tys.push(field_tys.get(i).cloned());
        }
        let out = self.eval(&br.body)?;
        self.coerce_to_word(out)?;
        for _ in 0..pushed {
            self.env.pop();
            self.tys.pop();
        }
        Ok(())
    }

    fn eval_perform(&mut self, cap: &Atom, op: OpId, args: &[Atom]) -> Result<Out, WasmError> {
        let cap_def =
            resolve_cap_def(cap, &self.tys, self.world).ok_or(WasmError::UnresolvedCapability)?;
        let cap_name = self.world.cap_name(&cap_def);
        let import = *self
            .import_index
            .get(&(cap_name, op.0))
            .ok_or(WasmError::UnresolvedCapability)?;
        // Push scalar operands (units occupy no slot).
        for a in args {
            let av = self.resolve_arg(a)?;
            self.push_argval(&av);
        }
        self.emit(Instruction::Call(import));
        // Result type from the cap op signature.
        let returns = self
            .world
            .cap(&cap_def)
            .and_then(|c| c.ops.get(op.0 as usize))
            .map(|s| !matches!(s.ret, Type::Unit))
            .unwrap_or(false);
        if returns {
            Ok(Out::Stack)
        } else {
            Ok(Out::Unit)
        }
    }

    /// Ensure the term's value is exactly one `i64` on the stack: a unit becomes
    /// the zero word, an aggregate is **boxed** into linear memory and its pointer
    /// pushed (MARV-9), and a partial application is a compile error.
    fn coerce_to_word(&mut self, out: Out) -> Result<(), WasmError> {
        match out {
            Out::Stack => Ok(()),
            Out::Unit => {
                self.emit(Instruction::I64Const(0));
                Ok(())
            }
            Out::Returned => Ok(()),
            Out::Partial { .. } => Err(WasmError::Unsupported(
                "a partially-applied function used as a value".into(),
            )),
            Out::Tuple { tag, fields } => self.box_tuple_slots(tag, fields),
        }
    }

    /// Box a compile-time aggregate (its fields are slots) into a fresh
    /// `[tag, field_0, …]` block in linear memory and leave the pointer on the
    /// stack (MARV-9). Nested aggregate fields are boxed recursively.
    fn box_tuple_slots(&mut self, tag: u32, fields: Vec<Slot>) -> Result<(), WasmError> {
        let base = self.bump_alloc(fields.len());
        self.store_word(base, 0, |t| {
            t.emit(Instruction::I64Const(tag as i64));
            Ok(())
        })?;
        for (i, f) in fields.into_iter().enumerate() {
            self.store_word(base, (i as u64 + 1) * SLOT, |t| t.emit_slot_word(&f))?;
        }
        self.emit(Instruction::LocalGet(base));
        Ok(())
    }

    /// Box an aggregate whose fields are resolved arguments (the call-site path)
    /// and leave the pointer on the stack (MARV-9).
    fn box_argvals(&mut self, tag: u32, fields: &[ArgVal]) {
        let base = self.bump_alloc(fields.len());
        // Tag (infallible writers, so the `store_word` results are discarded).
        let _ = self.store_word(base, 0, |t| {
            t.emit(Instruction::I64Const(tag as i64));
            Ok(())
        });
        for (i, f) in fields.iter().enumerate() {
            let _ = self.store_word(base, (i as u64 + 1) * SLOT, |t| {
                t.emit_argval_word(f);
                Ok(())
            });
        }
        self.emit(Instruction::LocalGet(base));
    }

    /// Bump-allocate `n_fields + 1` words (tag + payload) and return the local
    /// holding the base address. Memory grows before the bump pointer is
    /// committed, and scalar-carried loops reset it at safe scope boundaries.
    fn bump_alloc(&mut self, n_fields: usize) -> u32 {
        let total = (n_fields as i64 + 1) * SLOT as i64;
        let base = self.alloc_local();
        let end = self.alloc_local();
        self.emit(Instruction::GlobalGet(BUMP));
        self.emit(Instruction::LocalTee(base));
        self.emit(Instruction::I64Const(total));
        self.emit(Instruction::I64Add);
        self.emit(Instruction::LocalSet(end));
        self.ensure_heap_fits(end);
        self.emit(Instruction::LocalGet(end));
        self.emit(Instruction::GlobalSet(BUMP));
        base
    }

    /// Store one word at `base + offset`: emit the address, run `value` to push
    /// the i64, then `i64.store`.
    fn store_word(
        &mut self,
        base: u32,
        offset: u64,
        value: impl FnOnce(&mut Self) -> Result<(), WasmError>,
    ) -> Result<(), WasmError> {
        self.emit(Instruction::LocalGet(base));
        self.emit(Instruction::I32WrapI64);
        value(self)?;
        self.emit(Instruction::I64Store(memarg(offset)));
        Ok(())
    }

    /// Push the word for a slot, boxing a nested aggregate (a unit field occupies
    /// a zero word so field indices stay stable).
    fn emit_slot_word(&mut self, s: &Slot) -> Result<(), WasmError> {
        match s {
            Slot::Local(l) => self.emit(Instruction::LocalGet(*l)),
            Slot::Unit => self.emit(Instruction::I64Const(0)),
            Slot::Partial { .. } => {
                return Err(WasmError::Unsupported(
                    "a partially-applied function used as an aggregate field".into(),
                ))
            }
            Slot::Tuple { tag, fields } => return self.box_tuple_slots(*tag, fields.clone()),
        }
        Ok(())
    }

    /// Push the word for a resolved argument used as an aggregate field (a unit
    /// occupies a zero word here, unlike a call argument where it is dropped).
    fn emit_argval_word(&mut self, av: &ArgVal) {
        match av {
            ArgVal::Local(l) => self.emit(Instruction::LocalGet(*l)),
            ArgVal::Const(n) => self.emit(Instruction::I64Const(*n)),
            ArgVal::Unit => self.emit(Instruction::I64Const(0)),
            ArgVal::Boxed { tag, fields } => self.box_argvals(*tag, fields),
        }
    }
}

// ---- helpers ------------------------------------------------------------

fn qualify(module_path: &str, name: &str) -> String {
    if module_path.is_empty() {
        name.to_string()
    } else {
        format!("{module_path}.{name}")
    }
}

fn peel_lams(mut body: &Core) -> &Core {
    while let Core::Lam { body: inner, .. } = body {
        body = inner;
    }
    body
}

fn peel_param_types(mut ty: &Type) -> Vec<Type> {
    let mut params = Vec::new();
    while let Type::Arrow { param, ret, .. } = ty {
        params.push((**param).clone());
        ty = ret;
    }
    params
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

fn is_list_type(t: &Type) -> bool {
    matches!(
        t,
        Type::Nominal { def, args }
            if *def == symbol_hash("std.collections.List") && args.len() == 1
    )
}
