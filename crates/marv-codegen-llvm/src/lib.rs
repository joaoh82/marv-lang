//! # marv-codegen-llvm — LLVM release backend (MARV-69)
//!
//! This crate emits deterministic textual LLVM IR for the same content-hashed
//! Core IR consumed by the interpreter, Cranelift, and WASM backends. The first
//! release slice intentionally uses the system `clang` driver as the LLVM
//! optimizer/linker instead of binding to `llvm-sys`: that keeps the backend
//! usable on hosts where `clang` is installed but `llvm-config` is not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use marv_core::ir::*;
use marv_core::{symbol_hash, Hash};
use marv_types::{layout, World};

/// Codegen options for the LLVM release backend.
#[derive(Debug, Clone)]
pub struct Options {
    /// Emit Tier-1 runtime bounds checks for array/slice indexing.
    pub bounds_checks: bool,
    /// Ask LLVM/clang to optimize generated IR.
    pub optimize: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bounds_checks: true,
            optimize: true,
        }
    }
}

/// A backend failure. These are never type errors; `marv build` runs the checker
/// before the backend receives Core.
#[derive(Debug, Clone)]
pub enum LlvmError {
    Unsupported(String),
    UnknownGlobal(Hash),
    NoSuchEntry(String),
    ArgCount { expected: usize, got: usize },
    Backend(String),
}

impl std::fmt::Display for LlvmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlvmError::Unsupported(d) => write!(f, "llvm backend: unsupported: {d}"),
            LlvmError::UnknownGlobal(h) => write!(f, "llvm backend: unknown global {}", h.to_b3()),
            LlvmError::NoSuchEntry(e) => write!(f, "llvm backend: no entry point `{e}`"),
            LlvmError::ArgCount { expected, got } => write!(
                f,
                "llvm backend: entry expects {expected} argument(s), got {got}"
            ),
            LlvmError::Backend(m) => write!(f, "llvm backend: {m}"),
        }
    }
}

impl std::error::Error for LlvmError {}

#[derive(Debug, Clone)]
pub struct LlvmProgram {
    pub ir: String,
    pub entry_symbol: String,
    pub entry_arity: usize,
    optimize: bool,
}

impl LlvmProgram {
    pub fn entry_arity(&self) -> usize {
        self.entry_arity
    }

    pub fn link_executable(&self, out: impl AsRef<Path>) -> Result<(), LlvmError> {
        let out = out.as_ref();
        let tmp = unique_temp_dir("marv-llvm")?;
        let ir = tmp.join("module.ll");
        let runtime = tmp.join("runtime.c");
        std::fs::write(&ir, &self.ir).map_err(|e| LlvmError::Backend(e.to_string()))?;
        std::fs::write(&runtime, native_runtime_c(self))
            .map_err(|e| LlvmError::Backend(e.to_string()))?;

        let mut cmd = Command::new("clang");
        cmd.arg(&ir).arg(&runtime).arg("-o").arg(out);
        cmd.arg("-Wno-override-module");
        if self.optimize {
            cmd.arg("-O2");
        }
        let status = cmd.status().map_err(|e| {
            LlvmError::Backend(format!(
                "failed to invoke clang for LLVM executable link: {e}"
            ))
        })?;
        let cleanup = std::fs::remove_dir_all(&tmp);
        if !status.success() {
            return Err(LlvmError::Backend(format!(
                "clang failed while linking native LLVM executable `{}` with status {status}",
                out.display()
            )));
        }
        cleanup.map_err(|e| LlvmError::Backend(format!("cleanup {}: {e}", tmp.display())))?;
        Ok(())
    }

