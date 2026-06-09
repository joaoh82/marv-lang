//! # marv-codegen-cl — Cranelift backend (milestone M4, aggregates MARV-9, arrays MARV-30)
//!
//! Compiles the canonical **Core IR** (`marv-core`) to native code with
//! Cranelift, JIT-compiling each definition so the result can be called
//! in-process. It is the second backend behind the tree-walking interpreter
//! (`marv-interp`); the M4 acceptance gate is that the two **agree** on a corpus
//! of programs (`spec/01` §9 — "same Core IR feeds both").
//!
//! ## What it compiles
//!
//! The integer/boolean core that the front end lowers — arithmetic and
//! comparison [`PrimOp`]s, `if`/`else` (a two-branch `bool` [`Core::Match`]),
//! `let` bindings, curried calls between top-level functions (recursion
//! included), `while`/`for` loops ([`Core::Loop`], lowered to an SSA
//! header/body/exit) — **plus aggregates and enums** (MARV-9): a `struct`/tuple
//! product or an `enum` variant is a heap-boxed `[tag, field_0, …]` block, so
//! `Ctor`/`Proj`/n-way `Match` (with field binding) lower to real allocation,
//! loads, and a jump table on the tag. **Arrays** (MARV-30) reuse the same boxed
//! shape with the element count in the header word (`[len, e0, …]`), so a
//! `Core::Array` boxes like a tuple, `len` is a header load, `index` loads
//! `[i + 1]`, and an element store is a functional rebuild lowered upstream.
//! Every scalar still lives in a 64-bit register, so the backend's wrapping
//! arithmetic matches the oracle's `i64` semantics — the property that keeps the
//! differential test meaningful.
//!
//! ## Aggregate representation (MARV-9)
//!
//! Every marv value is one machine word. A scalar *is* that word; an aggregate
//! is a **pointer** to `(1 + arity)` contiguous `i64` words laid out as
//! `[tag, field_0, …, field_{n-1}]` (`spec/02` §C). The layout is identical to
//! the interpreter's `Value::Agg` and to the WASM backend's linear-memory form,
//! so all three agree by construction. Boxing is *lazy*: a `Ctor` first becomes a
//! compile-time [`Slot::Tuple`] (register-resident, which is what loop state and
//! purely-local products want) and is only spilled to the heap when it must cross
//! a function boundary, be returned, or be matched as a runtime value. Allocation
//! goes through the host `marv_rt_alloc` symbol and **leaks** — marv has no GC
//! yet (`spec/01` §4 leaves reclamation to a later milestone), which is fine for
//! the short-lived programs the JIT runs.
//!
//! ## Currying without heap closures
//!
//! Core application is curried and ANF-sequenced (`f(a, b)` becomes
//! `let t = App(Global f, a); App(t, b)`). The translator resolves this at
//! compile time: a [`Slot::Partial`] accumulates arguments across the `App`
//! spine and is lowered to a single direct Cranelift `call` the moment it is
//! saturated. Because the front end never emits a partially-applied function as
//! a *value*, no runtime closure is needed.

use std::collections::HashMap;

use cranelift::codegen::ir::{BlockArg, JumpTableData, Type as ClType};
use cranelift::codegen::settings::{self, Configurable};
// Explicit imports (not a glob) so cranelift's `Type` does not collide with
// `marv_core::ir::Type`, which we glob-import below as the Core type model.
use cranelift::prelude::{
    types, AbiParam, FunctionBuilder, FunctionBuilderContext, InstBuilder, IntCC, MemFlags,
    TrapCode, Value,
};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module, ModuleError};

use marv_core::ir::*;
use marv_core::symbol_hash;
use marv_types::{layout, World};

/// Every scalar the backend handles is a machine word; using one width keeps the
/// backend's wrapping arithmetic identical to the interpreter's `i64` oracle, and
/// an aggregate pointer is the same width (one word).
const WORD: ClType = types::I64;

/// Bytes per aggregate slot — every field (and the tag) is one machine word.
const SLOT: i32 = 8;

/// The host allocator the compiled code calls to box an aggregate: it returns a
/// pointer to `n_words` zeroed `i64` slots. marv has no GC yet (`spec/01` §4), so
/// this **leaks** — acceptable for the short-lived programs the JIT runs, and the
/// interpreter oracle leaks the same logical garbage (it never frees a `Value`).
extern "C" fn marv_rt_alloc(n_words: i64) -> i64 {
    let n = n_words.max(0) as usize;
    let mut buf = vec![0i64; n];
    let ptr = buf.as_mut_ptr() as i64;
    std::mem::forget(buf);
    ptr
}

/// A backend failure. Like the interpreter's `RunError`, these are conditions
/// the *codegen* cannot handle — never type errors (the M2 checker has already
/// run).
#[derive(Debug, Clone)]
pub enum CodegenError {
    /// A Core construct this backend does not lower yet (carries a description).
    Unsupported(String),
    /// A referenced global is not a known function in this program.
    UnknownGlobal(Hash),
    /// The requested entry point does not exist.
    NoSuchEntry(String),
    /// An entry was called with the wrong number of value arguments.
    ArgCount { expected: usize, got: usize },
    /// The host machine / Cranelift configuration could not be initialized, or a
    /// module operation failed.
    Backend(String),
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodegenError::Unsupported(d) => write!(f, "cranelift backend: unsupported: {d}"),
            CodegenError::UnknownGlobal(h) => {
                write!(f, "cranelift backend: unknown global {}", h.to_b3())
            }
            CodegenError::NoSuchEntry(e) => write!(f, "cranelift backend: no entry point `{e}`"),
            CodegenError::ArgCount { expected, got } => write!(
                f,
                "cranelift backend: entry expects {expected} argument(s), got {got}"
            ),
            CodegenError::Backend(m) => write!(f, "cranelift backend: {m}"),
        }
    }
}

