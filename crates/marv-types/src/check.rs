//! The M2 checker over the Core IR (`spec/02` §E, `spec/01` §§3–7).
//!
//! One demand-driven pass over a definition's Core body
//! ([`Checker::synth`]) simultaneously performs all six families of static
//! check the milestone requires, because they share a single traversal and a
//! single typing environment:
//!
//! 1. **Type checking** — bidirectional synthesis of every node's [`Type`]
//!    against the §E judgments `(Var)`, `(Global)`, `(App)`, `(Let)`,
//!    `(Match)`, plus the total [`PrimOp`] table.
//! 2. **Effect-row inference** — every node yields the [`EffectRow`] it may
//!    exercise; `(App)` folds in the callee arrow's row, `(Perform)` adds its
//!    capability, `(Raise)`/op-signatures add errors. The union over the body
//!    is the inferred row.
//! 3. **Capability checking** — a `Perform`'s capability must be a capability
//!    *value in scope* (no ambient authority) and must not have been *forged*
//!    by construction (`spec/01` §5).
//! 4. **Error-set inference** — the inferred errors must be a subset of the
//!    declared error set; any missing one is reported with a fix.
//! 5. **Second-class references** — a `Ref` may be passed down but never stored
//!    in an aggregate field, returned, or declared as a struct field
//!    (`spec/01` §4).
//! 6. **Linearity** — every `linear` binding is used *exactly once on every
//!    control path*, tracked as per-binder `(min, max)` use counts that compose
//!    across `Let` sequencing and `Match` branches (`spec/02` §E).
//!
//! Effect/error **subsumption** (the declared signature row must be a superset
//! of the inferred body row) and the return-position reference check run once
//! per definition in [`Checker::check_def`], after the body is synthesized.
//!
//! ## What the front end can reach today
//!
//! The M0/M1 front end emits only `fn`/`struct` over arithmetic, `if`, calls and
//! field access — no `Perform`, `Raise`, enum `Ctor`, or `linear` consumption,
//! and every lowered arrow currently carries the empty row. So from real `.mv`
//! source the reachable diagnostics are the type/return-reference/struct-field
//! ones; the capability, error-set, exhaustiveness, and linear-consumption rules
//! are exercised over hand-written Core (see `tests/rules.rs`). The checker
//! itself is complete over the whole Core IR regardless of which surface forms
//! exist yet.

// The synthesis methods take `&mut Vec<Binder>` and push/pop the typing
// environment as they descend through binders, so a `&mut [Binder]` slice (what
// `clippy::ptr_arg` would suggest) genuinely will not do.
#![allow(clippy::ptr_arg)]

use std::collections::BTreeMap;

use std::sync::OnceLock;

use marv_core::ir::*;
use marv_core::symbol_hash;

use crate::diagnostic::{Code, Diagnostic, Edit, Fix};
use crate::world::World;

/// Per-binder linear use profile across all control paths reaching a point:
/// `level → (min_uses, max_uses)`. `min` is the fewest uses on any path, `max`
/// the most. Only `linear` binders are tracked. See the module docs.
type Uses = BTreeMap<u32, (u32, u32)>;

/// A checker value type: a concrete Core [`Type`], an unconstrained integer
/// literal (compatible with any width), or `Unknown` — an opaque/unresolved
/// type (e.g. an imported global the [`World`] does not know) that is compatible
/// with everything, so it neither produces nor masks real errors downstream.
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Known(Type),
    IntLit,
    Unknown,
}

/// How a binder entered scope — used only to detect a *forged* capability
/// (`spec/01` §5): one produced by `Ctor`/`Prim` rather than received or
/// narrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prov {
    /// A function parameter or `match`-bound field: power that was *received*.
    Received,
    /// Bound to a `Ctor`/`Prim` result: constructed, hence forgeable.
    Computed,
    /// Any other `let` value (an application, a projection, …).
    Other,
}

/// One binding in the typing environment, indexed by de Bruijn *level*
/// (`env[0]` is the outermost binder). A `Var(idx)` at depth `d` refers to
/// `env[d - 1 - idx]`.
#[derive(Debug, Clone)]
struct Binder {
    ty: Ty,
    linear: bool,
    prov: Prov,
}

/// The result of synthesizing a Core term: its type, the effect row it may
/// exercise, and the linear-use profile of the binders in scope.
struct Out {
    ty: Ty,
    eff: EffectRow,
    uses: Uses,
}

/// Check a single definition against `world`, returning its diagnostics in a
/// deterministic order. `name` is used only to make messages friendlier.
pub fn check_def(world: &World, def: &Def, name: Option<&str>) -> Vec<Diagnostic> {
    let mut c = Checker {
        world,
        diags: Vec::new(),
    };
    c.check_def(def, name);
    c.diags
}

/// The **inferred** effect row of a function definition's body — the union of
/// every capability it performs and every error it can raise, regardless of what
/// its signature declares (`spec/02` §E effect/error inference).
///
/// This is the value the declared row is checked *against* in
/// [`check_def`]: a `MissingCapability`/`MissingError` diagnostic fires exactly
/// when this row is not a subset of the declared one. The query server uses it
/// for `marv/effects` and `marv/errorSet` (so they report what a body *actually*
/// exercises, not merely what it declares), and `marv/applyFix` uses it to
/// synthesize the repaired declaration. A non-`Fn` def, or a `Fn` with no body,
/// has the empty row.
pub fn effect_row(world: &World, def: &Def) -> EffectRow {
    if def.kind != DefKind::Fn {
        return EffectRow::empty();
    }
    let body = match &def.body {
        Some(b) => b,
        None => return EffectRow::empty(),
    };
    let mut c = Checker {
        world,
        diags: Vec::new(),
    };
    // Peel the curried arrow/lambda spine exactly as `check_fn` does, descending
    // into the innermost body before synthesizing its effect row.
    let mut env: Vec<Binder> = Vec::new();
    let mut cur_ty = &def.ty;
    let mut cur_body = body;
    while let (
        Type::Arrow { ret, .. },
        Core::Lam {
            param, body: lbody, ..
        },
    ) = (cur_ty, cur_body)
    {
        let linear = matches!(param, Type::Linear(_));
        env.push(Binder {
            ty: Ty::Known(param.clone()),
            linear,
            prov: Prov::Received,
        });
        cur_ty = ret;
        cur_body = lbody;
    }
    c.synth(cur_body, &mut env).eff
}

/// The checker state for one definition.
struct Checker<'a> {
    world: &'a World,
    diags: Vec<Diagnostic>,
}

