//! Monomorphization of generics and interface/impl lowering
//! (`spec/01-design-spec.md` §§3.3–3.4): a generic function call is specialized
//! per concrete type argument, interface-method calls in the specialized body are
//! dispatched to the coherent impl, and the lowerer records the interface/impl/
//! instantiation metadata the checker consumes.

use marv_core::ir::*;
use marv_core::{lower_module, lower_modules, DefEntry, LoweredModule};
use marv_syntax::parse;

fn lower(src: &str) -> LoweredModule {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"))
}

fn def<'a>(m: &'a LoweredModule, name: &str) -> &'a DefEntry {
    m.defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no def `{name}` (have: {:?})", names(m)))
}

fn names(m: &LoweredModule) -> Vec<&str> {
    m.defs.iter().map(|d| d.name.as_str()).collect()
}

const MAX: &str = "\
mod demo

enum Ordering {
    Lt,
    Eq,
    Gt,
}

interface Ord[T] {
    fn cmp(a: T, b: T) -> Ordering
}

impl Ord[i32] {
    fn cmp(a: i32, b: i32) -> Ordering {
        if (a < b) {
            Ordering.Lt
        } else {
            Ordering.Gt
        }
    }
}

fn max[T: Ord](a: T, b: T) -> T {
    match cmp(a, b) {
        Ordering.Lt => b,
        Ordering.Eq => a,
        Ordering.Gt => a,
    }
}

fn main() -> i32 {
    max(3, 7)
}
";

#[test]
fn impl_method_lowers_under_a_mangled_name() {
    let m = lower(MAX);
    // The impl's method is a concrete `Fn` def under a mangled, collision-free
    // name; `cmp` itself is not a top-level def.
    let cmp = def(&m, "cmp$Ord$i32");
    assert_eq!(cmp.def.kind, DefKind::Fn);
    assert!(!names(&m).contains(&"cmp"));
}

#[test]
fn generic_call_requests_a_specialized_instance() {
    let m = lower(MAX);
    // The generic base def is kept (so its body type-checks once)...
    assert_eq!(def(&m, "max").def.kind, DefKind::Fn);
    // ...and `main`'s call to `max(3, 7)` produced a concrete `max@i32` instance.
    let inst = def(&m, "max@i32");
    assert_eq!(inst.def.kind, DefKind::Fn);
}

#[test]
fn interface_is_recorded_and_carries_no_type_var() {
    let m = lower(MAX);
    // The interface is recorded with its method names.
    let iface = m
        .interfaces
        .iter()
        .find(|i| i.name == "Ord")
        .expect("Ord interface recorded");
    assert_eq!(iface.methods, vec!["cmp".to_string()]);
    // The impl is recorded for the concrete type key.
    let imp = m
        .impls
        .iter()
        .find(|i| i.interface == "Ord" && i.type_key == "i32")
        .expect("impl Ord[i32] recorded");
    assert!(imp
        .methods
        .iter()
        .any(|(meth, def)| meth == "cmp" && def == "demo.cmp$Ord$i32"));
    // The specialized instance carries no `Type::Var` (it is fully concrete).
    let inst = def(&m, "max@i32");
    assert!(
        !ty_has_var(&inst.def.ty),
        "instance arrow still has Type::Var"
    );
}

#[test]
fn instantiation_records_the_bound_and_concrete_type() {
    let m = lower(MAX);
    let inst = m
        .instantiations
        .iter()
        .find(|i| i.instance == "max@i32")
        .expect("instantiation recorded");
    assert_eq!(inst.generic, "max");
    assert_eq!(inst.args.len(), 1);
    let arg = &inst.args[0];
    assert_eq!(arg.param, "T");
    assert_eq!(arg.type_key, "i32");
    assert_eq!(arg.bound.as_deref(), Some("Ord"));
}

#[test]
fn specialized_body_dispatches_the_interface_method_to_the_impl() {
    let m = lower(MAX);
    let body = def(&m, "max@i32").def.body.clone().expect("instance body");
    // The `cmp(a, b)` call in the specialized body must reference the concrete
    // impl method `demo.cmp$Ord$i32`, never the un-dispatched `cmp`.
    let target = marv_core::symbol_hash("demo.cmp$Ord$i32");
    let undispatched = marv_core::symbol_hash("cmp");
    assert!(
        globals(&body).contains(&target),
        "instance does not call the resolved impl method"
    );
    assert!(
        !globals(&body).contains(&undispatched),
        "instance still references the un-dispatched interface method"
    );
}