impl std::error::Error for CodegenError {}

impl From<ModuleError> for CodegenError {
    fn from(e: ModuleError) -> Self {
        CodegenError::Backend(e.to_string())
    }
}

/// The compile-time metadata for one function: its Cranelift id, its curried
/// arity (number of parameters / lambdas), and which parameters are unit (so
/// they carry no runtime register and are dropped from the calling convention).
#[derive(Clone)]
struct FnMeta {
    id: FuncId,
    arity: usize,
    /// `param_is_unit[i]` is true iff the i-th curried parameter has type
    /// `Unit`; such parameters get no Cranelift ABI slot.
    param_is_unit: Vec<bool>,
    /// The declared parameter types, in curried order — threaded into the body's
    /// type environment so a `Match` can tell a `bool` scrutinee (the value *is*
    /// the tag) from a boxed enum (the tag lives at word 0). See [`layout`].
    param_tys: Vec<Type>,
}

impl FnMeta {
    /// The number of parameters that actually appear in the calling convention.
    fn abi_param_count(&self) -> usize {
        self.param_is_unit.iter().filter(|u| !**u).count()
    }
}

/// A JIT-compiled program: the finalized module plus the lookup needed to call
/// an entry point.
pub struct JitProgram {
    module: JITModule,
    /// `symbol_hash(qualified)` → metadata, and a name → hash map for entry
    /// resolution.
    metas: HashMap<Hash, FnMeta>,
    names: HashMap<String, Hash>,
}

/// Compile a set of definitions (named in `module_path`'s scope) to native code
/// with Cranelift, JIT-linking them so calls between them resolve.
///
/// Definitions are keyed under `symbol_hash("<module>.<name>")` — the hash a
/// body's `Atom::Global` carries (see `marv_core::lower`) — so intra-module
/// calls and recursion bind correctly. `world` supplies the struct/enum
/// declarations the aggregate paths need for layout (`spec/02` §C).
pub fn compile(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
) -> Result<JitProgram, CodegenError> {
    let mut module = make_module()?;

    // The host allocator is an import every boxing site can call.
    let mut alloc_sig = module.make_signature();
    alloc_sig.params.push(AbiParam::new(WORD));
    alloc_sig.returns.push(AbiParam::new(WORD));
    let alloc_id = module.declare_function("marv_rt_alloc", Linkage::Import, &alloc_sig)?;

    // Pass 1: declare every function (signature only) so that bodies compiled in
    // pass 2 can reference any callee — including not-yet-compiled and recursive
    // ones — by id.
    let mut metas: HashMap<Hash, FnMeta> = HashMap::new();
    let mut names: HashMap<String, Hash> = HashMap::new();
    let mut order: Vec<(Hash, usize)> = Vec::new();
    for (idx, (name, def)) in defs.iter().enumerate() {
        if def.kind != DefKind::Fn {
            continue;
        }
        // Skip generic templates (signatures mentioning a `Type::Var`): they have
        // no concrete ABI and are never called directly — only their
        // monomorphizations (`max@i64`, …) are. Compiling the template would also
        // fail, since its body references interface methods (`cmp`) that resolve
        // only in a specialized, impl-dispatched context (`spec/01` §§3.3–3.4).
        if def.ty.is_polymorphic() {
            continue;
        }
        let qualified = qualify(module_path, name);
        let h = symbol_hash(&qualified);
        let param_tys = peel_param_types(&def.ty);
        let param_is_unit: Vec<bool> = param_tys.iter().map(|t| matches!(t, Type::Unit)).collect();
        let arity = param_tys.len();

        let mut sig = module.make_signature();
        for _ in 0..param_is_unit.iter().filter(|u| !**u).count() {
            sig.params.push(AbiParam::new(WORD));
        }
        sig.returns.push(AbiParam::new(WORD));
        let id = module.declare_function(&qualified, Linkage::Export, &sig)?;

        metas.insert(
            h,
            FnMeta {
                id,
                arity,
                param_is_unit,
                param_tys,
            },
        );
        names.insert(name.clone(), h);
        names.insert(qualified.clone(), h);
        order.push((h, idx));
    }

    // Pass 2: compile each function body.
    let mut ctx = module.make_context();
    let mut fb_ctx = FunctionBuilderContext::new();
    for (h, idx) in &order {
        let (_, def) = &defs[*idx];
        compile_fn(
            &mut module,
            &metas,
            world,
            alloc_id,
            &mut ctx,
            &mut fb_ctx,
            *h,
            def,
        )?;
        module.clear_context(&mut ctx);
    }

    module
        .finalize_definitions()
        .map_err(|e| CodegenError::Backend(e.to_string()))?;

    Ok(JitProgram {
        module,
        metas,
        names,
    })
}