impl<'a> Checker<'a> {
    fn emit(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    // ---- definition entry point ----------------------------------------

    fn check_def(&mut self, def: &Def, name: Option<&str>) {
        match def.kind {
            DefKind::Struct => self.check_struct(def),
            DefKind::Fn => self.check_fn(def, name),
            // Other kinds carry no body the M2 checker inspects (enums/caps/
            // errors are declarations; consts/impls/interfaces arrive with the
            // surface forms that introduce them).
            _ => {}
        }
    }

    /// A `struct` may not declare a field of reference type: storing a
    /// second-class reference in an aggregate lets it escape its call frame
    /// (`spec/01` §4).
    fn check_struct(&mut self, def: &Def) {
        let (fields, _) = peel_struct(&def.ty);
        for (i, f) in fields.iter().enumerate() {
            if let Type::Ref { .. } = peel_ref_target(f) {
                self.emit(escaping_ref_diag(EscapeSite::StructField(i)));
            }
        }
    }

    fn check_fn(&mut self, def: &Def, _name: Option<&str>) {
        let body = match &def.body {
            Some(b) => b,
            None => return,
        };

        // Peel the curried arrow/lambda spine in lockstep to recover the
        // parameter types, the innermost return type, and the innermost declared
        // effect row (outer, partial-application arrows are always pure).
        let mut env: Vec<Binder> = Vec::new();
        let mut cur_ty = &def.ty;
        let mut cur_body = body;
        let mut declared_eff = EffectRow::empty();
        while let (
            Type::Arrow { ret, effects, .. },
            Core::Lam {
                param, body: lbody, ..
            },
        ) = (cur_ty, cur_body)
        {
            let linear = matches!(param, Type::Linear(_));
            env.push(Binder {
                ty: Ty::Known(param.clone()),
                linear,
                prov: Prov::Received,
            });
            declared_eff = effects.clone();
            cur_ty = ret;
            cur_body = lbody;
        }
        let declared_ret = cur_ty;

        let out = self.synth(cur_body, &mut env);

        // Return-type check (§E: the body's type is the return type).
        if !compatible(&Ty::Known(declared_ret.clone()), &out.ty) {
            self.emit(
                Diagnostic::error(
                    Code::TypeMismatch,
                    format!(
                        "function body has type `{}` but its signature declares `{}`",
                        show_ty(&out.ty),
                        show_type(declared_ret)
                    ),
                )
                .with_related("return type declared here"),
            );
        }

        // Returned reference (§4): a reference in return position escapes.
        if let Type::Ref { .. } = peel_ref_target(declared_ret) {
            self.emit(escaping_ref_diag(EscapeSite::Return));
        }

        // Effect & error subsumption (§E): declared row ⊇ inferred row. A `!T`
        // (error-union) return type means the error set is *inferred*, not
        // listed in the signature (`spec/01` §6), so it accepts any inferred
        // error — no `MissingError`. A plain return type does not, so raising
        // without declaring `!` is still reported.
        let errors_open = error_union_success(declared_ret).is_some();
        self.check_subsumption(&declared_eff, &out.eff, errors_open);

        // Linearity of parameters: each `linear` parameter must be consumed
        // exactly once on every path through the body.
        let nparams = env.len() as u32;
        for level in 0..nparams {
            if env[level as usize].linear {
                let profile = out.uses.get(&level).copied().unwrap_or((0, 0));
                self.check_linear(profile);
            }
        }
    }

    /// Report each capability and error the body infers but the signature does
    /// not declare, with the mechanical fix (`spec/03` §2).
    fn check_subsumption(&mut self, declared: &EffectRow, inferred: &EffectRow, errors_open: bool) {
        // A held capability authorizes everything it can be *narrowed* to
        // (`spec/01` §5), so subsumption is checked against the narrowing closure
        // of the declared row, not just its literal entries.
        let authorized = self.world.authorized_caps(&declared.caps);
        for cap in &inferred.caps {
            if !authorized.contains(cap) {
                let name = self.world.cap_name(cap);
                let param = lowercase_first(&name);
                self.emit(
                    Diagnostic::error(
                        Code::MissingCapability,
                        format!(
                            "this function exercises capability `{name}` but its signature \
                             declares no `{name}`"
                        ),
                    )
                    .with_related(format!(
                        "`{name}` first required by a `perform` in the body"
                    ))
                    .with_fix(Fix::new(
                        format!("add capability parameter `{param}: {name}`"),
                        Edit::insert(format!("{param}: {name}, ")),
                        0.9,
                    )),
                );
            }
        }
        for err in &inferred.errors {
            if !errors_open && !declared.errors.contains(err) {
                let name = self.world.error_name(err);
                self.emit(
                    Diagnostic::error(
                        Code::MissingError,
                        format!(
                            "this function can raise `{name}`, which is not in its declared \
                             error set"
                        ),
                    )
                    .with_fix(Fix::new(
                        format!("add `{name}` to the declared error set"),
                        Edit::insert(name.clone()),
                        0.9,
                    )),
                );
            }
        }
    }

    // ---- synthesis ------------------------------------------------------

    /// Synthesize a Core term's type, effect row, and linear-use profile under
    /// `env`. `env` is pushed/popped around the binders the term introduces, so
    /// it is restored to its entry state on return.
    fn synth(&mut self, c: &Core, env: &mut Vec<Binder>) -> Out {
        match c {
            Core::Atom(a) => Out {
                ty: self.atom_ty(a, env),
                eff: EffectRow::empty(),
                uses: atom_uses(a, env),
            },

            Core::Let { value, body } => {
                let vout = self.synth(value, env);
                let linear = is_linear(&vout.ty);
                env.push(Binder {
                    ty: vout.ty.clone(),
                    linear,
                    prov: value_prov(value),
                });
                let level = env.len() as u32 - 1;
                let mut bout = self.synth(body, env);
                if linear {
                    let profile = bout.uses.get(&level).copied().unwrap_or((0, 0));
                    self.check_linear(profile);
                }
                bout.uses.remove(&level);
                env.pop();
                Out {
                    ty: bout.ty,
                    eff: union(vout.eff, &bout.eff),
                    uses: seq(&vout.uses, &bout.uses),
                }
            }

            Core::Lam {
                param,
                effects,
                body,
            } => {
                // A value-level lambda. Its body's inferred effects must be a
                // subset of its declared row, just like a top-level function.
                let linear = matches!(param, Type::Linear(_));
                env.push(Binder {
                    ty: Ty::Known(param.clone()),
                    linear,
                    prov: Prov::Received,
                });
                let level = env.len() as u32 - 1;
                let mut bout = self.synth(body, env);
                // A value lambda carries no `!T` return annotation, so its error
                // set is closed (declared by `effects`); a raise it does not
                // declare is still reported.
                self.check_subsumption(effects, &bout.eff, false);
                if linear {
                    let profile = bout.uses.get(&level).copied().unwrap_or((0, 0));
                    self.check_linear(profile);
                }
                bout.uses.remove(&level);
                env.pop();
                // Defining a lambda exercises no effect itself.
                Out {
                    ty: Ty::Known(Type::Arrow {
                        param: Box::new(param.clone()),
                        ret: Box::new(ty_to_type(&bout.ty)),
                        effects: effects.clone(),
                    }),
                    eff: EffectRow::empty(),
                    uses: bout.uses,
                }
            }

            Core::App { func, arg } => {
                let ft = self.atom_ty(func, env);
                let at = self.atom_ty(arg, env);
                let uses = seq(&atom_uses(func, env), &atom_uses(arg, env));
                match ft {
                    Ty::Unknown => Out {
                        ty: Ty::Unknown,
                        eff: EffectRow::empty(),
                        uses,
                    },
                    Ty::Known(Type::Arrow {
                        param,
                        ret,
                        effects,
                    }) => {
                        if !compatible(&Ty::Known(*param.clone()), &at) {
                            self.emit(Diagnostic::error(
                                Code::TypeMismatch,
                                format!(
                                    "argument has type `{}` but the function expects `{}`",
                                    show_ty(&at),
                                    show_type(&param)
                                ),
                            ));
                        }
                        Out {
                            ty: Ty::Known(*ret),
                            eff: effects,
                            uses,
                        }
                    }
                    other => {
                        self.emit(Diagnostic::error(
                            Code::NotAFunction,
                            format!(
                                "`{}` is not a function and cannot be called",
                                show_ty(&other)
                            ),
                        ));
                        Out {
                            ty: Ty::Unknown,
                            eff: EffectRow::empty(),
                            uses,
                        }
                    }
                }
            }

            Core::Ctor { ty, tag, fields } => self.synth_ctor(ty, *tag, fields, env),

            Core::Array { elem, items } => self.synth_array(elem, items, env),

            Core::IndexSet { base, index, value } => self.synth_index_set(base, index, value, env),

            Core::ListNew {
                elem,
                alloc,
                capacity,
            } => self.synth_list_new(elem, alloc, capacity, env),

            Core::ListPush { alloc, list, value } => self.synth_list_push(alloc, list, value, env),

            Core::ListPop { list } => Out {
                ty: self.atom_ty(list, env),
                eff: EffectRow::empty(),
                uses: atom_uses(list, env),
            },

            Core::ListSet { list, index, value } => self.synth_list_set(list, index, value, env),

            Core::Proj { base, idx } => {
                let bt = self.atom_ty(base, env);
                let ty = self.proj_ty(&bt, *idx);
                Out {
                    ty,
                    eff: EffectRow::empty(),
                    uses: atom_uses(base, env),
                }
            }

            Core::Match {
                scrutinee,
                branches,
            } => self.synth_match(scrutinee, branches, env),

            Core::Prim { op, args } => self.synth_prim(*op, args, env),

            Core::Cast { value, to } => self.synth_cast(value, to, env),

            // `&e` / `&mut e` (`spec/02` §B `unary`): the result is a
            // [`Type::Ref`] over the operand's type. The second-class rules — a
            // reference may not be stored in a field, returned, or captured — are
            // enforced wherever such a `Ref` type later flows (see
            // [`EscapeSite`]); producing one is itself fine. An `Unknown` operand
            // stays `Unknown` so an unresolved referent neither errors nor masks.
            Core::Ref { mutable, of } => {
                let ty = match self.atom_ty(of, env) {
                    Ty::Unknown => Ty::Unknown,
                    other => Ty::Known(Type::Ref {
                        mutable: *mutable,
                        of: Box::new(ty_to_type(&other)),
                    }),
                };
                Out {
                    ty,
                    eff: EffectRow::empty(),
                    uses: atom_uses(of, env),
                }
            }

            Core::Perform { cap, op, args } => self.synth_perform(cap, *op, args, env),

            Core::Raise { error, args } => {
                // Check payload arity/types if the error is known.
                if let Some(decl) = self.world.error(error) {
                    self.check_args(&decl.payload.clone(), args, env, "error payload");
                }
                let mut eff = EffectRow::empty();
                eff.errors.push(*error);
                let mut uses = Uses::new();
                for a in args {
                    uses = seq(&uses, &atom_uses(a, env));
                }
                // `Raise` yields the bottom of the error union: any type.
                Out {
                    ty: Ty::Unknown,
                    eff,
                    uses,
                }
            }

            Core::Loop {
                state, cond, body, ..
            } => {
                // The loop-carried variables enter scope as the innermost binders
                // for `cond`/`body` (`spec/02` §C `Loop`). Synthesize each initial
                // value's type, then push it as a carried binder.
                let mut state_uses = Uses::new();
                let mut carried_tys: Vec<Ty> = Vec::with_capacity(state.len());
                for a in state {
                    let t = self.atom_ty(a, env);
                    state_uses = seq(&state_uses, &atom_uses(a, env));
                    carried_tys.push(t.clone());
                    env.push(Binder {
                        ty: t,
                        linear: false,
                        prov: Prov::Other,
                    });
                }

                let cout = self.synth(cond, env);
                if !compatible(&Ty::Known(Type::Bool), &cout.ty) {
                    self.emit(Diagnostic::error(
                        Code::TypeMismatch,
                        format!(
                            "loop condition has type `{}`, expected `bool`",
                            show_ty(&cout.ty)
                        ),
                    ));
                }
                let bout = self.synth(body, env);

                for _ in state {
                    env.pop();
                }

                // A loop body runs zero-or-more times: a linear value consumed
                // inside it is consumed an unknown number of times. Model that as
                // "0 on some path, ≥2 on another" so both the not-all-paths and
                // duplicated checks fire for any linear use in a loop body. Uses of
                // the carried binders themselves are loop-local — drop them.
                let env_depth = env.len() as u32;
                let body_uses: Uses = bout
                    .uses
                    .into_iter()
                    .filter(|(l, _)| *l < env_depth)
                    .map(|(l, (_, mx))| (l, if mx >= 1 { (0, 2) } else { (0, 0) }))
                    .collect();
                let cond_uses: Uses = cout
                    .uses
                    .into_iter()
                    .filter(|(l, _)| *l < env_depth)
                    .collect();
                // The loop evaluates to the final values of its carried variables,
                // a tuple the enclosing scope projects. Give it a precise `Tuple`
                // type only when every carried element is concretely known; an
                // unconstrained integer literal (e.g. `var sum = 0`) loses its
                // width through the state atom, so fall back to `Unknown` rather
                // than guessing a width and reporting a spurious mismatch.
                let result_ty = if carried_tys.iter().all(|t| matches!(t, Ty::Known(_))) {
                    Ty::Known(Type::Tuple(carried_tys.iter().map(ty_to_type).collect()))
                } else {
                    Ty::Unknown
                };
                Out {
                    ty: result_ty,
                    eff: union(cout.eff, &bout.eff),
                    uses: seq(&state_uses, &seq(&cond_uses, &body_uses)),
                }
            }
        }
    }

    fn synth_ctor(&mut self, ty: &Hash, tag: u32, fields: &[Atom], env: &mut Vec<Binder>) -> Out {
        // Expected field types, if the nominal is a known struct or enum.
        let expected: Option<Vec<Type>> = if let Some(s) = self.world.struct_decl(ty) {
            Some(s.fields.clone())
        } else {
            self.world
                .enum_decl(ty)
                .and_then(|e| e.variants.get(tag as usize))
                .map(|v| v.fields.clone())
        };

        let mut uses = Uses::new();
        for (i, a) in fields.iter().enumerate() {
            let at = self.atom_ty(a, env);
            // Second-class reference may not be stored in an aggregate field.
            if is_ref(&at) {
                self.emit(escaping_ref_diag(EscapeSite::CtorField(i)));
            }
            if let Some(exp) = &expected {
                if let Some(et) = exp.get(i) {
                    if !compatible(&Ty::Known(et.clone()), &at) {
                        self.emit(Diagnostic::error(
                            Code::TypeMismatch,
                            format!(
                                "field {i} has type `{}` but the constructor expects `{}`",
                                show_ty(&at),
                                show_type(et)
                            ),
                        ));
                    }
                }
            }
            uses = seq(&uses, &atom_uses(a, env));
        }

        // A constructed value of a `linear` struct is itself linear.
        let linear_struct = self
            .world
            .struct_decl(ty)
            .map(|s| s.linear)
            .unwrap_or(false);
        let nominal = Type::Nominal {
            def: *ty,
            args: Vec::new(),
        };
        let result = if linear_struct {
            Type::Linear(Box::new(nominal))
        } else {
            nominal
        };
        Out {
            ty: Ty::Known(result),
            eff: EffectRow::empty(),
            uses,
        }
    }

    /// Type an array literal `[e0, e1, …]` ([`Core::Array`]). Every element must
    /// be compatible with the array's declared element type (homogeneity); a
    /// second-class reference may not be stored as an element (it would escape).
    /// The result is the fixed-length array type `[N]elem`.
    fn synth_array(&mut self, elem: &Type, items: &[Atom], env: &mut Vec<Binder>) -> Out {
        let mut uses = Uses::new();
        for (i, a) in items.iter().enumerate() {
            let at = self.atom_ty(a, env);
            if is_ref(&at) {
                self.emit(escaping_ref_diag(EscapeSite::CtorField(i)));
            }
            if !compatible(&Ty::Known(elem.clone()), &at) {
                self.emit(Diagnostic::error(
                    Code::TypeMismatch,
                    format!(
                        "array element {i} has type `{}` but the array's element type is `{}`",
                        show_ty(&at),
                        show_type(elem)
                    ),
                ));
            }
            uses = seq(&uses, &atom_uses(a, env));
        }
        Out {
            ty: Ty::Known(Type::Array(Box::new(elem.clone()), items.len() as u64)),
            eff: EffectRow::empty(),
            uses,
        }
    }

    /// Type a runtime element store `s[i] = e` ([`Core::IndexSet`], MARV-33). The
    /// base must be a slice or array (peeling any reference), the index numeric,
    /// and the value compatible with the element type. A second-class reference
    /// may not be stored as an element (it would escape). The result is the same
    /// collection type as the base — the store rebinds the root to it.
    fn synth_index_set(
        &mut self,
        base: &Atom,
        index: &Atom,
        value: &Atom,
        env: &mut Vec<Binder>,
    ) -> Out {
        let bt = self.atom_ty(base, env);
        let vt = self.atom_ty(value, env);
        let elem = match &bt {
            Ty::Known(t) => match peel(t) {
                Type::Slice(e) | Type::Array(e, _) => Some((**e).clone()),
                _ => {
                    self.emit(Diagnostic::error(
                        Code::BadPrimOperand,
                        "element store requires a slice or array base".to_string(),
                    ));
                    None
                }
            },
            Ty::Unknown => None,
            _ => {
                self.emit(Diagnostic::error(
                    Code::BadPrimOperand,
                    "element store requires a slice or array base".to_string(),
                ));
                None
            }
        };
        if !numeric(&self.atom_ty(index, env)) {
            self.emit(Diagnostic::error(
                Code::BadPrimOperand,
                "element store requires an integer index".to_string(),
            ));
        }
        if is_ref(&vt) {
            self.emit(escaping_ref_diag(EscapeSite::CtorField(0)));
        }
        if let Some(elem) = &elem {
            if !compatible(&Ty::Known(elem.clone()), &vt) {
                self.emit(Diagnostic::error(
                    Code::TypeMismatch,
                    format!(
                        "stored value has type `{}` but the element type is `{}`",
                        show_ty(&vt),
                        show_type(elem)
                    ),
                ));
            }
        }
        let uses = seq(
            &seq(&atom_uses(base, env), &atom_uses(index, env)),
            &atom_uses(value, env),
        );
        Out {
            ty: bt,
            eff: EffectRow::empty(),
            uses,
        }
    }

    fn synth_list_new(
        &mut self,
        elem: &Type,
        alloc: &Atom,
        capacity: &Atom,
        env: &mut Vec<Binder>,
    ) -> Out {
        let (eff, mut uses) = self.alloc_effect(alloc, env);
        if !numeric(&self.atom_ty(capacity, env)) {
            self.emit(Diagnostic::error(
                Code::BadPrimOperand,
                "list capacity must be an integer".to_string(),
            ));
        }
        uses = seq(&uses, &atom_uses(capacity, env));
        Out {
            ty: Ty::Known(list_type(elem.clone())),
            eff,
            uses,
        }
    }

    fn synth_list_push(
        &mut self,
        alloc: &Atom,
        list: &Atom,
        value: &Atom,
        env: &mut Vec<Binder>,
    ) -> Out {
        let lt = self.atom_ty(list, env);
        let vt = self.atom_ty(value, env);
        let elem = match &lt {
            Ty::Known(t) => list_elem_type(t).cloned(),
            Ty::Unknown => None,
            _ => None,
        };
        if elem.is_none() && !matches!(lt, Ty::Unknown) {
            self.emit(Diagnostic::error(
                Code::BadPrimOperand,
                "list push requires a `List[T]` value".to_string(),
            ));
        }
        if let Some(elem) = &elem {
            if !compatible(&Ty::Known(elem.clone()), &vt) {
                self.emit(Diagnostic::error(
                    Code::TypeMismatch,
                    format!(
                        "pushed value has type `{}` but the list element type is `{}`",
                        show_ty(&vt),
                        show_type(elem)
                    ),
                ));
            }
        }
        let (eff, cap_uses) = self.alloc_effect(alloc, env);
        let uses = seq(
            &seq(&cap_uses, &atom_uses(list, env)),
            &atom_uses(value, env),
        );
        Out { ty: lt, eff, uses }
    }

    fn synth_list_set(
        &mut self,
        list: &Atom,
        index: &Atom,
        value: &Atom,
        env: &mut Vec<Binder>,
    ) -> Out {
        let lt = self.atom_ty(list, env);
        let vt = self.atom_ty(value, env);
        let elem = match &lt {
            Ty::Known(t) => list_elem_type(t).cloned(),
            Ty::Unknown => None,
            _ => None,
        };
        if elem.is_none() && !matches!(lt, Ty::Unknown) {
            self.emit(Diagnostic::error(
                Code::BadPrimOperand,
                "list set requires a `List[T]` value".to_string(),
            ));
        }
        if !numeric(&self.atom_ty(index, env)) {
            self.emit(Diagnostic::error(
                Code::BadPrimOperand,
                "list set requires an integer index".to_string(),
            ));
        }
        if let Some(elem) = &elem {
            if !compatible(&Ty::Known(elem.clone()), &vt) {
                self.emit(Diagnostic::error(
                    Code::TypeMismatch,
                    format!(
                        "stored value has type `{}` but the list element type is `{}`",
                        show_ty(&vt),
                        show_type(elem)
                    ),
                ));
            }
        }
        let uses = seq(
            &seq(&atom_uses(list, env), &atom_uses(index, env)),
            &atom_uses(value, env),
        );
        Out {
            ty: lt,
            eff: EffectRow::empty(),
            uses,
        }
    }

    fn alloc_effect(&mut self, alloc: &Atom, env: &mut Vec<Binder>) -> (EffectRow, Uses) {
        let at = self.atom_ty(alloc, env);
        let mut eff = EffectRow::empty();
        match &at {
            Ty::Known(Type::Nominal { def, .. })
                if self.world.is_cap(def) && self.world.cap_name(def) == "Alloc" =>
            {
                eff.caps.push(*def);
            }
            Ty::Unknown => {}
            _ => self.emit(Diagnostic::error(
                Code::BadPrimOperand,
                "list allocation requires an `Alloc` capability".to_string(),
            )),
        }
        (eff, atom_uses(alloc, env))
    }

    fn synth_match(&mut self, scrutinee: &Atom, branches: &[Branch], env: &mut Vec<Binder>) -> Out {
        let st = self.atom_ty(scrutinee, env);
        let s_uses = atom_uses(scrutinee, env);

        // Recover the variant arities/field-types and the total variant count of
        // the scrutinee, so exhaustiveness and branch binders are checked.
        let (variant_count, variant_fields, variant_names): (
            Option<usize>,
            Vec<Vec<Type>>,
            Vec<String>,
        ) = match &st {
            Ty::Known(Type::Bool) => (
                Some(2),
                vec![Vec::new(), Vec::new()],
                vec!["false".into(), "true".into()],
            ),
            Ty::Known(Type::Nominal { def, .. }) => {
                if let Some(e) = self.world.enum_decl(def) {
                    (
                        Some(e.variants.len()),
                        e.variants.iter().map(|v| v.fields.clone()).collect(),
                        e.variants.iter().map(|v| v.name.clone()).collect(),
                    )
                } else {
                    (None, Vec::new(), Vec::new())
                }
            }
            _ => (None, Vec::new(), Vec::new()),
        };

        // Exhaustiveness (§E): a known scrutinee must have a branch per variant.
        if let Some(v) = variant_count {
            if branches.len() < v {
                let missing: Vec<String> =
                    variant_names.iter().skip(branches.len()).cloned().collect();
                let listed = if missing.is_empty() {
                    String::new()
                } else {
                    format!(" (missing: {})", missing.join(", "))
                };
                self.emit(
                    Diagnostic::error(
                        Code::NonExhaustiveMatch,
                        format!("`match` covers {} of {v} variants{listed}", branches.len()),
                    )
                    .with_fix(Fix::new(
                        "add the missing match arm(s)",
                        Edit::insert(missing_arms_text(&missing)),
                        0.85,
                    )),
                );
            }
        }

        // Synthesize each branch under its bound fields; collect the branch's
        // result type, effects, and outer-binder use profile.
        let mut branch_tys: Vec<Ty> = Vec::new();
        let mut branch_eff = EffectRow::empty();
        let mut branch_use_sets: Vec<Uses> = Vec::new();
        for (bi, br) in branches.iter().enumerate() {
            let base = env.len() as u32;
            let fields = variant_fields.get(bi).cloned().unwrap_or_default();
            for k in 0..br.binds {
                let fty = fields
                    .get(k as usize)
                    .cloned()
                    .map(Ty::Known)
                    .unwrap_or(Ty::Unknown);
                let linear = is_linear(&fty);
                env.push(Binder {
                    ty: fty,
                    linear,
                    prov: Prov::Received,
                });
            }
            let mut bout = self.synth(&br.body, env);
            // Branch-local linear fields must be consumed within the branch.
            for k in 0..br.binds {
                let level = base + k;
                if env[level as usize].linear {
                    let profile = bout.uses.get(&level).copied().unwrap_or((0, 0));
                    self.check_linear(profile);
                }
                bout.uses.remove(&level);
            }
            for _ in 0..br.binds {
                env.pop();
            }
            branch_eff = union(branch_eff, &bout.eff);
            branch_tys.push(bout.ty);
            branch_use_sets.push(bout.uses);
        }

        // Result type: branches must agree (the first known type is the join).
        let mut result = Ty::Unknown;
        for t in &branch_tys {
            if matches!(result, Ty::Unknown) {
                result = t.clone();
            } else if !compatible(&result, t) && !matches!(t, Ty::Unknown) {
                self.emit(Diagnostic::error(
                    Code::TypeMismatch,
                    format!(
                        "match arms have incompatible types `{}` and `{}`",
                        show_ty(&result),
                        show_ty(t)
                    ),
                ));
            }
        }

        let merged = branch_merge(&branch_use_sets);
        Out {
            ty: result,
            eff: union(branch_eff, &EffectRow::empty()),
            uses: seq(&s_uses, &merged),
        }
    }

    fn synth_prim(&mut self, op: PrimOp, args: &[Atom], env: &mut Vec<Binder>) -> Out {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.atom_ty(a, env)).collect();
        let mut uses = Uses::new();
        for a in args {
            uses = seq(&uses, &atom_uses(a, env));
        }
        let ty = self.prim_result(op, &arg_tys);
        Out {
            ty,
            eff: EffectRow::empty(),
            uses,
        }
    }

