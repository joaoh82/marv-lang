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
    BinOp, Block, Else, Expr, Field, FnDecl, IfExpr, Item, Module, Stmt, StructDecl, Tail,
    Type as SType,
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
        }
    }
}

impl std::error::Error for LowerError {}

/// One lowered top-level definition, paired with its content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefEntry {
    /// The definition's *source* name. Not part of the hash — renaming a `Def`
    /// does not change its identity (`spec/02` §F).
    pub name: String,
    pub def: Def,
    pub hash: Hash,
}

/// A whole module lowered to Core: its definitions in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredModule {
    pub module: Vec<String>,
    pub defs: Vec<DefEntry>,
}

/// Lower a parsed module to Core, hashing each definition.
pub fn lower_module(m: &Module) -> Result<LoweredModule, LowerError> {
    let lw = Lowerer::new(m);
    let mut defs = Vec::with_capacity(m.items.len());
    for item in &m.items {
        let (name, def) = match item {
            Item::Fn(f) => (f.name.clone(), lw.lower_fn(f)?),
            Item::Struct(s) => (s.name.clone(), lw.lower_struct(s)),
        };
        let hash = def.content_hash();
        defs.push(DefEntry { name, def, hash });
    }
    Ok(LoweredModule {
        module: m.name.clone(),
        defs,
    })
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
}

impl Lowerer {
    fn new(m: &Module) -> Self {
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
            }
        }
        Lowerer {
            module_path: m.name.join("."),
            local_items,
            structs,
            fn_rets,
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
        let field_tys: Vec<Type> = s.fields.iter().map(|f| self.lower_type(&f.ty)).collect();
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

    fn lower_fn(&self, f: &FnDecl) -> Result<Def, LowerError> {
        // A nullary function is curried application over a single `()` param, so
        // synthesize one (unnamed, hence never referenced) when there are none.
        let synth_unit = f.params.is_empty();
        let param_ctys: Vec<Type> = if synth_unit {
            vec![Type::Unit]
        } else {
            f.params.iter().map(|p| self.lower_type(&p.ty)).collect()
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
            .map(|t| self.lower_type(t))
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
        Ok(Def {
            kind: DefKind::Fn,
            ty: arrow,
            requires: Vec::new(),
            ensures: Vec::new(),
            body: Some(body),
        })
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

    // ---- expressions ----------------------------------------------------

    /// Lower `e` to an **atom**, hoisting any compound computation into a `let`
    /// recorded in `b`. Atomic expressions add no binding.
    fn emit_atom(&self, e: &Expr, env: &[Binding], b: &mut Builder) -> Result<Atom, LowerError> {
        match e {
            Expr::Unit => Ok(Atom::Lit(Literal::Unit)),
            Expr::Int(n) => Ok(Atom::Lit(Literal::Int(*n))),
            Expr::Bool(v) => Ok(Atom::Lit(Literal::Bool(*v))),
            Expr::Str(s) => Ok(Atom::Lit(Literal::Str(s.clone()))),
            Expr::Var(name) => Ok(self.resolve_var(name, env)),
            Expr::Binary(l, op, r) => {
                let al = self.emit_atom(l, env, b)?;
                let ar = self.emit_atom(r, env, b)?;
                Ok(b.push(Core::Prim {
                    op: prim_op(*op),
                    args: vec![al, ar],
                }))
            }
            Expr::Field(base, field) => {
                let ab = self.emit_atom(base, env, b)?;
                let idx = self.resolve_proj(base, field, env)?;
                Ok(b.push(Core::Proj { base: ab, idx }))
            }
            Expr::Call(callee, args) => {
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
                let ab = self.emit_atom(base, env, b)?;
                let idx = self.resolve_proj(base, field, env)?;
                Ok(Core::Proj { base: ab, idx })
            }
            Expr::Call(callee, args) => {
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

    /// A bare identifier resolves to a local binding's atom, or — if unbound — to
    /// a content-addressed [`Atom::Global`] keyed on its (module-qualified) name.
    fn resolve_var(&self, name: &str, env: &[Binding]) -> Atom {
        if let Some(binding) = env.iter().rev().find(|x| x.name == name) {
            binding.atom.clone()
        } else {
            Atom::Global(symbol_hash(&self.qualify_value(name)))
        }
    }

    // ---- types & projection --------------------------------------------

    fn lower_type(&self, t: &SType) -> Type {
        match t {
            SType::Unit => Type::Unit,
            SType::Named(path) => self.lower_named(path),
            SType::Slice(inner) => Type::Slice(Box::new(self.lower_type(inner))),
            SType::Ref { mutable, inner } => Type::Ref {
                mutable: *mutable,
                of: Box::new(self.lower_type(inner)),
            },
        }
    }

    fn lower_named(&self, path: &[String]) -> Type {
        if path.len() == 1 {
            if let Some(builtin) = builtin_type(&path[0]) {
                return builtin;
            }
            let qualified = if self.structs.contains_key(&path[0]) {
                format!("{}.{}", self.module_path, path[0])
            } else {
                path[0].clone()
            };
            return Type::Nominal {
                def: symbol_hash(&qualified),
                args: Vec::new(),
            };
        }
        Type::Nominal {
            def: symbol_hash(&path.join(".")),
            args: Vec::new(),
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