impl JitProgram {
    /// Resolve an entry point the same way the interpreter does: an explicit
    /// name (bare or qualified), else `main`, else the sole function.
    fn resolve_entry(&self, entry: &str) -> Result<Hash, CodegenError> {
        if !entry.is_empty() {
            return self
                .names
                .get(entry)
                .copied()
                .ok_or_else(|| CodegenError::NoSuchEntry(entry.to_string()));
        }
        if let Some(h) = self.names.get("main").copied() {
            return Ok(h);
        }
        let all: Vec<Hash> = self.metas.keys().copied().collect();
        match all.as_slice() {
            [h] => Ok(*h),
            _ => Err(CodegenError::NoSuchEntry("main".to_string())),
        }
    }

    /// The number of machine-word arguments an entry expects (its non-unit
    /// parameters).
    pub fn entry_arity(&self, entry: &str) -> Result<usize, CodegenError> {
        let h = self.resolve_entry(entry)?;
        Ok(self.metas[&h].abi_param_count())
    }

    /// Call a compiled entry point with `args` (one per non-unit parameter),
    /// returning the 64-bit result. Supports up to four arguments — enough for
    /// the differential corpus; more would just extend the match below.
    ///
    /// # Safety of the transmute
    ///
    /// The function was declared with exactly `args.len()` `i64` parameters and
    /// one `i64` return in [`compile`], so transmuting its finalized pointer to
    /// the matching `extern "C"` signature is sound.
    pub fn run_i64(&self, entry: &str, args: &[i64]) -> Result<i64, CodegenError> {
        let h = self.resolve_entry(entry)?;
        let meta = &self.metas[&h];
        let expected = meta.abi_param_count();
        if args.len() != expected {
            return Err(CodegenError::ArgCount {
                expected,
                got: args.len(),
            });
        }
        let ptr: *const u8 = self.module.get_finalized_function(meta.id);
        // SAFETY: see the doc comment — signature matches by construction.
        unsafe {
            Ok(match args {
                [] => std::mem::transmute::<*const u8, extern "C" fn() -> i64>(ptr)(),
                [a] => std::mem::transmute::<*const u8, extern "C" fn(i64) -> i64>(ptr)(*a),
                [a, b] => {
                    std::mem::transmute::<*const u8, extern "C" fn(i64, i64) -> i64>(ptr)(*a, *b)
                }
                [a, b, c] => std::mem::transmute::<*const u8, extern "C" fn(i64, i64, i64) -> i64>(
                    ptr,
                )(*a, *b, *c),
                [a, b, c, d] => std::mem::transmute::<
                    *const u8,
                    extern "C" fn(i64, i64, i64, i64) -> i64,
                >(ptr)(*a, *b, *c, *d),
                _ => {
                    return Err(CodegenError::Unsupported(
                        "entry points with more than four arguments".into(),
                    ))
                }
            })
        }
    }
}

/// Build a fresh JIT module configured for the host machine, with the host
/// allocator symbol registered so boxing sites can call it.
fn make_module() -> Result<JITModule, CodegenError> {
    let mut flags = settings::builder();
    // The JIT loads code into its own process; position-independent code and
    // colocated libcalls are unnecessary and complicate relocation.
    flags
        .set("use_colocated_libcalls", "false")
        .map_err(|e| CodegenError::Backend(e.to_string()))?;
    flags
        .set("is_pic", "false")
        .map_err(|e| CodegenError::Backend(e.to_string()))?;
    let isa_builder = cranelift_native::builder()
        .map_err(|m| CodegenError::Backend(format!("host machine is not supported: {m}")))?;
    let isa = isa_builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| CodegenError::Backend(e.to_string()))?;
    let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
    builder.symbol("marv_rt_alloc", marv_rt_alloc as *const u8);
    Ok(JITModule::new(builder))
}

/// Compile one function definition into the module.
#[allow(clippy::too_many_arguments)]
fn compile_fn(
    module: &mut JITModule,
    metas: &HashMap<Hash, FnMeta>,
    world: &World,
    alloc_id: FuncId,
    ctx: &mut cranelift::codegen::Context,
    fb_ctx: &mut FunctionBuilderContext,
    h: Hash,
    def: &Def,
) -> Result<(), CodegenError> {
    let meta = metas[&h].clone();
    // Rebuild the signature into the context's function.
    for _ in 0..meta.abi_param_count() {
        ctx.func.signature.params.push(AbiParam::new(WORD));
    }
    ctx.func.signature.returns.push(AbiParam::new(WORD));

    let body = def
        .body
        .as_ref()
        .ok_or_else(|| CodegenError::Unsupported("function without a body".into()))?;
    let inner = peel_lams(body);

    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, fb_ctx);
        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // Bind parameters at de Bruijn levels 0..arity. Unit parameters get no
        // ABI slot, so map them to `Slot::Unit`; the rest pull the next block
        // parameter, in order. Each binder's type is recorded in parallel so a
        // boxed-enum `Match` over a parameter can be told from a `bool` one.
        let block_params: Vec<Value> = builder.block_params(entry_block).to_vec();
        let mut env: Vec<Slot> = Vec::with_capacity(meta.arity);
        let mut tys: Vec<Option<Type>> = Vec::with_capacity(meta.arity);
        let mut abi_i = 0usize;
        for (i, is_unit) in meta.param_is_unit.iter().enumerate() {
            if *is_unit {
                env.push(Slot::Unit);
            } else {
                env.push(Slot::Val(block_params[abi_i]));
                abi_i += 1;
            }
            tys.push(meta.param_tys.get(i).cloned());
        }

        // Scope the translator so its `&mut builder` borrow ends before
        // `finalize` (which consumes the builder).
        {
            let mut trans = Trans {
                module,
                metas,
                world,
                alloc_id,
                alloc_ref: None,
                builder: &mut builder,
                env,
                tys,
            };
            let result = trans.eval(inner)?;
            let ret = trans.as_word(result)?;
            trans.builder.ins().return_(&[ret]);
        }
        builder.finalize();
    }

    module.define_function(meta.id, ctx)?;
    Ok(())
}