#[test]
fn generic_struct_field_lowers_to_a_type_var() {
    let m = lower("mod demo\n\nstruct Pair[T] {\n    a: T,\n    b: T,\n}\n");
    let pair = def(&m, "Pair");
    // `struct Pair[T] { a: T, b: T }` → a 2-tuple of `Type::Var(0)`.
    match &pair.def.ty {
        Type::Tuple(fields) => {
            assert_eq!(fields, &vec![Type::Var(0), Type::Var(0)]);
        }
        other => panic!("expected a tuple struct body, got {other:?}"),
    }
}

#[test]
fn cross_module_generic_instance_lands_in_the_defining_module() {
    // The `std/ord.mv` prelude defines the generic `max`; a dependent module that
    // calls it must specialize `max@i32` into the *defining* module so the
    // instance's internal references (the impl method) resolve.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf();
    let ord = parse(&std::fs::read_to_string(root.join("std/ord.mv")).unwrap()).expect("parse ord");
    let app = parse("mod app\nimport std.ord (Ordering)\n\nfn main() -> i32 {\n    max(3, 7)\n}\n")
        .expect("parse app");
    let lowered = lower_modules(&[ord, app]).expect("lower prelude + app");
    let ord_mod = lowered
        .iter()
        .find(|m| m.module == ["std", "ord"])
        .expect("std.ord module");
    // The instance is generated into std.ord (where `max` is defined), not app.
    assert!(
        ord_mod.defs.iter().any(|d| d.name == "max@i32"),
        "max@i32 instance not emitted into the defining module (have: {:?})",
        names(ord_mod)
    );
    let app_mod = lowered.iter().find(|m| m.module == ["app"]).unwrap();
    assert!(
        app_mod.instantiations.is_empty(),
        "the instance should be recorded against the defining module"
    );
    // And the instantiation is recorded against std.ord for bound checking.
    assert!(ord_mod
        .instantiations
        .iter()
        .any(|i| i.instance == "max@i32"));
}

// ---- helpers ------------------------------------------------------------

fn ty_has_var(t: &Type) -> bool {
    match t {
        Type::Var(_) => true,
        Type::Array(e, _) | Type::Slice(e) | Type::Linear(e) | Type::Ref { of: e, .. } => {
            ty_has_var(e)
        }
        Type::Tuple(es) => es.iter().any(ty_has_var),
        Type::Arrow { param, ret, .. } => ty_has_var(param) || ty_has_var(ret),
        Type::Nominal { args, .. } => args.iter().any(ty_has_var),
        _ => false,
    }
}

/// Collect every `Atom::Global` hash referenced anywhere in a Core term.
fn globals(c: &Core) -> Vec<Hash> {
    let mut out = Vec::new();
    walk(c, &mut out);
    out
}

fn walk(c: &Core, out: &mut Vec<Hash>) {
    let atom = |a: &Atom, out: &mut Vec<Hash>| {
        if let Atom::Global(h) = a {
            out.push(*h);
        }
    };
    match c {
        Core::Atom(a) => atom(a, out),
        Core::Let { value, body } => {
            walk(value, out);
            walk(body, out);
        }
        Core::Lam { body, .. } => walk(body, out),
        Core::App { func, arg } => {
            atom(func, out);
            atom(arg, out);
        }
        Core::Ctor { fields, .. } => fields.iter().for_each(|a| atom(a, out)),
        Core::Array { items, .. } => items.iter().for_each(|a| atom(a, out)),
        Core::IndexSet { base, index, value } => {
            atom(base, out);
            atom(index, out);
            atom(value, out);
        }
        Core::ListNew {
            alloc, capacity, ..
        } => {
            atom(alloc, out);
            atom(capacity, out);
        }
        Core::ListPush { alloc, list, value } => {
            atom(alloc, out);
            atom(list, out);
            atom(value, out);
        }
        Core::ListPop { list } => atom(list, out),
        Core::ListSet { list, index, value } => {
            atom(list, out);
            atom(index, out);
            atom(value, out);
        }
        Core::Proj { base, .. } => atom(base, out),
        Core::Match {
            scrutinee,
            branches,
        } => {
            atom(scrutinee, out);
            branches.iter().for_each(|b| walk(&b.body, out));
        }
        Core::Prim { args, .. } => args.iter().for_each(|a| atom(a, out)),
        Core::Cast { value, .. } => atom(value, out),
        Core::Ref { of, .. } => atom(of, out),
        Core::Perform { cap, args, .. } => {
            atom(cap, out);
            args.iter().for_each(|a| atom(a, out));
        }
        Core::Raise { args, .. } => args.iter().for_each(|a| atom(a, out)),
        Core::Return { value } => atom(value, out),
        Core::Loop {
            state, cond, body, ..
        } => {
            state.iter().for_each(|a| atom(a, out));
            walk(cond, out);
            walk(body, out);
        }
    }
}
