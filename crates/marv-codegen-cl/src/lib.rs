//! # marv-codegen-cl — Cranelift backend (milestone M4)
//!
//! Compiles the canonical **Core IR** (`marv-core`) to native code with
//! Cranelift, JIT-compiling each definition so the result can be called
//! in-process. It is the second backend behind the tree-walking interpreter
//! (`marv-interp`); the M4 acceptance gate is that the two **agree** on a corpus
//! of programs (`spec/01` §9 — "same Core IR feeds both").
//!
//! ## What it compiles
//!
//! The integer/boolean core that the M0/M1 front end can express and lower:
//! arithmetic and comparison [`PrimOp`]s, `if`/`else` (a two-branch `bool`
//! [`Core::Match`]), `let` bindings, and curried calls between top-level
//! functions (recursion included). Every scalar lives in a 64-bit register, so
//! the backend's wrapping arithmetic matches the oracle's `i64` semantics — the
//! property that keeps the differential test meaningful.
//!
//! Constructs with no surface form yet (aggregates with runtime layout,
//! capability `perform`, first-class closures, floats) return
//! [`CodegenError::Unsupported`] rather than emitting wrong code. They are added
//! to *both* backends together so agreement is preserved by construction.
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

use cranelift::codegen::ir::Type as ClType;
use cranelift::codegen::settings::{self, Configurable};
// Explicit imports (not a glob) so cranelift's `Type` does not collide with
// `marv_core::ir::Type`, which we glob-import below as the Core type model.
use cranelift::prelude::{
    types, AbiParam, FunctionBuilder, FunctionBuilderContext, InstBuilder, IntCC, Value,
};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module, ModuleError};

use marv_core::ir::*;
use marv_core::symbol_hash;

/// Every scalar the backend handles is a machine word; using one width keeps the
/// backend's wrapping arithmetic identical to the interpreter's `i64` oracle.
const WORD: ClType = types::I64;

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
/// calls and recursion bind correctly.
pub fn compile(module_path: &str, defs: &[(String, Def)]) -> Result<JitProgram, CodegenError> {
    let mut module = make_module()?;

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
        compile_fn(&mut module, &metas, &mut ctx, &mut fb_ctx, *h, def)?;
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

/// Build a fresh JIT module configured for the host machine.
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
    let builder = JITBuilder::with_isa(isa, default_libcall_names());
    Ok(JITModule::new(builder))
}

/// Compile one function definition into the module.
fn compile_fn(
    module: &mut JITModule,
    metas: &HashMap<Hash, FnMeta>,
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
        // parameter, in order.
        let block_params: Vec<Value> = builder.block_params(entry_block).to_vec();
        let mut env: Vec<Slot> = Vec::with_capacity(meta.arity);
        let mut abi_i = 0usize;
        for is_unit in &meta.param_is_unit {
            if *is_unit {
                env.push(Slot::Unit);
            } else {
                env.push(Slot::Val(block_params[abi_i]));
                abi_i += 1;
            }
        }

        // Scope the translator so its `&mut builder` borrow ends before
        // `finalize` (which consumes the builder).
        {
            let mut trans = Trans {
                module,
                metas,
                builder: &mut builder,
                env,
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

/// A compile-time value: a real machine word, a (zero-sized) unit, or a
/// partially-applied call accumulating its arguments (the currying mirror).
#[derive(Clone)]
enum Slot {
    Val(Value),
    Unit,
    Partial { func: Hash, got: Vec<Slot> },
}

/// The per-function translation state.
struct Trans<'a, 'b> {
    module: &'a mut JITModule,
    metas: &'a HashMap<Hash, FnMeta>,
    builder: &'a mut FunctionBuilder<'b>,
    /// Environment indexed by de Bruijn *level* (`env[0]` = outermost binder).
    env: Vec<Slot>,
}

impl Trans<'_, '_> {
    fn eval(&mut self, c: &Core) -> Result<Slot, CodegenError> {
        match c {
            Core::Atom(a) => self.eval_atom(a),

            Core::Let { value, body } => {
                let v = self.eval(value)?;
                self.env.push(v);
                let r = self.eval(body);
                self.env.pop();
                r
            }

            Core::App { func, arg } => {
                let f = self.eval_atom(func)?;
                let a = self.eval_atom(arg)?;
                self.apply(f, a)
            }

            Core::Prim { op, args } => self.eval_prim(*op, args),

            Core::Match {
                scrutinee,
                branches,
            } => self.eval_match(scrutinee, branches),

            Core::Lam { .. } => Err(CodegenError::Unsupported("first-class lambda".into())),
            Core::Ctor { .. } => Err(CodegenError::Unsupported("aggregate construction".into())),
            Core::Proj { .. } => Err(CodegenError::Unsupported("field projection".into())),
            Core::Perform { .. } => Err(CodegenError::Unsupported(
                "capability perform (use the interpreter)".into(),
            )),
            Core::Raise { .. } => Err(CodegenError::Unsupported("raise".into())),
            Core::Loop { .. } => Err(CodegenError::Unsupported("loop".into())),
        }
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
            Literal::Char(_) => Err(CodegenError::Unsupported("char literal".into())),
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
        // Saturated: drop unit arguments, lower the rest to words, and call.
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
            Len | Index => {
                return Err(CodegenError::Unsupported(
                    "len/index (no aggregate layout yet)".into(),
                ))
            }
        };
        Ok(Slot::Val(out))
    }

    /// Emit a comparison whose `i8` result is zero-extended to a machine word so
    /// booleans share the integer representation everywhere.
    fn cmp(&mut self, cc: IntCC, a: Value, b: Value) -> Value {
        let c = self.builder.ins().icmp(cc, a, b);
        self.builder.ins().uextend(WORD, c)
    }

    fn eval_match(&mut self, scrutinee: &Atom, branches: &[Branch]) -> Result<Slot, CodegenError> {
        // The front end emits only the two-armed `bool` match (`if`/`else`):
        // branch 0 = false, branch 1 = true (`spec/02` §D), with no bound
        // fields. Anything else needs aggregate/enum support not present yet.
        if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
            return Err(CodegenError::Unsupported(
                "match other than a two-arm boolean `if`/`else`".into(),
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

    /// Coerce a slot to a machine word: a real value passes through, a unit
    /// becomes the zero word, and a partial application is a compile error
    /// (a function used where a value is required).
    fn as_word(&mut self, s: Slot) -> Result<Value, CodegenError> {
        match s {
            Slot::Val(v) => Ok(v),
            Slot::Unit => Ok(self.builder.ins().iconst(WORD, 0)),
            Slot::Partial { .. } => Err(CodegenError::Unsupported(
                "a partially-applied function used as a value".into(),
            )),
        }
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