/// A compile-time value: a real machine word, a (zero-sized) unit, a
/// partially-applied call accumulating its arguments (the currying mirror), or a
/// compile-time aggregate.
#[derive(Clone)]
enum Slot {
    Val(Value),
    Unit,
    Partial {
        func: Hash,
        got: Vec<Slot>,
    },
    /// A compile-time aggregate (a product/struct `Ctor`, an enum-variant `Ctor`
    /// whose tag is `tag`, or the loop-carried-state tuple a `Loop` yields). Its
    /// leaves live in registers; it is **boxed** to the heap lazily — only when
    /// it must become a single word ([`Trans::as_word`]), i.e. cross a function
    /// boundary, be returned, or be matched at runtime (MARV-9). A `Proj` selects
    /// a field at compile time.
    Tuple {
        tag: u32,
        fields: Vec<Slot>,
    },
}

/// The per-function translation state.
struct Trans<'a, 'b> {
    module: &'a mut JITModule,
    metas: &'a HashMap<Hash, FnMeta>,
    world: &'a World,
    alloc_id: FuncId,
    /// The allocator's func-ref in the current function, declared lazily on the
    /// first boxing site.
    alloc_ref: Option<cranelift::codegen::ir::FuncRef>,
    builder: &'a mut FunctionBuilder<'b>,
    /// Environment indexed by de Bruijn *level* (`env[0]` = outermost binder).
    env: Vec<Slot>,
    /// Binder types, parallel to `env` (`None` when unknown). Drives the
    /// `Match` scalar-vs-boxed decision and boxed field binding.
    tys: Vec<Option<Type>>,
}

impl Trans<'_, '_> {
    fn eval(&mut self, c: &Core) -> Result<Slot, CodegenError> {
        match c {
            Core::Atom(a) => self.eval_atom(a),

            Core::Let { value, body } => {
                let v = self.eval(value)?;
                let world = self.world;
                let t = layout::type_of(world, value, &mut self.tys);
                self.env.push(v);
                self.tys.push(t);
                let r = self.eval(body);
                self.env.pop();
                self.tys.pop();
                r
            }

            Core::App { func, arg } => {
                let f = self.eval_atom(func)?;
                let a = self.eval_atom(arg)?;
                self.apply(f, a)
            }

            Core::Prim { op, args } => self.eval_prim(*op, args),

            Core::Cast { value, to } => self.eval_cast(value, to),

            Core::Match {
                scrutinee,
                branches,
            } => self.eval_match(scrutinee, branches),

            Core::Ctor { tag, fields, .. } => {
                // A product/enum value as a compile-time tuple (boxed lazily; see
                // `Slot::Tuple`). Fields are atomic in ANF.
                let mut slots = Vec::with_capacity(fields.len());
                for a in fields {
                    slots.push(self.eval_atom(a)?);
                }
                Ok(Slot::Tuple {
                    tag: *tag,
                    fields: slots,
                })
            }

            // An array literal: a compile-time tuple whose header word (the
            // `Slot::Tuple` tag) is the element **count**, so once boxed the block
            // is `[len, e0, …]`. `len` then reads word 0 and `index` loads
            // `[i + 1]` (`eval_prim`). Boxing is still lazy (`Slot::Tuple`).
            Core::Array { items, .. } => {
                let mut slots = Vec::with_capacity(items.len());
                for a in items {
                    slots.push(self.eval_atom(a)?);
                }
                Ok(Slot::Tuple {
                    tag: items.len() as u32,
                    fields: slots,
                })
            }

            Core::IndexSet { base, index, value } => self.eval_index_set(base, index, value),

            Core::Proj { base, idx } => self.eval_proj(base, *idx),

            Core::Loop {
                state, cond, body, ..
            } => self.eval_loop(state, cond, body),

            // A second-class reference has no runtime cell (mutable value
            // semantics, `spec/01` §4); it evaluates to its referent's value.
            Core::Ref { of, .. } => self.eval_atom(of),

            Core::Lam { .. } => Err(CodegenError::Unsupported("first-class lambda".into())),
            Core::Perform { .. } => Err(CodegenError::Unsupported(
                "capability perform (use the interpreter)".into(),
            )),
            Core::Raise { .. } => Err(CodegenError::Unsupported("raise".into())),
        }
    }

    /// Project field `idx`. A compile-time tuple selects the field directly; a
    /// boxed aggregate (a word that is a pointer) loads from `[idx + 1]` — word 0
    /// is the tag (MARV-9).
    fn eval_proj(&mut self, base: &Atom, idx: u32) -> Result<Slot, CodegenError> {
        let b = self.eval_atom(base)?;
        match b {
            Slot::Tuple { mut fields, .. } => {
                let i = idx as usize;
                if i < fields.len() {
                    Ok(fields.swap_remove(i))
                } else {
                    Err(CodegenError::Unsupported("projection out of range".into()))
                }
            }
            Slot::Val(ptr) => {
                let off = (idx as i32 + 1) * SLOT;
                let v = self.builder.ins().load(WORD, MemFlags::trusted(), ptr, off);
                Ok(Slot::Val(v))
            }
            _ => Err(CodegenError::Unsupported(
                "projection of a non-aggregate".into(),
            )),
        }
    }