    /// Type the result of a primitive, reporting [`Code::BadPrimOperand`] for
    /// ill-typed operands. `Unknown` operands are permissive (no false errors).
    fn prim_result(&mut self, op: PrimOp, args: &[Ty]) -> Ty {
        use PrimOp::*;
        let bad = |c: &mut Self, what: &str| {
            c.emit(Diagnostic::error(
                Code::BadPrimOperand,
                format!("operator `{}` {what}", prim_name(op)),
            ));
        };
        match op {
            Add | Sub | Mul | Div | Rem => {
                let (l, r) = (arg(args, 0), arg(args, 1));
                if !numeric(l) || !numeric(r) {
                    bad(self, "requires numeric operands");
                    return Ty::Unknown;
                }
                if !compatible(l, r) {
                    bad(self, "requires both operands to have the same type");
                    return Ty::Unknown;
                }
                join_numeric(l, r)
            }
            Lt | Le | Gt | Ge => {
                let (l, r) = (arg(args, 0), arg(args, 1));
                if !numeric(l) || !numeric(r) {
                    bad(self, "requires numeric operands");
                } else if !compatible(l, r) {
                    bad(self, "requires both operands to have the same type");
                }
                Ty::Known(Type::Bool)
            }
            Eq | Ne => {
                let (l, r) = (arg(args, 0), arg(args, 1));
                if !compatible(l, r) {
                    bad(self, "requires both operands to have the same type");
                }
                Ty::Known(Type::Bool)
            }
            And | Or => {
                for i in 0..2 {
                    if !compatible(&Ty::Known(Type::Bool), arg(args, i)) {
                        bad(self, "requires `bool` operands");
                    }
                }
                Ty::Known(Type::Bool)
            }
            Not => {
                if !compatible(&Ty::Known(Type::Bool), arg(args, 0)) {
                    bad(self, "requires a `bool` operand");
                }
                Ty::Known(Type::Bool)
            }
            Neg => {
                let o = arg(args, 0);
                if !numeric(o) {
                    bad(self, "requires a numeric operand");
                    return Ty::Unknown;
                }
                // Negation preserves the operand's (numeric) type; an
                // unconstrained integer literal stays a literal.
                o.clone()
            }
            Len => {
                // The operand may be the collection itself or a second-class
                // reference to it (`&[]T`), so peel references before matching.
                match arg(args, 0) {
                    Ty::Unknown => {}
                    // A `str` is a UTF-8 slice (`spec/01` §3.1), so `len` accepts
                    // it (its byte length) just as the interpreter does.
                    Ty::Known(t)
                        if matches!(peel(t), Type::Slice(_) | Type::Array(_, _) | Type::Str)
                            || list_elem_type(t).is_some() => {}
                    _ => bad(self, "requires a slice, array, list, or `str` operand"),
                }
                Ty::Known(Type::Int(IntTy::Usize))
            }
            Index => {
                // As with `len`, peel any reference to reach the collection.
                let elem = match arg(args, 0) {
                    Ty::Known(t) => match peel(t) {
                        Type::Slice(e) | Type::Array(e, _) => Ty::Known((**e).clone()),
                        _ if list_elem_type(t).is_some() => {
                            Ty::Known(list_elem_type(t).unwrap().clone())
                        }
                        _ => {
                            bad(
                                self,
                                "requires a slice, array, or list as its first operand",
                            );
                            Ty::Unknown
                        }
                    },
                    Ty::Unknown => Ty::Unknown,
                    _ => {
                        bad(
                            self,
                            "requires a slice, array, or list as its first operand",
                        );
                        Ty::Unknown
                    }
                };
                if !numeric(arg(args, 1)) {
                    bad(self, "requires an integer index");
                }
                elem
            }
        }
    }