    pub fn run_i64(&self, args: &[i64]) -> Result<i64, LlvmError> {
        if args.len() != self.entry_arity {
            return Err(LlvmError::ArgCount {
                expected: self.entry_arity,
                got: args.len(),
            });
        }
        let tmp = unique_temp_dir("marv-llvm-run")?;
        let exe = tmp.join(if cfg!(windows) { "run.exe" } else { "run" });
        self.link_executable(&exe)?;
        let output = Command::new(&exe)
            .args(args.iter().map(i64::to_string))
            .output()
            .map_err(|e| LlvmError::Backend(format!("failed to execute {}: {e}", exe.display())))?;
        let cleanup = std::fs::remove_dir_all(&tmp);
        if !output.status.success() {
            return Err(LlvmError::Backend(format!(
                "LLVM executable exited with status {}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        cleanup.map_err(|e| LlvmError::Backend(format!("cleanup {}: {e}", tmp.display())))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.trim().parse::<i64>().map_err(|e| {
            LlvmError::Backend(format!(
                "LLVM executable printed non-integer `{stdout}`: {e}"
            ))
        })
    }
}

#[derive(Clone)]
struct FnMeta {
    symbol: String,
    arity: usize,
    param_is_unit: Vec<bool>,
    param_tys: Vec<Type>,
}

impl FnMeta {
    fn abi_param_count(&self) -> usize {
        self.param_is_unit.iter().filter(|u| !**u).count()
    }
}

pub fn compile_reachable(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
    opts: &Options,
    entry: &str,
) -> Result<LlvmProgram, LlvmError> {
    let hashed: Vec<_> = defs
        .iter()
        .map(|(name, def)| {
            let qualified = if module_path.is_empty() {
                name.clone()
            } else {
                format!("{module_path}.{name}")
            };
            (symbol_hash(&qualified), qualified, def.clone())
        })
        .collect();
    let aliases: Vec<_> = defs
        .iter()
        .map(|(name, _)| {
            let qualified = if module_path.is_empty() {
                name.clone()
            } else {
                format!("{module_path}.{name}")
            };
            (name.clone(), symbol_hash(&qualified))
        })
        .collect();
    compile_hashed_reachable(&hashed, &aliases, world, opts, entry)
}

pub fn compile_hashed_reachable(
    defs: &[(Hash, String, Def)],
    aliases: &[(String, Hash)],
    world: &World,
    opts: &Options,
    entry: &str,
) -> Result<LlvmProgram, LlvmError> {
    let mask = hashed_reachable_mask(defs, aliases, entry);
    let mut metas = HashMap::new();
    let mut names = HashMap::new();
    let mut order = Vec::new();

    for (idx, (h, name, def)) in defs.iter().enumerate() {
        if def.kind != DefKind::Fn || def.ty.is_polymorphic() || !mask[idx] {
            continue;
        }
        let param_tys = peel_param_types(&def.ty);
        let param_is_unit = param_tys.iter().map(|t| is_no_slot(t, world)).collect();
        metas.insert(
            *h,
            FnMeta {
                symbol: hashed_symbol_name(h),
                arity: param_tys.len(),
                param_is_unit,
                param_tys,
            },
        );
        names.insert(name.clone(), *h);
        names.insert(h.to_b3(), *h);
        order.push((*h, idx));
    }
    for (alias, h) in aliases {
        names.insert(alias.clone(), *h);
    }

    let entry_hash = resolve_entry_from_maps(&metas, &names, entry)?;
    let entry_symbol = metas[&entry_hash].symbol.clone();
    let entry_arity = metas[&entry_hash].abi_param_count();

    let mut module = ModuleIr::new();
    for (h, idx) in order {
        let (_, _, def) = &defs[idx];
        compile_fn(&mut module, &metas, world, opts, h, def)?;
    }

    Ok(LlvmProgram {
        ir: module.finish(),
        entry_symbol,
        entry_arity,
        optimize: opts.optimize,
    })
}

struct ModuleIr {
    funcs: Vec<String>,
}

impl ModuleIr {
    fn new() -> Self {
        Self { funcs: Vec::new() }
    }

    fn finish(self) -> String {
        let mut out = String::new();
        out.push_str("; marv LLVM backend (MARV-69)\n");
        out.push_str("declare ptr @calloc(i64, i64)\n");
        out.push_str("declare void @abort()\n\n");
        for func in self.funcs {
            out.push_str(&func);
            out.push('\n');
        }
        out
    }
}

fn compile_fn(
    module: &mut ModuleIr,
    metas: &HashMap<Hash, FnMeta>,
    world: &World,
    opts: &Options,
    h: Hash,
    def: &Def,
) -> Result<(), LlvmError> {
    let meta = metas[&h].clone();
    let body = def
        .body
        .as_ref()
        .ok_or_else(|| LlvmError::Unsupported("function without a body".into()))?;
    let inner = peel_lams(body);
    let params = (0..meta.abi_param_count())
        .map(|i| format!("i64 %a{i}"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut b = FuncBuilder::new(metas, world, opts);
    b.push_label("entry".to_string());
    let mut abi_i = 0usize;
    for (i, is_unit) in meta.param_is_unit.iter().enumerate() {
        if *is_unit {
            b.env.push(Slot::Unit);
        } else {
            b.env.push(Slot::Val(format!("%a{abi_i}")));
            abi_i += 1;
        }
        b.tys.push(meta.param_tys.get(i).cloned());
    }
    let result = b.eval(inner)?;
    if !b.terminated {
        let ret = b.as_word(result)?;
        b.emit(format!("ret i64 {ret}"));
        b.terminated = true;
    }
    let body = b.finish();
    module.funcs.push(format!(
        "define i64 @{}({params}) {{\n{body}}}\n",
        meta.symbol
    ));
    Ok(())
}

struct FuncBuilder<'a> {
    metas: &'a HashMap<Hash, FnMeta>,
    world: &'a World,
    opts: &'a Options,
    lines: Vec<String>,
    env: Vec<Slot>,
    tys: Vec<Option<Type>>,
    tmp: usize,
    label: usize,
    current_label: String,
    terminated: bool,
}

impl<'a> FuncBuilder<'a> {
    fn new(metas: &'a HashMap<Hash, FnMeta>, world: &'a World, opts: &'a Options) -> Self {
        Self {
            metas,
            world,
            opts,
            lines: Vec::new(),
            env: Vec::new(),
            tys: Vec::new(),
            tmp: 0,
            label: 0,
            current_label: String::new(),
            terminated: false,
        }
    }

    fn finish(self) -> String {
        self.lines.join("\n")
    }

    fn emit(&mut self, line: impl Into<String>) {
        self.lines.push(format!("  {}", line.into()));
    }

    fn push_label(&mut self, label: String) {
        self.lines.push(format!("{label}:"));
        self.current_label = label;
        self.terminated = false;
    }

    fn tmp(&mut self) -> String {
        let t = format!("%t{}", self.tmp);
        self.tmp += 1;
        t
    }

    fn label(&mut self, prefix: &str) -> String {
        let l = format!("{prefix}.{}", self.label);
        self.label += 1;
        l
    }

    fn eval(&mut self, c: &Core) -> Result<Slot, LlvmError> {
        match c {
            Core::Atom(a) => self.eval_atom(a),
            Core::Let { value, body } => {
                let v = self.eval(value)?;
                if matches!(v, Slot::Returned) {
                    return Ok(Slot::Returned);
                }
                let t = layout::type_of(self.world, value, &mut self.tys);
                self.env.push(v);
                self.tys.push(t);
                let out = self.eval(body);
                self.env.pop();
                self.tys.pop();
                out
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
                let mut slots = Vec::with_capacity(fields.len());
                for f in fields {
                    slots.push(self.eval_atom(f)?);
                }
                Ok(Slot::Tuple {
                    tag: *tag,
                    fields: slots,
                })
            }
            Core::Array { items, .. } => {
                let mut slots = Vec::with_capacity(items.len());
                for item in items {
                    slots.push(self.eval_atom(item)?);
                }
                Ok(Slot::Tuple {
                    tag: items.len() as u32,
                    fields: slots,
                })
            }
            Core::Proj { base, idx } => self.eval_proj(base, *idx),
            Core::IndexSet { base, index, value } => self.eval_index_set(base, index, value),
            Core::Loop {
                state, cond, body, ..
            } => self.eval_loop(state, cond, body),
            Core::Ref { of, .. } => self.eval_atom(of),
            Core::Return { value } => {
                let v = self.eval_atom(value)?;
                let v = self.as_word(v)?;
                self.emit(format!("ret i64 {v}"));
                self.terminated = true;
                Ok(Slot::Returned)
            }
            Core::ListNew { .. }
            | Core::ListPush { .. }
            | Core::ListPop { .. }
            | Core::ListSet { .. } => Err(LlvmError::Unsupported(
                "std.collections.List runtime operations".into(),
            )),
            Core::Lam { .. } => Err(LlvmError::Unsupported("first-class lambda".into())),
            Core::Perform { .. } => Err(LlvmError::Unsupported(
                "capability perform (use the interpreter or WASM host imports)".into(),
            )),
            Core::Raise { .. } => Err(LlvmError::Unsupported("raise".into())),
        }
    }

    fn eval_atom(&mut self, a: &Atom) -> Result<Slot, LlvmError> {
        match a {
            Atom::Lit(l) => self.lit(l),
            Atom::Var(idx) => {
                let d = self.env.len();
                let i = (*idx as usize) + 1;
                if i > d {
                    return Err(LlvmError::Unsupported(format!(
                        "de Bruijn index {idx} out of scope at depth {d}"
                    )));
                }
                Ok(self.env[d - i].clone())
            }
            Atom::Global(h) if self.metas.contains_key(h) => Ok(Slot::Partial {
                func: *h,
                got: Vec::new(),
            }),
            Atom::Global(h) => Err(LlvmError::UnknownGlobal(*h)),
        }
    }

    fn lit(&mut self, l: &Literal) -> Result<Slot, LlvmError> {
        match l {
            Literal::Unit => Ok(Slot::Unit),
            Literal::Bool(b) => Ok(Slot::Val((*b as i64).to_string())),
            Literal::Int(n) => Ok(Slot::Val(n.to_string())),
            Literal::Char(c) => Ok(Slot::Val((*c as i64).to_string())),
            Literal::Str(s) => {
                let fields = s
                    .chars()
                    .map(|c| Slot::Val((c as i64).to_string()))
                    .collect();
                Ok(Slot::Tuple {
                    tag: s.chars().count() as u32,
                    fields,
                })
            }
            Literal::Float(_) => Err(LlvmError::Unsupported("float literal".into())),
        }
    }

    fn apply(&mut self, f: Slot, arg: Slot) -> Result<Slot, LlvmError> {
        let (func, mut got) = match f {
            Slot::Partial { func, got } => (func, got),
            _ => {
                return Err(LlvmError::Unsupported(
                    "application of a non-function".into(),
                ))
            }
        };
        got.push(arg);
        let meta = self
            .metas
            .get(&func)
            .ok_or(LlvmError::UnknownGlobal(func))?;
        if got.len() < meta.arity {
            return Ok(Slot::Partial { func, got });
        }
        let mut call_args = Vec::new();
        for (slot, is_unit) in got.into_iter().zip(meta.param_is_unit.iter()) {
            if !is_unit {
                call_args.push(self.as_word(slot)?);
            }
        }
        let tmp = self.tmp();
        self.emit(format!(
            "{tmp} = call i64 @{}({})",
            meta.symbol,
            call_args
                .iter()
                .map(|a| format!("i64 {a}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        Ok(Slot::Val(tmp))
    }

    fn eval_prim(&mut self, op: PrimOp, args: &[Atom]) -> Result<Slot, LlvmError> {
        use PrimOp::*;
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            let s = self.eval_atom(a)?;
            vals.push(self.as_word(s)?);
        }
        let v = |i: usize| vals[i].clone();
        let out = match op {
            Add => self.bin("add", &v(0), &v(1)),
            Sub => self.bin("sub", &v(0), &v(1)),
            Mul => self.bin("mul", &v(0), &v(1)),
            Div => self.bin("sdiv", &v(0), &v(1)),
            Rem => self.bin("srem", &v(0), &v(1)),
            Eq => self.cmp("eq", &v(0), &v(1)),
            Ne => self.cmp("ne", &v(0), &v(1)),
            Lt => self.cmp("slt", &v(0), &v(1)),
            Le => self.cmp("sle", &v(0), &v(1)),
            Gt => self.cmp("sgt", &v(0), &v(1)),
            Ge => self.cmp("sge", &v(0), &v(1)),
            And => self.bin("and", &v(0), &v(1)),
            Or => self.bin("or", &v(0), &v(1)),
            Not => self.bin("xor", &v(0), "1"),
            Neg => self.bin("sub", "0", &v(0)),
            Len => self.load_word(&v(0), "0"),
            Index => {
                self.emit_bounds_check(&v(0), &v(1));
                let slot = self.bin("add", &v(1), "1");
                self.load_word(&v(0), &slot)
            }
            Slice | FromChars => {
                return Err(LlvmError::Unsupported("string slice/from_chars".into()))
            }
        };
        Ok(Slot::Val(out))
    }

    fn eval_cast(&mut self, value: &Atom, to: &Type) -> Result<Slot, LlvmError> {
        let v = self.eval_atom(value)?;
        let v = self.as_word(v)?;
        match to {
            Type::Bool => Ok(Slot::Val(self.cmp("ne", &v, "0"))),
            Type::Char | Type::Int(IntTy::I64 | IntTy::Isize | IntTy::U64 | IntTy::Usize) => {
                Ok(Slot::Val(v))
            }
            Type::Int(IntTy::I8) => Ok(Slot::Val(self.trunc_ext(&v, 8, true))),
            Type::Int(IntTy::I16) => Ok(Slot::Val(self.trunc_ext(&v, 16, true))),
            Type::Int(IntTy::I32) => Ok(Slot::Val(self.trunc_ext(&v, 32, true))),
            Type::Int(IntTy::U8) => Ok(Slot::Val(self.trunc_ext(&v, 8, false))),
            Type::Int(IntTy::U16) => Ok(Slot::Val(self.trunc_ext(&v, 16, false))),
            Type::Int(IntTy::U32) => Ok(Slot::Val(self.trunc_ext(&v, 32, false))),
            other => Err(LlvmError::Unsupported(format!("cast to {other:?}"))),
        }
    }

    fn eval_proj(&mut self, base: &Atom, idx: u32) -> Result<Slot, LlvmError> {
        let base_slot = self.eval_atom(base)?;
        match base_slot {
            Slot::Tuple { fields, .. } => fields
                .get(idx as usize)
                .cloned()
                .ok_or_else(|| LlvmError::Unsupported("projection out of range".into())),
            other => {
                let ptr = self.as_word(other)?;
                Ok(Slot::Val(
                    self.load_word(&ptr, &(idx as i64 + 1).to_string()),
                ))
            }
        }
    }

    fn eval_index_set(
        &mut self,
        base: &Atom,
        index: &Atom,
        value: &Atom,
    ) -> Result<Slot, LlvmError> {
        let ptr = self.eval_atom(base)?;
        let ptr = self.as_word(ptr)?;
        let idx = self.eval_atom(index)?;
        let idx = self.as_word(idx)?;
        let val = self.eval_atom(value)?;
        let val = self.as_word(val)?;
        self.emit_bounds_check(&ptr, &idx);
        let len = self.load_word(&ptr, "0");
        let total = self.bin("add", &len, "1");
        let newptr = self.alloc_words(&total);
        self.copy_words(&ptr, &newptr, &total);
        let slot = self.bin("add", &idx, "1");
        self.store_word(&newptr, &slot, &val);
        Ok(Slot::Val(newptr))
    }

    fn eval_match(&mut self, scrutinee: &Atom, branches: &[Branch]) -> Result<Slot, LlvmError> {
        let scrut_ty = layout::atom_type(self.world, scrutinee, &self.tys);
        let boxed = scrut_ty
            .as_ref()
            .map(|t| layout::is_boxed(self.world, t))
            .unwrap_or(false);
        if boxed {
            self.eval_match_boxed(scrutinee, branches)
        } else {
            self.eval_match_scalar(scrutinee, branches)
        }
    }

    fn eval_match_scalar(
        &mut self,
        scrutinee: &Atom,
        branches: &[Branch],
    ) -> Result<Slot, LlvmError> {
        if branches.len() != 2 || branches.iter().any(|b| b.binds != 0) {
            return Err(LlvmError::Unsupported(
                "scalar match other than bool/if".into(),
            ));
        }
        let cond = self.eval_atom(scrutinee)?;
        let cond = self.as_word(cond)?;
        let cond = self.i1(&cond);
        let then_label = self.label("if.then");
        let else_label = self.label("if.else");
        let merge_label = self.label("if.end");
        self.emit(format!(
            "br i1 {cond}, label %{then_label}, label %{else_label}"
        ));
        self.terminated = true;

        self.push_label(then_label.clone());
        let then_slot = self.eval(&branches[1].body)?;
        let then_end = self.current_label.clone();
        let then_live = !self.terminated;
        if then_live {
            self.emit(format!("br label %{merge_label}"));
            self.terminated = true;
        }

        self.push_label(else_label.clone());
        let else_slot = self.eval(&branches[0].body)?;
        let else_end = self.current_label.clone();
        let else_live = !self.terminated;
        if else_live {
            self.emit(format!("br label %{merge_label}"));
            self.terminated = true;
        }

        self.merge_slots(
            merge_label,
            vec![
                (then_slot, then_end, then_live),
                (else_slot, else_end, else_live),
            ],
        )
    }

    fn eval_match_boxed(
        &mut self,
        scrutinee: &Atom,
        branches: &[Branch],
    ) -> Result<Slot, LlvmError> {
        let ptr = self.eval_atom(scrutinee)?;
        let ptr = self.as_word(ptr)?;
        let tag = self.load_word(&ptr, "0");
        let merge_label = self.label("match.end");
        let default_label = self.label("match.default");
        let arm_labels = (0..branches.len())
            .map(|_| self.label("match.arm"))
            .collect::<Vec<_>>();
        let cases = arm_labels
            .iter()
            .enumerate()
            .map(|(i, label)| format!("i64 {i}, label %{label}"))
            .collect::<Vec<_>>()
            .join("\n    ");
        self.emit(format!(
            "switch i64 {tag}, label %{default_label} [\n    {cases}\n  ]"
        ));
        self.terminated = true;

        self.push_label(default_label);
        self.emit("call void @abort()");
        self.emit("unreachable");
        self.terminated = true;

        let mut arms = Vec::new();
        for (i, br) in branches.iter().enumerate() {
            self.push_label(arm_labels[i].clone());
            let pushed = br.binds as usize;
            for field_i in 0..pushed {
                let slot = self.load_word(&ptr, &(field_i as i64 + 1).to_string());
                self.env.push(Slot::Val(slot));
                self.tys.push(None);
            }
            let slot = self.eval(&br.body)?;
            for _ in 0..pushed {
                self.env.pop();
                self.tys.pop();
            }
            let end = self.current_label.clone();
            let live = !self.terminated;
            if live {
                self.emit(format!("br label %{merge_label}"));
                self.terminated = true;
            }
            arms.push((slot, end, live));
        }
        self.merge_slots(merge_label, arms)
    }

    fn merge_slots(
        &mut self,
        merge_label: String,
        arms: Vec<(Slot, String, bool)>,
    ) -> Result<Slot, LlvmError> {
        let live: Vec<_> = arms.into_iter().filter(|(_, _, live)| *live).collect();
        if live.is_empty() {
            return Ok(Slot::Returned);
        }
        self.push_label(merge_label);
        if live.len() == 1 {
            return Ok(live[0].0.clone());
        }
        match &live[0].0 {
            Slot::Val(_) | Slot::Unit => {
                let incoming = live
                    .into_iter()
                    .map(|(slot, label, _)| match slot {
                        Slot::Val(v) => Ok(format!("[ {v}, %{label} ]")),
                        Slot::Unit => Ok(format!("[ 0, %{label} ]")),
                        other => Err(LlvmError::Unsupported(format!(
                            "cannot merge branch result {other:?}"
                        ))),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ");
                let tmp = self.tmp();
                self.emit(format!("{tmp} = phi i64 {incoming}"));
                Ok(Slot::Val(tmp))
            }
            Slot::Tuple { fields, .. } => {
                let mut merged = Vec::with_capacity(fields.len());
                for field_i in 0..fields.len() {
                    let incoming = live
                        .iter()
                        .map(|(slot, label, _)| {
                            let Slot::Tuple { fields, .. } = slot else {
                                return Err(LlvmError::Unsupported(
                                    "branch tuple/non-tuple mismatch".into(),
                                ));
                            };
                            let v = self.as_word(fields[field_i].clone())?;
                            Ok(format!("[ {v}, %{label} ]"))
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .join(", ");
                    let tmp = self.tmp();
                    self.emit(format!("{tmp} = phi i64 {incoming}"));
                    merged.push(Slot::Val(tmp));
                }
                let tag = match &live[0].0 {
                    Slot::Tuple { tag, .. } => *tag,
                    _ => 0,
                };
                Ok(Slot::Tuple {
                    tag,
                    fields: merged,
                })
            }
            other => Err(LlvmError::Unsupported(format!(
                "cannot merge branch result {other:?}"
            ))),
        }
    }

    fn eval_loop(&mut self, state: &[Atom], cond: &Core, body: &Core) -> Result<Slot, LlvmError> {
        let mut slots = Vec::new();
        let mut carried_tys = Vec::new();
        for a in state {
            let init = self.eval_atom(a)?;
            let init = self.as_word(init)?;
            let slot = self.tmp();
            self.emit(format!("{slot} = alloca i64"));
            self.emit(format!("store i64 {init}, ptr {slot}"));
            slots.push(slot);
            carried_tys.push(layout::atom_type(self.world, a, &self.tys));
        }
        let header = self.label("loop.header");
        let body_label = self.label("loop.body");
        let exit = self.label("loop.exit");
        self.emit(format!("br label %{header}"));
        self.terminated = true;

        self.push_label(header.clone());
        for (slot, ty) in slots.iter().zip(&carried_tys) {
            let cur = self.tmp();
            self.emit(format!("{cur} = load i64, ptr {slot}"));
            self.env.push(Slot::Val(cur));
            self.tys.push(ty.clone());
        }
        let c = self.eval(cond)?;
        let c = self.as_word(c)?;
        let c = self.i1(&c);
        self.emit(format!("br i1 {c}, label %{body_label}, label %{exit}"));
        self.terminated = true;

        self.push_label(body_label);
        let next = self.eval_loop_body(body, state.len())?;
        for _ in 0..state.len() {
            self.env.pop();
            self.tys.pop();
        }
        if !self.terminated {
            for (slot, value) in slots.iter().zip(next) {
                let v = self.as_word(value)?;
                self.emit(format!("store i64 {v}, ptr {slot}"));
            }
            self.emit(format!("br label %{header}"));
            self.terminated = true;
        }

        self.push_label(exit);
        let finals = slots
            .iter()
            .map(|slot| {
                let cur = self.tmp();
                self.emit(format!("{cur} = load i64, ptr {slot}"));
                Slot::Val(cur)
            })
            .collect();
        Ok(Slot::Tuple {
            tag: 0,
            fields: finals,
        })
    }

    fn eval_loop_body(&mut self, body: &Core, k: usize) -> Result<Vec<Slot>, LlvmError> {
        let slot = self.eval(body)?;
        match slot {
            Slot::Tuple { fields, .. } if fields.len() == k => Ok(fields),
            Slot::Returned => Ok(Vec::new()),
            other => Err(LlvmError::Unsupported(format!(
                "loop body did not produce {k} carried value(s): {other:?}"
            ))),
        }
    }

    fn as_word(&mut self, slot: Slot) -> Result<String, LlvmError> {
        match slot {
            Slot::Val(v) => Ok(v),
            Slot::Unit => Ok("0".to_string()),
            Slot::Tuple { tag, fields } => {
                let total = (fields.len() + 1).to_string();
                let ptr = self.alloc_words(&total);
                self.store_word(&ptr, "0", &tag.to_string());
                for (i, field) in fields.into_iter().enumerate() {
                    let v = self.as_word(field)?;
                    self.store_word(&ptr, &(i + 1).to_string(), &v);
                }
                Ok(ptr)
            }
            Slot::Returned => Err(LlvmError::Unsupported("returned value used".into())),
            Slot::Partial { .. } => Err(LlvmError::Unsupported(
                "partially applied function escaped".into(),
            )),
        }
    }

    fn alloc_words(&mut self, words: &str) -> String {
        let raw = self.tmp();
        self.emit(format!("{raw} = call ptr @calloc(i64 {words}, i64 8)"));
        let ptr = self.tmp();
        self.emit(format!("{ptr} = ptrtoint ptr {raw} to i64"));
        ptr
    }

    fn load_word(&mut self, base: &str, index: &str) -> String {
        let raw = self.tmp();
        self.emit(format!("{raw} = inttoptr i64 {base} to ptr"));
        let addr = self.tmp();
        self.emit(format!(
            "{addr} = getelementptr i64, ptr {raw}, i64 {index}"
        ));
        let out = self.tmp();
        self.emit(format!("{out} = load i64, ptr {addr}"));
        out
    }

    fn store_word(&mut self, base: &str, index: &str, value: &str) {
        let raw = self.tmp();
        self.emit(format!("{raw} = inttoptr i64 {base} to ptr"));
        let addr = self.tmp();
        self.emit(format!(
            "{addr} = getelementptr i64, ptr {raw}, i64 {index}"
        ));
        self.emit(format!("store i64 {value}, ptr {addr}"));
    }

    fn copy_words(&mut self, src: &str, dst: &str, total: &str) {
        let slot = self.tmp();
        self.emit(format!("{slot} = alloca i64"));
        self.emit(format!("store i64 0, ptr {slot}"));
        let header = self.label("copy.header");
        let body = self.label("copy.body");
        let exit = self.label("copy.exit");
        self.emit(format!("br label %{header}"));
        self.terminated = true;

        self.push_label(header.clone());
        let k = self.tmp();
        self.emit(format!("{k} = load i64, ptr {slot}"));
        let more = self.cmp("ult", &k, total);
        let more = self.i1(&more);
        self.emit(format!("br i1 {more}, label %{body}, label %{exit}"));
        self.terminated = true;

        self.push_label(body);
        let word = self.load_word(src, &k);
        self.store_word(dst, &k, &word);
        let next = self.bin("add", &k, "1");
        self.emit(format!("store i64 {next}, ptr {slot}"));
        self.emit(format!("br label %{header}"));
        self.terminated = true;

        self.push_label(exit);
    }

    fn emit_bounds_check(&mut self, base: &str, index: &str) {
        if !self.opts.bounds_checks {
            return;
        }
        let len = self.load_word(base, "0");
        let ok = self.cmp("ult", index, &len);
        let ok = self.i1(&ok);
        let pass = self.label("bounds.pass");
        let fail = self.label("bounds.fail");
        self.emit(format!("br i1 {ok}, label %{pass}, label %{fail}"));
        self.terminated = true;
        self.push_label(fail);
        self.emit("call void @abort()");
        self.emit("unreachable");
        self.terminated = true;
        self.push_label(pass);
    }

    fn bin(&mut self, op: &str, a: &str, b: &str) -> String {
        let tmp = self.tmp();
        self.emit(format!("{tmp} = {op} i64 {a}, {b}"));
        tmp
    }

    fn cmp(&mut self, pred: &str, a: &str, b: &str) -> String {
        let c = self.tmp();
        self.emit(format!("{c} = icmp {pred} i64 {a}, {b}"));
        let z = self.tmp();
        self.emit(format!("{z} = zext i1 {c} to i64"));
        z
    }

    fn i1(&mut self, v: &str) -> String {
        let tmp = self.tmp();
        self.emit(format!("{tmp} = icmp ne i64 {v}, 0"));
        tmp
    }

    fn trunc_ext(&mut self, v: &str, bits: u32, signed: bool) -> String {
        let t = self.tmp();
        self.emit(format!("{t} = trunc i64 {v} to i{bits}"));
        let e = self.tmp();
        let op = if signed { "sext" } else { "zext" };
        self.emit(format!("{e} = {op} i{bits} {t} to i64"));
        e
    }
}

#[derive(Debug, Clone)]
enum Slot {
    Val(String),
    Unit,
    Returned,
    Partial { func: Hash, got: Vec<Slot> },
    Tuple { tag: u32, fields: Vec<Slot> },
}

fn native_runtime_c(program: &LlvmProgram) -> String {
    let params = (0..program.entry_arity)
        .map(|i| format!("int64_t a{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let args = (0..program.entry_arity)
        .map(|i| format!("argv_i64[{i}]"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

extern int64_t {entry}({params});

static int parse_i64(const char *s, int64_t *out) {{
    errno = 0;
    char *end = 0;
    long long v = strtoll(s, &end, 10);
    if (errno || !end || *end != 0) return 0;
    *out = (int64_t)v;
    return 1;
}}

int main(int argc, char **argv) {{
    if (argc - 1 != {arity}) {{
        fprintf(stderr, "marv: entry expects {arity} integer argument(s), got %d\n", argc - 1);
        return 2;
    }}
    int64_t argv_i64[8] = {{0, 0, 0, 0, 0, 0, 0, 0}};
    for (int i = 0; i < argc - 1; i++) {{
        if (i >= 8 || !parse_i64(argv[i + 1], &argv_i64[i])) {{
            fprintf(stderr, "marv: argument %d `%s` is not an integer\n", i, argv[i + 1]);
            return 2;
        }}
    }}
    int64_t result = {entry}({args});
    printf("%lld\n", (long long)result);
    return 0;
}}
"#,
        entry = program.entry_symbol,
        arity = program.entry_arity,
    )
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf, LlvmError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| LlvmError::Backend(format!("system clock before epoch: {e}")))?
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| LlvmError::Backend(format!("{}: {e}", dir.display())))?;
    Ok(dir)
}

fn resolve_entry_from_maps(
    metas: &HashMap<Hash, FnMeta>,
    names: &HashMap<String, Hash>,
    entry: &str,
) -> Result<Hash, LlvmError> {
    if !entry.is_empty() {
        return names
            .get(entry)
            .copied()
            .filter(|h| metas.contains_key(h))
            .ok_or_else(|| LlvmError::NoSuchEntry(entry.to_string()));
    }
    if let Some(h) = names.get("main").copied().filter(|h| metas.contains_key(h)) {
        return Ok(h);
    }
    let all = metas.keys().copied().collect::<Vec<_>>();
    match all.as_slice() {
        [h] => Ok(*h),
        _ => Err(LlvmError::NoSuchEntry("main".to_string())),
    }
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

fn peel_lams(mut c: &Core) -> &Core {
    while let Core::Lam { body, .. } = c {
        c = body;
    }
    c
}

fn peel_param_types(mut ty: &Type) -> Vec<Type> {
    let mut params = Vec::new();
    while let Type::Arrow { param, ret, .. } = ty {
        params.push((**param).clone());
        ty = ret;
    }
    params
}

fn is_no_slot(t: &Type, world: &World) -> bool {
    match t {
        Type::Unit => true,
        Type::Nominal { def, .. } => world.is_cap(def),
        _ => false,
    }
}

fn hashed_symbol_name(h: &Hash) -> String {
    format!("marv_b3_{}", h.to_hex())
}