    /// Lower a [`Core::Loop`] to SSA control flow: a `header` block carrying the
    /// loop-carried state as block parameters, a `body` block that computes the
    /// next state and jumps back, and an `exit` block carrying the final state
    /// (`spec/02` §C `Loop`). Loop state stays in registers (never boxed), so the
    /// already-tested loop lowering is unchanged by MARV-9. The loop evaluates to
    /// the final state as a compile-time [`Slot::Tuple`] for the enclosing scope
    /// to project. Invariants are Tier-1/Tier-2 obligations checked elsewhere.
    fn eval_loop(
        &mut self,
        state: &[Atom],
        cond: &Core,
        body: &Core,
    ) -> Result<Slot, CodegenError> {
        let k = state.len();
        // Initial carried values (loop-carried state must be scalar words), and
        // their types (so a carried aggregate pointer can still be matched).
        let mut init: Vec<BlockArg> = Vec::with_capacity(k);
        let mut carried_tys: Vec<Option<Type>> = Vec::with_capacity(k);
        for a in state {
            let s = self.eval_atom(a)?;
            init.push(self.as_word(s)?.into());
            carried_tys.push(layout::atom_type(self.world, a, &self.tys));
        }

        let header = self.builder.create_block();
        let body_block = self.builder.create_block();
        let exit = self.builder.create_block();
        for _ in 0..k {
            self.builder.append_block_param(header, WORD);
            self.builder.append_block_param(exit, WORD);
        }

        self.builder.ins().jump(header, &init);

        // Header: bind the carried params as the innermost env slots, test the
        // condition, and branch to the body (continue) or the exit (with the
        // current carried values as the loop's result).
        self.builder.switch_to_block(header);
        let carried: Vec<Value> = self.builder.block_params(header).to_vec();
        for (v, t) in carried.iter().zip(&carried_tys) {
            self.env.push(Slot::Val(*v));
            self.tys.push(t.clone());
        }
        let c = self.eval(cond)?;
        let c = self.as_word(c)?;
        let exit_args: Vec<BlockArg> = carried.iter().map(|v| (*v).into()).collect();
        self.builder
            .ins()
            .brif(c, body_block, &[], exit, &exit_args);

        // Body: compute the next state, pop the carried slots, jump back.
        self.builder.switch_to_block(body_block);
        self.builder.seal_block(body_block);
        let next = self.eval(body)?;
        let next_args: Vec<BlockArg> = match next {
            Slot::Tuple { fields, .. } => fields
                .into_iter()
                .map(|s| self.as_word(s).map(BlockArg::from))
                .collect::<Result<_, _>>()?,
            other => {
                let _ = other;
                return Err(CodegenError::Unsupported(
                    "loop body did not produce its carried state".into(),
                ));
            }
        };
        for _ in 0..k {
            self.env.pop();
            self.tys.pop();
        }
        self.builder.ins().jump(header, &next_args);
        self.builder.seal_block(header);

        // Exit: the loop's result is the final carried state.
        self.builder.switch_to_block(exit);
        self.builder.seal_block(exit);
        let finals: Vec<Slot> = self
            .builder
            .block_params(exit)
            .iter()
            .map(|v| Slot::Val(*v))
            .collect();
        Ok(Slot::Tuple {
            tag: 0,
            fields: finals,
        })
    }

    /// Lower a runtime element store `s[i] = e` ([`Core::IndexSet`], MARV-33).
    /// Unlike the array store (unrolled over a static length), a slice's length is
    /// only known at runtime, so this emits the functional update directly: read
    /// the element count from the header, allocate a fresh `[len, …]` block, copy
    /// every word into it with a runtime loop, then overwrite element `i`. The
    /// result is the new block pointer, which the surface store rebinds the root
    /// to (mutable value semantics — the old block is untouched, `spec/01` §4).
    fn eval_index_set(
        &mut self,
        base: &Atom,
        index: &Atom,
        value: &Atom,
    ) -> Result<Slot, CodegenError> {
        let base_slot = self.eval_atom(base)?;
        let ptr = self.as_word(base_slot)?;
        let i = self.eval_atom(index)?;
        let i = self.as_word(i)?;
        let v = self.eval_atom(value)?;
        let v = self.as_word(v)?;

        // The header word holds the element count; the block is `len + 1` words.
        let len = self.builder.ins().load(WORD, MemFlags::trusted(), ptr, 0);
        let total = self.builder.ins().iadd_imm(len, 1);
        let newptr = self.alloc_dyn(total);

        // Copy loop: `for k in 0..total { new[k] = old[k] }` (header + elements).
        let header = self.builder.create_block();
        let body = self.builder.create_block();
        let exit = self.builder.create_block();
        self.builder.append_block_param(header, WORD); // the induction counter `k`
        let zero = self.builder.ins().iconst(WORD, 0);
        self.builder.ins().jump(header, &[zero.into()]);

        self.builder.switch_to_block(header);
        let k = self.builder.block_params(header)[0];
        let more = self.builder.ins().icmp(IntCC::UnsignedLessThan, k, total);
        self.builder.ins().brif(more, body, &[], exit, &[]);

        self.builder.switch_to_block(body);
        self.builder.seal_block(body);
        let off = self.builder.ins().imul_imm(k, SLOT as i64);
        let src = self.builder.ins().iadd(ptr, off);
        let w = self.builder.ins().load(WORD, MemFlags::trusted(), src, 0);
        let dst = self.builder.ins().iadd(newptr, off);
        self.builder.ins().store(MemFlags::trusted(), w, dst, 0);
        let k1 = self.builder.ins().iadd_imm(k, 1);
        self.builder.ins().jump(header, &[k1.into()]);
        self.builder.seal_block(header);

        // Exit: overwrite the one element at `[i + 1]` with the new value.
        self.builder.switch_to_block(exit);
        self.builder.seal_block(exit);
        let plus1 = self.builder.ins().iadd_imm(i, 1);
        let eoff = self.builder.ins().imul_imm(plus1, SLOT as i64);
        let eaddr = self.builder.ins().iadd(newptr, eoff);
        self.builder.ins().store(MemFlags::trusted(), v, eaddr, 0);
        Ok(Slot::Val(newptr))
    }