    /// Type an explicit `as` cast (`spec/01` §3.1). Only scalar↔scalar
    /// conversions are legal (`int`/`float`/`bool`/`char`); anything else is a
    /// [`Code::BadCast`]. An `Unknown` source is permissive (no false error). The
    /// debug-checked narrowing range guard is a Tier-1 *runtime* obligation, so it
    /// is the interpreter/backends' job — the checker only gates legality. The
    /// cast's result type is its declared target.
    fn synth_cast(&mut self, value: &Atom, to: &Type, env: &mut Vec<Binder>) -> Out {
        let from = self.atom_ty(value, env);
        let source_ok = match peel_ty(&from) {
            Ty::Unknown => true,
            Ty::IntLit => true,
            Ty::Known(t) => is_scalar(&t),
        };
        if !source_ok || !is_scalar(to) {
            self.emit(Diagnostic::error(
                Code::BadCast,
                format!(
                    "`as` cast to `{}` requires a scalar operand (`int`/`float`/`bool`/`char`); \
                     there is no conversion from `{}`",
                    show_type(to),
                    show_ty(&from),
                ),
            ));
        } else if let Atom::Lit(Literal::Int(n)) = value {
            // The narrowing range check (`spec/01` §3.1) is a Tier-1 debug
            // obligation; for a *constant* operand it is statically decidable, so
            // a literal that cannot fit its narrowing target is a compile error
            // here (a fuller runtime check awaits width-tracked runtime values).
            let overflows = match to {
                Type::Int(width) => !int_lit_fits(*n, *width),
                Type::Char => char::from_u32(*n as u32).is_none() || *n < 0,
                _ => false,
            };
            if overflows {
                self.emit(Diagnostic::error(
                    Code::BadCast,
                    format!(
                        "the literal `{n}` does not fit in `{}` (narrowing `as` is checked)",
                        show_type(to),
                    ),
                ));
            }
        }
        Out {
            ty: Ty::Known(to.clone()),
            eff: EffectRow::empty(),
            uses: atom_uses(value, env),
        }
    }

