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
//! The same integer/boolean subset the Cranelift backend handles — arithmetic
//! and comparison [`PrimOp`]s, `if`/`else` (a two-arm `bool` [`Core::Match`]),
//! `let`, curried cross-function calls and recursion — plus [`Core::Perform`]
//! lowered to a host-import call. Every scalar is an `i64`, matching the oracle's
//! semantics, so the differential test is meaningful. Constructs with no surface
//! form yet (aggregate layout, first-class closures, floats, string-typed
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
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, Module, TypeSection, ValType,
};

use marv_core::ir::*;
use marv_core::symbol_hash;
use marv_types::World;

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
    // ---- gather functions, in definition order --------------------------
    let fns: Vec<(Hash, &str, &Def)> = defs
        .iter()
        .filter(|(_, d)| d.kind == DefKind::Fn)
        .map(|(name, d)| (symbol_hash(&qualify(module_path, name)), name.as_str(), d))
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
        let func = compile_fn(*h, def, world, &metas, &fn_index, &import_index)?;
        code_sec.function(&func);
    }

    let mut module = Module::new();
    module
        .section(&types)
        .section(&import_sec)
        .section(&func_sec)
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
                        Type::Int(_) | Type::Bool => params += 1,
                        Type::Unit => {}
                        _ => {
                            return Err(WasmError::Unsupported(format!(
                                "capability operand type `{}` (only scalar i64/bool/unit operands \
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
        Core::Loop { cond, body, .. } => {
            walk_caps(cond, tys, world, f)?;
            walk_caps(body, tys, world, f)?;
        }
        Core::Perform { cap, op, .. } => {
            let def = resolve_cap_def(cap, tys, world).ok_or(WasmError::UnresolvedCapability)?;
            f(def, op.0)?;
        }
        // Atom / App / Ctor / Proj / Prim / Raise carry only atomic children.
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
    /// A partially-applied function (compile-time only): nothing pushed.
    Partial { func: Hash, args: Vec<ArgVal> },
}

/// An argument resolved to something re-emittable at the saturating call site,
/// independent of de Bruijn depth.
#[derive(Clone)]
enum ArgVal {
    Local(u32),
    Const(i64),
    Unit,
}

/// A binding in scope: an absolute local, a unit, or a pending partial call.
#[derive(Clone)]
enum Slot {
    Local(u32),
    Unit,
    Partial { func: Hash, args: Vec<ArgVal> },
}

fn compile_fn(
    h: Hash,
    def: &Def,
    world: &World,
    metas: &HashMap<Hash, FnMeta>,
    fn_index: &HashMap<Hash, u32>,
    import_index: &HashMap<(String, u32), u32>,
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
        insts: Vec::new(),
        env,
        tys,
        next_local: n_params,
    };
    let out = t.eval(inner)?;
    t.coerce_to_word(out)?;

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
                    Out::Partial { func, args } => Slot::Partial { func, args },
                };
                self.env.push(slot);
                self.tys.push(None);
                let r = self.eval(body);
                self.env.pop();
                self.tys.pop();
                r
            }

            Core::App { func, arg } => self.eval_app(func, arg),

            Core::Prim { op, args } => self.eval_prim(*op, args),

            Core::Match {
                scrutinee,
                branches,
            } => self.eval_match(scrutinee, branches),

            Core::Perform { cap, op, args } => self.eval_perform(cap, *op, args),

            Core::Lam { .. } => Err(WasmError::Unsupported("first-class lambda".into())),
            Core::Ctor { .. } => Err(WasmError::Unsupported("aggregate construction".into())),
            Core::Proj { .. } => Err(WasmError::Unsupported("field projection".into())),
            Core::Raise { .. } => Err(WasmError::Unsupported("raise".into())),
            Core::Loop { .. } => Err(WasmError::Unsupported("loop".into())),
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
            Atom::Lit(Literal::Str(_)) => Err(WasmError::Unsupported("string literal".into())),
            Atom::Lit(Literal::Char(_)) => Err(WasmError::Unsupported("char literal".into())),
            Atom::Var(idx) => match self.slot(*idx)? {
                Slot::Local(l) => {
                    self.emit(Instruction::LocalGet(l));
                    Ok(Out::Stack)
                }
                Slot::Unit => Ok(Out::Unit),
                Slot::Partial { func, args } => Ok(Out::Partial { func, args }),
            },
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
            Atom::Lit(Literal::Unit) => Ok(ArgVal::Unit),
            Atom::Var(idx) => match self.slot(*idx)? {
                Slot::Local(l) => Ok(ArgVal::Local(l)),
                Slot::Unit => Ok(ArgVal::Unit),
                Slot::Partial { .. } => Err(WasmError::Unsupported(
                    "passing a partially-applied function as an argument".into(),
                )),
            },
            Atom::Lit(_) => Err(WasmError::Unsupported("non-scalar literal argument".into())),
            Atom::Global(_) => Err(WasmError::Unsupported(
                "passing a function as a value argument".into(),
            )),
        }
    }

    /// Push a resolved argument (unit arguments occupy no slot).
    fn push_argval(&mut self, av: &ArgVal) {
        match av {
            ArgVal::Local(l) => self.emit(Instruction::LocalGet(*l)),
            ArgVal::Const(n) => self.emit(Instruction::I64Const(*n)),
            ArgVal::Unit => {}
        }
    }

    fn eval_prim(&mut self, op: PrimOp, args: &[Atom]) -> Result<Out, WasmError> {
        use PrimOp::*;
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
            Len | Index => {
                return Err(WasmError::Unsupported(
                    "len/index (no aggregate layout yet)".into(),
                ))
            }
        }
        Ok(Out::Stack)
    }

    /// A wasm comparison yields `i32`; widen it to the uniform `i64` boolean.
    fn cmp(&mut self, op: Instruction<'static>) {
        self.emit(op);
        self.emit(Instruction::I64ExtendI32U);
    }

    fn eval_match(&mut self, scrutinee: &Atom, branches: &[Branch]) -> Result<Out, WasmError> {
        if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
            return Err(WasmError::Unsupported(
                "match other than a two-arm boolean `if`/`else`".into(),
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

    /// Ensure the term's value is exactly one `i64` on the stack (a unit becomes
    /// the zero word; a partial application is a compile error).
    fn coerce_to_word(&mut self, out: Out) -> Result<(), WasmError> {
        match out {
            Out::Stack => Ok(()),
            Out::Unit => {
                self.emit(Instruction::I64Const(0));
                Ok(())
            }
            Out::Partial { .. } => Err(WasmError::Unsupported(
                "a partially-applied function used as a value".into(),
            )),
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