    fn eval_atom(&mut self, a: &Atom) -> Result<Slot, CodegenError> {
        match a {
            Atom::Lit(l) => self.lit(l),
            Atom::Var(idx) => {
                let d = self.env.len();
                let i = (*idx as usize) + 1;
                if i > d {
                    return Err(CodegenError::Unsupported(format!(
                        "de Bruijn index {idx} out of scope at depth {d}"
                    )));
                }
                Ok(self.env[d - i].clone())
            }
            Atom::Global(h) => {
                if self.metas.contains_key(h) {
                    Ok(Slot::Partial {
                        func: *h,
                        got: Vec::new(),
                    })
                } else {
                    Err(CodegenError::UnknownGlobal(*h))
                }
            }
        }
    }

    fn lit(&mut self, l: &Literal) -> Result<Slot, CodegenError> {
        match l {
            Literal::Unit => Ok(Slot::Unit),
            Literal::Bool(b) => Ok(Slot::Val(self.builder.ins().iconst(WORD, *b as i64))),
            Literal::Int(n) => Ok(Slot::Val(self.builder.ins().iconst(WORD, *n))),
            Literal::Float(_) => Err(CodegenError::Unsupported("float literal".into())),
            Literal::Str(_) => Err(CodegenError::Unsupported("string literal".into())),
            // A `char` is its Unicode code point as a machine word — the same
            // scalar the interpreter computes, keeping the two in agreement.
            Literal::Char(c) => Ok(Slot::Val(self.builder.ins().iconst(WORD, *c as i64))),
        }
    }

    /// Apply a partial to one more argument, emitting a direct call once the
    /// last curried parameter arrives.
    fn apply(&mut self, f: Slot, arg: Slot) -> Result<Slot, CodegenError> {
        let (func, mut got) = match f {
            Slot::Partial { func, got } => (func, got),
            _ => {
                return Err(CodegenError::Unsupported(
                    "application of a non-function".into(),
                ))
            }
        };
        got.push(arg);
        let meta = self
            .metas
            .get(&func)
            .ok_or(CodegenError::UnknownGlobal(func))?;
        if got.len() < meta.arity {
            return Ok(Slot::Partial { func, got });
        }
        // Saturated: drop unit arguments, lower the rest to words (boxing any
        // aggregate argument), and call.
        let param_is_unit = meta.param_is_unit.clone();
        let func_id = meta.id;
        let mut call_args: Vec<Value> = Vec::with_capacity(got.len());
        for (slot, is_unit) in got.into_iter().zip(param_is_unit) {
            if is_unit {
                continue;
            }
            call_args.push(self.as_word(slot)?);
        }
        let func_ref = self.module.declare_func_in_func(func_id, self.builder.func);
        let call = self.builder.ins().call(func_ref, &call_args);
        let results = self.builder.inst_results(call);
        Ok(Slot::Val(results[0]))
    }