    fn synth_perform(&mut self, cap: &Atom, op: OpId, args: &[Atom], env: &mut Vec<Binder>) -> Out {
        let cap_ty = self.atom_ty(cap, env);

        // The capability identity (its nominal hash) and whether it is a known,
        // in-scope capability value.
        let cap_hash = match &cap_ty {
            Ty::Known(Type::Nominal { def, .. }) if self.world.is_cap(def) => Some(*def),
            _ => None,
        };
        if cap_hash.is_none() {
            self.emit(Diagnostic::error(
                Code::UnauthorizedPerform,
                "`perform` requires a capability value in scope; none was received \
                 (no ambient authority)"
                    .to_string(),
            ));
        }

        // A capability must be received or narrowed — never forged by
        // construction (`spec/01` §5).
        if let Atom::Var(idx) = cap {
            let d = env.len();
            if (*idx as usize) < d {
                let level = d - 1 - *idx as usize;
                if env[level].prov == Prov::Computed {
                    self.emit(Diagnostic::error(
                        Code::ForgedCapability,
                        "capability value was constructed; capabilities are unforgeable and \
                         may only be received or narrowed"
                            .to_string(),
                    ));
                }
            }
        }

        // Operation signature: type the arguments and gather the result type and
        // the errors performing it may raise.
        let mut eff = EffectRow::empty();
        if let Some(h) = cap_hash {
            eff.caps.push(h);
        }
        let ret = if let Some(h) = cap_hash {
            if let Some(sig) = self
                .world
                .cap(&h)
                .and_then(|c| c.ops.get(op.0 as usize))
                .cloned()
            {
                self.check_args(&sig.params, args, env, "capability operation");
                for e in &sig.errors {
                    eff.errors.push(*e);
                }
                Ty::Known(sig.ret)
            } else {
                Ty::Unknown
            }
        } else {
            Ty::Unknown
        };

        let mut uses = atom_uses(cap, env);
        for a in args {
            uses = seq(&uses, &atom_uses(a, env));
        }
        Out { ty: ret, eff, uses }
    }

