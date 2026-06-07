//! Running monomorphized generics end to end through the real front end
//! (`spec/01-design-spec.md` §§3.3–3.4): a generic function with an interface
//! bound is specialized at the call site, its interface-method call is dispatched
//! to the coherent impl, and the specialized instance runs on the interpreter
//! oracle.

use marv_core::lower_module;
use marv_interp::{Program, Value};
use marv_types::World;

fn program_from_source(src: &str) -> Program {
    let module = marv_syntax::parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let module_path = module.name.join(".");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    Program::new(&module_path, defs, world)
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
        } else if (a > b) {
            Ordering.Gt
        } else {
            Ordering.Eq
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
";

#[test]
fn runs_a_monomorphized_bounded_generic() {
    let src = format!("{MAX}\nfn main() -> i32 {{\n    max(3, 7)\n}}\n");
    let prog = program_from_source(&src);
    let out = prog.run("main", &[], &[]).expect("run");
    assert_eq!(out.value, Value::Int(7));
}

#[test]
fn dispatches_through_the_impl_both_ways() {
    // `max(9, 2)` exercises the `Gt` arm — the impl's comparison really drives the
    // result, not a constant.
    let src = format!("{MAX}\nfn main() -> i32 {{\n    max(9, 2)\n}}\n");
    let prog = program_from_source(&src);
    let out = prog.run("main", &[], &[]).expect("run");
    assert_eq!(out.value, Value::Int(9));
}