    fn eval_prim(&mut self, op: PrimOp, args: &[Atom]) -> Result<Slot, CodegenError> {
        use PrimOp::*;
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            let s = self.eval_atom(a)?;
            vals.push(self.as_word(s)?);
        }
        let v = |i: usize| vals[i];
        let out = match op {
            Add => self.builder.ins().iadd(v(0), v(1)),
            Sub => self.builder.ins().isub(v(0), v(1)),
            Mul => self.builder.ins().imul(v(0), v(1)),
            Div => self.builder.ins().sdiv(v(0), v(1)),
            Rem => self.builder.ins().srem(v(0), v(1)),
            Eq => self.cmp(IntCC::Equal, v(0), v(1)),
            Ne => self.cmp(IntCC::NotEqual, v(0), v(1)),
            Lt => self.cmp(IntCC::SignedLessThan, v(0), v(1)),
            Le => self.cmp(IntCC::SignedLessThanOrEqual, v(0), v(1)),
            Gt => self.cmp(IntCC::SignedGreaterThan, v(0), v(1)),
            Ge => self.cmp(IntCC::SignedGreaterThanOrEqual, v(0), v(1)),
            And => self.builder.ins().band(v(0), v(1)),
            Or => self.builder.ins().bor(v(0), v(1)),
            Not => self.builder.ins().bxor_imm(v(0), 1),
            Neg => self.builder.ins().ineg(v(0)),
            // `len(a)` / `a[i]` over a boxed array (`[len, e0, …]`, MARV-30). The
            // operand is coerced to a word above, so `v(0)` is the array pointer;
            // boxing wrote the element count into the header (word 0) and element
            // `i` at word `i + 1`.
            Len => self.builder.ins().load(WORD, MemFlags::trusted(), v(0), 0),
            Index => {
                // addr = base + (i + 1) * SLOT, then load the element word.
                let plus1 = self.builder.ins().iadd_imm(v(1), 1);
                let off = self.builder.ins().imul_imm(plus1, SLOT as i64);
                let addr = self.builder.ins().iadd(v(0), off);
                self.builder.ins().load(WORD, MemFlags::trusted(), addr, 0)
            }
        };
        Ok(Slot::Val(out))
    }

    /// Emit an `as` cast (`spec/01` §3.1). Integer targets truncate/wrap to their
    /// width, `char` is the code-point identity, and `bool` maps nonzero→true —
    /// matching the interpreter's `eval_cast` so the backends agree. Float targets
    /// are not yet supported (the backend is integer-only, like float literals).
    fn eval_cast(&mut self, value: &Atom, to: &Type) -> Result<Slot, CodegenError> {
        let v = self.eval_atom(value)?;
        let v = self.as_word(v)?;
        let out = match to {
            Type::Int(width) => self.wrap_int(v, *width),
            // A `char` shares the integer representation (its code point).
            Type::Char => v,
            Type::Bool => {
                let c = self.builder.ins().icmp_imm(IntCC::NotEqual, v, 0);
                self.builder.ins().uextend(WORD, c)
            }
            Type::Float(_) => {
                return Err(CodegenError::Unsupported(
                    "float cast (the backend is integer-only)".into(),
                ))
            }
            _ => {
                return Err(CodegenError::Unsupported(
                    "cast to a non-scalar type".into(),
                ))
            }
        };
        Ok(Slot::Val(out))
    }

    /// Truncate/wrap a machine word to a narrower integer width by shifting the
    /// significant bits up and back down — arithmetically for signed widths
    /// (sign-extending), logically for unsigned (zero-extending). The 64-bit
    /// widths are the identity. Mirrors the interpreter's `wrap_int`.
    fn wrap_int(&mut self, v: Value, ty: IntTy) -> Value {
        let (bits, signed) = match ty {
            IntTy::I8 => (8, true),
            IntTy::I16 => (16, true),
            IntTy::I32 => (32, true),
            IntTy::U8 => (8, false),
            IntTy::U16 => (16, false),
            IntTy::U32 => (32, false),
            IntTy::I64 | IntTy::Isize | IntTy::U64 | IntTy::Usize => return v,
        };
        let shift = 64 - bits;
        let up = self.builder.ins().ishl_imm(v, shift);
        if signed {
            self.builder.ins().sshr_imm(up, shift)
        } else {
            self.builder.ins().ushr_imm(up, shift)
        }
    }

    /// Emit a comparison whose `i8` result is zero-extended to a machine word so
    /// booleans share the integer representation everywhere.
    fn cmp(&mut self, cc: IntCC, a: Value, b: Value) -> Value {
        let c = self.builder.ins().icmp(cc, a, b);
        self.builder.ins().uextend(WORD, c)
    }

    /// Lower a `Match`. A `bool` scrutinee (the `if`/`else` desugaring) takes the
    /// two-arm scalar path; a boxed `enum`/`struct` scrutinee takes the runtime
    /// path — load the tag from word 0 and dispatch through a jump table, binding
    /// each variant's fields by loading them from the payload (MARV-9).
    fn eval_match(&mut self, scrutinee: &Atom, branches: &[Branch]) -> Result<Slot, CodegenError> {
        let scrut_ty = layout::atom_type(self.world, scrutinee, &self.tys);
        let boxed = scrut_ty
            .as_ref()
            .map(|t| layout::is_boxed(self.world, t))
            .unwrap_or(false);
        if boxed {
            return self.eval_match_boxed(scrutinee, branches, &scrut_ty.unwrap());
        }

        // Scalar path: the front end emits only the two-armed `bool` match
        // (`if`/`else`): branch 0 = false, branch 1 = true (`spec/02` §D), with no
        // bound fields. A non-`bool`, non-aggregate scrutinee has no layout here.
        if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
            return Err(CodegenError::Unsupported(
                "match on a value whose layout could not be determined".into(),
            ));
        }
        let cond = self.eval_atom(scrutinee)?;
        let cond = self.as_word(cond)?;

        let then_block = self.builder.create_block();
        let else_block = self.builder.create_block();
        let merge_block = self.builder.create_block();
        self.builder.append_block_param(merge_block, WORD);

        // brif: nonzero condition takes the `then` (true) edge.
        self.builder
            .ins()
            .brif(cond, then_block, &[], else_block, &[]);

        // true branch (tag 1)
        self.builder.switch_to_block(then_block);
        self.builder.seal_block(then_block);
        let tv = self.eval(&branches[1].body)?;
        let tv = self.as_word(tv)?;
        self.builder.ins().jump(merge_block, &[tv.into()]);

        // false branch (tag 0)
        self.builder.switch_to_block(else_block);
        self.builder.seal_block(else_block);
        let ev = self.eval(&branches[0].body)?;
        let ev = self.as_word(ev)?;
        self.builder.ins().jump(merge_block, &[ev.into()]);

        self.builder.switch_to_block(merge_block);
        self.builder.seal_block(merge_block);
        let result = self.builder.block_params(merge_block)[0];
        Ok(Slot::Val(result))
    }

    /// The runtime enum/struct `Match` (MARV-9): box the scrutinee to a pointer,
    /// load the tag from word 0, and `br_table` over the branch blocks. Each arm
    /// binds its variant's fields by loading `[i + 1]` from the payload, then
    /// evaluates its body; all arms converge on a merge block carrying the result.
    fn eval_match_boxed(
        &mut self,
        scrutinee: &Atom,
        branches: &[Branch],
        scrut_ty: &Type,
    ) -> Result<Slot, CodegenError> {
        let scrut = self.eval_atom(scrutinee)?;
        let ptr = self.as_word(scrut)?;
        let tag64 = self.builder.ins().load(WORD, MemFlags::trusted(), ptr, 0);
        // `br_table` dispatches on an `i32` index; the tag fits a variant count.
        let tag = self.builder.ins().ireduce(types::I32, tag64);

        let merge = self.builder.create_block();
        self.builder.append_block_param(merge, WORD);
        let arm_blocks: Vec<_> = branches
            .iter()
            .map(|_| self.builder.create_block())
            .collect();
        let default_block = self.builder.create_block();

        // Jump table on the tag; an out-of-range tag (impossible for an
        // exhaustively-checked match) lands on the trapping default.
        let arm_calls: Vec<_> = arm_blocks
            .iter()
            .map(|b| self.builder.func.dfg.block_call(*b, &[]))
            .collect();
        let default_call = self.builder.func.dfg.block_call(default_block, &[]);
        let jt = self
            .builder
            .create_jump_table(JumpTableData::new(default_call, &arm_calls));
        self.builder.ins().br_table(tag, jt);

        // Default: unreachable for a well-checked program.
        self.builder.switch_to_block(default_block);
        self.builder.seal_block(default_block);
        self.builder.ins().trap(TrapCode::unwrap_user(1));

        for (t, br) in branches.iter().enumerate() {
            self.builder.switch_to_block(arm_blocks[t]);
            self.builder.seal_block(arm_blocks[t]);
            // Bind the variant's fields from the payload (`[i + 1]`), recording
            // their types so a nested match over a field stays well-formed.
            let field_tys =
                layout::variant_fields(self.world, scrut_ty, t as u32).unwrap_or_default();
            let pushed = br.binds as usize;
            for i in 0..pushed {
                let off = (i as i32 + 1) * SLOT;
                let v = self.builder.ins().load(WORD, MemFlags::trusted(), ptr, off);
                self.env.push(Slot::Val(v));
                self.tys.push(field_tys.get(i).cloned());
            }
            let body = self.eval(&br.body)?;
            let w = self.as_word(body)?;
            for _ in 0..pushed {
                self.env.pop();
                self.tys.pop();
            }
            self.builder.ins().jump(merge, &[w.into()]);
        }

        self.builder.switch_to_block(merge);
        self.builder.seal_block(merge);
        let result = self.builder.block_params(merge)[0];
        Ok(Slot::Val(result))
    }

    /// Coerce a slot to a machine word: a real value passes through, a unit
    /// becomes the zero word, an aggregate is **boxed** to the heap (MARV-9), and
    /// a partial application is a compile error (a function used where a value is
    /// required).
    fn as_word(&mut self, s: Slot) -> Result<Value, CodegenError> {
        match s {
            Slot::Val(v) => Ok(v),
            Slot::Unit => Ok(self.builder.ins().iconst(WORD, 0)),
            Slot::Partial { .. } => Err(CodegenError::Unsupported(
                "a partially-applied function used as a value".into(),
            )),
            Slot::Tuple { tag, fields } => self.box_tuple(tag, fields),
        }
    }

    /// Box an aggregate into a fresh `[tag, field_0, …]` heap block and return the
    /// pointer (MARV-9). Nested aggregate fields are boxed recursively.
    fn box_tuple(&mut self, tag: u32, fields: Vec<Slot>) -> Result<Value, CodegenError> {
        let n = fields.len();
        let base = self.alloc((n + 1) as i64);
        let tagv = self.builder.ins().iconst(WORD, tag as i64);
        self.builder.ins().store(MemFlags::trusted(), tagv, base, 0);
        for (i, f) in fields.into_iter().enumerate() {
            let w = self.as_word(f)?;
            let off = (i as i32 + 1) * SLOT;
            self.builder.ins().store(MemFlags::trusted(), w, base, off);
        }
        Ok(base)
    }

    /// Emit a call to the host allocator for a compile-time-constant `n_words`
    /// slots, returning the pointer.
    fn alloc(&mut self, n_words: i64) -> Value {
        let n = self.builder.ins().iconst(WORD, n_words);
        self.alloc_dyn(n)
    }

    /// Emit a call to the host allocator for a **runtime** word count `n_words`,
    /// returning the pointer (MARV-33: a slice store allocates a block whose size
    /// is only known at run time). The allocator's func-ref is declared lazily,
    /// once per function.
    fn alloc_dyn(&mut self, n_words: Value) -> Value {
        let aref = match self.alloc_ref {
            Some(r) => r,
            None => {
                let r = self
                    .module
                    .declare_func_in_func(self.alloc_id, self.builder.func);
                self.alloc_ref = Some(r);
                r
            }
        };
        let call = self.builder.ins().call(aref, &[n_words]);
        self.builder.inst_results(call)[0]
    }
}

// ============================ free helpers ===============================

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