    /// Check a positional argument list against expected parameter types.
    fn check_args(&mut self, expected: &[Type], args: &[Atom], env: &[Binder], what: &str) {
        for (i, a) in args.iter().enumerate() {
            if let Some(et) = expected.get(i) {
                let at = self.atom_ty(a, env);
                if !compatible(&Ty::Known(et.clone()), &at) {
                    self.emit(Diagnostic::error(
                        Code::TypeMismatch,
                        format!(
                            "{what} argument {i} has type `{}`, expected `{}`",
                            show_ty(&at),
                            show_type(et)
                        ),
                    ));
                }
            }
        }
    }

    // ---- atoms & helpers ------------------------------------------------

    fn atom_ty(&self, a: &Atom, env: &[Binder]) -> Ty {
        match a {
            Atom::Lit(l) => lit_ty(l),
            Atom::Global(h) => self
                .world
                .global(h)
                .map(|t| Ty::Known(t.clone()))
                .unwrap_or(Ty::Unknown),
            Atom::Var(idx) => {
                let d = env.len();
                if (*idx as usize) >= d {
                    return Ty::Unknown;
                }
                env[d - 1 - *idx as usize].ty.clone()
            }
        }
    }

    /// The type of projecting field `idx` from a base, peeling references and
    /// `linear` wrappers and resolving nominal structs / tuples.
    fn proj_ty(&self, base: &Ty, idx: u32) -> Ty {
        let t = match base {
            Ty::Known(t) => t,
            _ => return Ty::Unknown,
        };
        match peel(t) {
            Type::Nominal { def, .. } => self
                .world
                .struct_decl(def)
                .and_then(|s| s.fields.get(idx as usize))
                .cloned()
                .map(Ty::Known)
                .unwrap_or(Ty::Unknown),
            Type::Tuple(elems) => elems
                .get(idx as usize)
                .cloned()
                .map(Ty::Known)
                .unwrap_or(Ty::Unknown),
            _ => Ty::Unknown,
        }
    }

    /// Emit the appropriate linearity diagnostic for a `(min, max)` use profile,
    /// or nothing if the value is consumed exactly once on every path.
    fn check_linear(&mut self, (min, max): (u32, u32)) {
        if max == 0 {
            self.emit(linear_diag(
                Code::LinearUnused,
                "a `linear` value is never consumed; it must be used exactly once",
                "consume it (pass it to a consuming function, or close/free it)",
            ));
        } else if max >= 2 {
            self.emit(linear_diag(
                Code::LinearDuplicated,
                "a `linear` value is consumed more than once along some path",
                "remove the extra use so the value is consumed exactly once",
            ));
        } else if min == 0 {
            self.emit(linear_diag(
                Code::LinearNotAllPaths,
                "a `linear` value is consumed on some control paths but not all",
                "consume it in every branch (or in none, moving the use after the match)",
            ));
        }
    }
}

// ============================ free helpers ===============================

/// Where a reference escaped, for the diagnostic message.
enum EscapeSite {
    StructField(usize),
    CtorField(usize),
    Return,
}

fn escaping_ref_diag(site: EscapeSite) -> Diagnostic {
    let (msg, fix_title) = match site {
        EscapeSite::StructField(i) => (
            format!(
                "field {i} has reference type; a second-class reference may not be stored in a \
                 struct (it would escape its call frame)"
            ),
            "store the referent by value instead of a reference",
        ),
        EscapeSite::CtorField(i) => (
            format!(
                "field {i} is a reference; a second-class reference may not be placed in an \
                 aggregate (it would escape its call frame)"
            ),
            "store the referent by value instead of a reference",
        ),
        EscapeSite::Return => (
            "a second-class reference may not be returned; it would outlive the call that \
             created it"
                .to_string(),
            "return the referent by value instead of a reference",
        ),
    };
    Diagnostic::error(Code::EscapingReference, msg).with_fix(Fix::new(
        fix_title,
        Edit::insert(""),
        0.6,
    ))
}

fn linear_diag(code: Code, msg: &str, fix_title: &str) -> Diagnostic {
    Diagnostic::error(code, msg.to_string()).with_fix(Fix::new(fix_title, Edit::insert(""), 0.6))
}

/// `Out`-free linear-use profile for an atom: a use of a `linear` `Var` is one
/// use on the single path; everything else is empty.
fn atom_uses(a: &Atom, env: &[Binder]) -> Uses {
    if let Atom::Var(idx) = a {
        let d = env.len();
        if (*idx as usize) < d {
            let level = (d - 1 - *idx as usize) as u32;
            if env[level as usize].linear {
                let mut u = Uses::new();
                u.insert(level, (1, 1));
                return u;
            }
        }
    }
    Uses::new()
}

/// Sequence two use profiles (both execute): add `min` and `max` per level.
fn seq(a: &Uses, b: &Uses) -> Uses {
    let mut out = a.clone();
    for (k, (bmin, bmax)) in b {
        let e = out.entry(*k).or_insert((0, 0));
        e.0 += bmin;
        e.1 += bmax;
    }
    out
}

/// Merge sibling branches (exactly one executes): `min` is the least over
/// branches (a level absent from a branch contributes 0), `max` the greatest.
fn branch_merge(branches: &[Uses]) -> Uses {
    let mut keys: BTreeMap<u32, ()> = BTreeMap::new();
    for b in branches {
        for k in b.keys() {
            keys.insert(*k, ());
        }
    }
    let mut out = Uses::new();
    for k in keys.keys() {
        let mut min = u32::MAX;
        let mut max = 0u32;
        for b in branches {
            let (bmin, bmax) = b.get(k).copied().unwrap_or((0, 0));
            min = min.min(bmin);
            max = max.max(bmax);
        }
        if branches.is_empty() {
            min = 0;
        }
        out.insert(*k, (min, max));
    }
    out
}

fn lit_ty(l: &Literal) -> Ty {
    match l {
        Literal::Unit => Ty::Known(Type::Unit),
        Literal::Bool(_) => Ty::Known(Type::Bool),
        Literal::Int(_) => Ty::IntLit,
        Literal::Float(_) => Ty::Known(Type::Float(FloatTy::F64)),
        Literal::Str(_) => Ty::Known(Type::Str),
        Literal::Char(_) => Ty::Known(Type::Char),
    }
}

fn value_prov(value: &Core) -> Prov {
    match value {
        Core::Ctor { .. }
        | Core::Array { .. }
        | Core::IndexSet { .. }
        | Core::ListNew { .. }
        | Core::ListPush { .. }
        | Core::ListPop { .. }
        | Core::ListSet { .. }
        | Core::Prim { .. } => Prov::Computed,
        _ => Prov::Other,
    }
}

fn is_linear(t: &Ty) -> bool {
    matches!(t, Ty::Known(Type::Linear(_)))
}

fn is_ref(t: &Ty) -> bool {
    matches!(t, Ty::Known(ty) if matches!(peel_ref_target(ty), Type::Ref { .. }))
}

/// The content hash of the synthetic `Result` nominal a lowered `!T` error
/// union uses, and of its `@error-union` set marker (`marv_core::lower`,
/// `spec/02` §D). Computed once.
fn result_hash() -> Hash {
    static H: OnceLock<Hash> = OnceLock::new();
    *H.get_or_init(|| symbol_hash("Result"))
}

fn error_union_marker_hash() -> Hash {
    static H: OnceLock<Hash> = OnceLock::new();
    *H.get_or_init(|| symbol_hash("@error-union"))
}

/// If `t` is a lowered `!T` error union (`Result[T, @error-union]`), the success
/// type `T`; otherwise `None`. The `@error-union` marker in the second argument
/// distinguishes it from a user-written `Result[T, E]`.
fn error_union_success(t: &Type) -> Option<&Type> {
    if let Type::Nominal { def, args } = t {
        if *def == result_hash() && args.len() == 2 {
            if let Type::Nominal { def: e, .. } = &args[1] {
                if *e == error_union_marker_hash() {
                    return Some(&args[0]);
                }
            }
        }
    }
    None
}

/// Peel an error union to its success type (recursively), so a value of type
/// `!T` behaves as its success `T` wherever a concrete type is required — the
/// effect of the `?` operator, which propagates the error and yields the success
/// value (`spec/01` §6). A non-union type is returned unchanged.
fn peel_eu(t: &Type) -> &Type {
    match error_union_success(t) {
        Some(succ) => peel_eu(succ),
        None => t,
    }
}

/// Peel `Linear` wrappers to reach the underlying type (for ref/struct checks).
fn peel_ref_target(t: &Type) -> &Type {
    match t {
        Type::Linear(inner) => peel_ref_target(inner),
        other => other,
    }
}

/// Peel both `Ref` and `Linear` wrappers to reach a nominal/tuple for
/// projection.
fn peel(t: &Type) -> &Type {
    match t {
        Type::Ref { of, .. } => peel(of),
        Type::Linear(inner) => peel(inner),
        other => other,
    }
}

/// Whether a type is a scalar `as`-cast operand/target (`spec/01` §3.1):
/// `int`/`float`/`bool`/`char`. References and `linear` wrappers are peeled.
fn is_scalar(t: &Type) -> bool {
    matches!(
        peel(t),
        Type::Int(_) | Type::Float(_) | Type::Bool | Type::Char
    )
}

/// Whether a constant integer literal fits in the given integer width — the
/// statically-decidable case of the narrowing `as` check (`spec/01` §3.1). The
/// 64-bit widths accept any `i64` (the literal's own representation).
fn int_lit_fits(n: i64, ty: IntTy) -> bool {
    let n = n as i128;
    let (lo, hi): (i128, i128) = match ty {
        IntTy::I8 => (i8::MIN as i128, i8::MAX as i128),
        IntTy::I16 => (i16::MIN as i128, i16::MAX as i128),
        IntTy::I32 => (i32::MIN as i128, i32::MAX as i128),
        IntTy::I64 | IntTy::Isize => (i64::MIN as i128, i64::MAX as i128),
        IntTy::U8 => (0, u8::MAX as i128),
        IntTy::U16 => (0, u16::MAX as i128),
        IntTy::U32 => (0, u32::MAX as i128),
        IntTy::U64 | IntTy::Usize => (0, u64::MAX as i128),
    };
    lo <= n && n <= hi
}

/// The fields and linearity of a lowered struct type (`Tuple`, possibly
/// `Linear`-wrapped).
fn peel_struct(t: &Type) -> (Vec<Type>, bool) {
    match t {
        Type::Linear(inner) => {
            let (f, _) = peel_struct(inner);
            (f, true)
        }
        Type::Tuple(fields) => (fields.clone(), false),
        _ => (Vec::new(), false),
    }
}

/// Structural type compatibility with the checker's two special types: `Unknown`
/// is compatible with anything, an integer literal with any integer type. Arrow
/// effect rows are ignored (subsumption is checked separately).
fn compatible(a: &Ty, b: &Ty) -> bool {
    // An error-union value (`!T`) is compatible with its success type `T` — the
    // `?` operator yields the success value (`spec/01` §6). Peel both sides first
    // so `!i64` and `i64` (and an integer literal) compare equal.
    let pa = peel_ty(a);
    let pb = peel_ty(b);
    match (&pa, &pb) {
        (Ty::Unknown, _) | (_, Ty::Unknown) => true,
        (Ty::IntLit, Ty::IntLit) => true,
        (Ty::IntLit, Ty::Known(Type::Int(_))) | (Ty::Known(Type::Int(_)), Ty::IntLit) => true,
        // An unresolved type parameter accepts a literal (see `type_compat`).
        (Ty::IntLit, Ty::Known(Type::Var(_))) | (Ty::Known(Type::Var(_)), Ty::IntLit) => true,
        (Ty::IntLit, _) | (_, Ty::IntLit) => false,
        // `a` is the expected (target) type and `b` the actual (source) at every
        // call site, so the array→slice coercion is checked directionally here.
        (Ty::Known(x), Ty::Known(y)) => {
            type_compat(x, y) || coerces_to(x, y) || ctor_erased_nominal_eq(x, y)
        }
    }
}

/// Structural compatibility with unresolved type parameters as wildcards. A
/// [`Type::Var`] is a generic parameter the current context cannot resolve — a
/// generic *enum's* field type at a construction or `match` site
/// (`Option.Some(n)` checks `i64` against the declaration's `T`), or a generic
/// function's un-instantiated base body — so it is compatible with anything
/// here. This looseness is confined to [`compatible`]: every monomorphized
/// instance is still checked at its concrete types, and [`type_eq`] stays exact
/// for every other comparison.
fn type_compat(a: &Type, b: &Type) -> bool {
    use Type::*;
    match (a, b) {
        (Var(_), _) | (_, Var(_)) => true,
        (Array(e1, n1), Array(e2, n2)) => n1 == n2 && type_compat(e1, e2),
        (Slice(e1), Slice(e2)) => type_compat(e1, e2),
        (Tuple(xs), Tuple(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| type_compat(x, y))
        }
        (
            Arrow {
                param: p1, ret: r1, ..
            },
            Arrow {
                param: p2, ret: r2, ..
            },
        ) => type_compat(p1, p2) && type_compat(r1, r2),
        (Nominal { def: d1, args: a1 }, Nominal { def: d2, args: a2 }) => {
            d1 == d2 && a1.len() == a2.len() && a1.iter().zip(a2).all(|(x, y)| type_compat(x, y))
        }
        (
            Ref {
                mutable: m1,
                of: o1,
            },
            Ref {
                mutable: m2,
                of: o2,
            },
        ) => m1 == m2 && type_compat(o1, o2),
        (Linear(x), Linear(y)) => type_compat(x, y),
        _ => type_eq(a, b),
    }
}

/// A constructed value ([`Core::Ctor`]) carries no type arguments — `synth_ctor`
/// types it as `Nominal { def, args: [] }` — while a declared generic reference
/// (`Option[T]`) carries them. The same `def` with exactly one side
/// unparameterized is therefore compatible: the instantiation is not recoverable
/// at the `Ctor` site (the names-erased Core records only the nominal hash and
/// tag), so the checker trusts the parameterized side. Two *parameterized*
/// references still compare argument-by-argument in [`type_eq`].
fn ctor_erased_nominal_eq(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Nominal { def: d1, args: a1 }, Type::Nominal { def: d2, args: a2 }) => {
            d1 == d2 && (a1.is_empty() != a2.is_empty())
        }
        _ => false,
    }
}

/// Whether a value of type `source` may be used where `target` is expected via an
/// implicit coercion (MARV-33). The only such coercion is a fixed-length array
/// `[N]T` used as a runtime-length slice `[]T`: the two share the boxed
/// `[len, e0, …]` layout, so the conversion just forgets the static length. It
/// applies through a second-class reference too (`&[N]T` → `&[]T`), which is how
/// an array argument satisfies a `&[]T` parameter (`spec/01` §4). The coercion is
/// one-way: a slice is *not* usable where a fixed array is expected.
fn coerces_to(target: &Type, source: &Type) -> bool {
    match (target, source) {
        (Type::Slice(t), Type::Array(s, _)) => type_eq(t, s),
        (Type::Ref { mutable: mt, of: t }, Type::Ref { mutable: ms, of: s }) => {
            mt == ms && coerces_to(t, s)
        }
        _ => false,
    }
}

fn list_type(elem: Type) -> Type {
    Type::Nominal {
        def: symbol_hash("std.collections.List"),
        args: vec![elem],
    }
}

fn list_elem_type(t: &Type) -> Option<&Type> {
    match peel(t) {
        Type::Nominal { def, args }
            if *def == symbol_hash("std.collections.List") && args.len() == 1 =>
        {
            args.first()
        }
        _ => None,
    }
}

/// Peel an error union out of a checker [`Ty`] (a no-op for `IntLit`/`Unknown`
/// and non-union known types).
fn peel_ty(t: &Ty) -> Ty {
    match t {
        Ty::Known(ty) => Ty::Known(peel_eu(ty).clone()),
        other => other.clone(),
    }
}

/// Structural type equality that ignores arrow effect rows (those are compared
/// by the dedicated effect/error subsumption check, not by unification).
fn type_eq(a: &Type, b: &Type) -> bool {
    use Type::*;
    match (a, b) {
        (Unit, Unit) | (Bool, Bool) | (Str, Str) | (Char, Char) => true,
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Array(e1, n1), Array(e2, n2)) => n1 == n2 && type_eq(e1, e2),
        (Slice(e1), Slice(e2)) => type_eq(e1, e2),
        (Tuple(xs), Tuple(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| type_eq(x, y))
        }
        (
            Arrow {
                param: p1, ret: r1, ..
            },
            Arrow {
                param: p2, ret: r2, ..
            },
        ) => type_eq(p1, p2) && type_eq(r1, r2),
        (Nominal { def: d1, args: a1 }, Nominal { def: d2, args: a2 }) => {
            d1 == d2 && a1.len() == a2.len() && a1.iter().zip(a2).all(|(x, y)| type_eq(x, y))
        }
        (
            Ref {
                mutable: m1,
                of: o1,
            },
            Ref {
                mutable: m2,
                of: o2,
            },
        ) => m1 == m2 && type_eq(o1, o2),
        (Linear(x), Linear(y)) => type_eq(x, y),
        (Var(x), Var(y)) => x == y,
        _ => false,
    }
}

fn numeric(t: &Ty) -> bool {
    matches!(
        peel_ty(t),
        Ty::IntLit | Ty::Unknown | Ty::Known(Type::Int(_)) | Ty::Known(Type::Float(_))
    )
}

/// The result type of an arithmetic op given two numeric operands: prefer a
/// concrete width over an integer literal.
fn join_numeric(l: &Ty, r: &Ty) -> Ty {
    match (peel_ty(l), peel_ty(r)) {
        (Ty::Known(t), _) | (_, Ty::Known(t)) => Ty::Known(t),
        _ => Ty::IntLit,
    }
}

fn ty_to_type(t: &Ty) -> Type {
    match t {
        Ty::Known(t) => t.clone(),
        // An integer literal with no constraint defaults to `i32`; `Unknown`
        // collapses to unit only for display/return purposes (it never reaches a
        // hash — the checker does not re-emit Core).
        Ty::IntLit => Type::Int(IntTy::I32),
        Ty::Unknown => Type::Unit,
    }
}

fn arg(args: &[Ty], i: usize) -> &Ty {
    args.get(i).unwrap_or(&Ty::Unknown)
}

fn union(mut a: EffectRow, b: &EffectRow) -> EffectRow {
    for c in &b.caps {
        if !a.caps.contains(c) {
            a.caps.push(*c);
        }
    }
    for e in &b.errors {
        if !a.errors.contains(e) {
            a.errors.push(*e);
        }
    }
    a
}

fn lowercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn missing_arms_text(missing: &[String]) -> String {
    missing
        .iter()
        .map(|v| format!("{v} => {{ todo }},\n"))
        .collect()
}

fn prim_name(op: PrimOp) -> &'static str {
    use PrimOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Rem => "%",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        And => "and",
        Or => "or",
        Not => "not",
        Neg => "-",
        Len => "len",
        Index => "index",
    }
}

// ---- display ------------------------------------------------------------

fn show_ty(t: &Ty) -> String {
    match t {
        Ty::Known(t) => show_type(t),
        Ty::IntLit => "{integer}".to_string(),
        Ty::Unknown => "{unknown}".to_string(),
    }
}

fn show_type(t: &Type) -> String {
    match t {
        Type::Unit => "()".into(),
        Type::Bool => "bool".into(),
        Type::Int(i) => int_name(*i).into(),
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

fn int_name(i: IntTy) -> &'static str {
    match i {
        IntTy::I8 => "i8",
        IntTy::I16 => "i16",
        IntTy::I32 => "i32",
        IntTy::I64 => "i64",
        IntTy::Isize => "isize",
        IntTy::U8 => "u8",
        IntTy::U16 => "u16",
        IntTy::U32 => "u32",
        IntTy::U64 => "u64",
        IntTy::Usize => "usize",
    }
}
