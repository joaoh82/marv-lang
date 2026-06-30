//! Interpreter behavior tests: pure evaluation through the real front end, and
//! the capability-injection sandbox (`spec/03` §4.5) — driven both over
//! hand-built Core and, since MARV-6, from real `.mv` source that performs and
//! narrows capabilities.

use marv_core::ir::*;
use marv_core::{lower_module, symbol_hash};
use marv_interp::{Program, RunError, Value};
use marv_types::{OpSig, World, WorldBuilder};

/// Lower a source module and wrap it in a runnable program.
fn program_from_source(src: &str) -> Program {
    let module = marv_syntax::parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let module_path = module.name.join(".");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    Program::new(&module_path, defs, world)
}

#[test]
fn interprets_recursive_factorial() {
    let prog = program_from_source(
        "mod demo\n\npure fn factorial(n: i64) -> i64 {\n    if n < 2 {\n        1\n    } else {\n        n * factorial(n - 1)\n    }\n}\n",
    );
    let out = prog.run("factorial", &[], &["6".to_string()]).expect("run");
    assert_eq!(out.value, Value::Int(720));
    assert!(
        out.effects.is_empty(),
        "a pure function performs no effects"
    );
}

#[test]
fn fills_value_parameters_from_args_in_order() {
    let prog =
        program_from_source("mod demo\n\npure fn sub(a: i64, b: i64) -> i64 {\n    a - b\n}\n");
    let out = prog
        .run("sub", &[], &["10".to_string(), "3".to_string()])
        .expect("run");
    assert_eq!(out.value, Value::Int(7));
}

#[test]
fn interprets_enum_construction_and_match() {
    // A monomorphic enum + an exhaustive `match`, end to end through the real
    // front end: `main` constructs `Color.Green` and `rank` matches it to 2.
    let prog = program_from_source(
        "mod demo\n\nenum Color {\n    Red,\n    Green,\n    Blue,\n}\n\npure fn rank(c: Color) -> i64 {\n    match c {\n        Color.Red => 1,\n        Color.Green => 2,\n        Color.Blue => 3,\n    }\n}\n\npure fn main() -> i64 {\n    rank(Color.Green)\n}\n",
    );
    let out = prog.run("main", &[], &[]).expect("run");
    assert_eq!(out.value, Value::Int(2));
    assert!(out.effects.is_empty(), "a pure match performs no effects");
}

#[test]
fn interprets_payload_variant_binding() {
    // `Some(x) => x` binds and returns the payload; `None => fallback` does not.
    let src = "mod demo\n\nenum Box {\n    Empty,\n    Full(i64),\n}\n\npure fn unwrap(b: Box, fallback: i64) -> i64 {\n    match b {\n        Box.Empty => fallback,\n        Box.Full(x) => x,\n    }\n}\n\npure fn main() -> i64 {\n    unwrap(Box.Full(42), 0)\n}\n";
    let prog = program_from_source(src);
    let out = prog.run("main", &[], &[]).expect("run");
    assert_eq!(out.value, Value::Int(42));
}

#[test]
fn interprets_struct_construction_field_and_var_mutation() {
    // Build a struct, mutate a `var` accumulator and a struct field, and read
    // fields back — all end to end through the real front end (MARV-4).
    let src = "mod demo\n\nstruct Point { x: i64, y: i64 }\n\npure fn run() -> i64 {\n    var sum = 0\n    let p = Point { x: 10, y: 20 }\n    sum = (sum + p.x)\n    sum = (sum + p.y)\n    var q = Point { x: 1, y: 2 }\n    q.x = 5\n    sum = (sum + q.x)\n    sum\n}\n";
    let prog = program_from_source(src);
    let out = prog.run("run", &[], &[]).expect("run");
    // 0 + 10 + 20 + 5 = 35.
    assert_eq!(out.value, Value::Int(35));
    assert!(out.effects.is_empty(), "mutable value semantics is pure");
}

#[test]
fn mutation_has_value_semantics_no_aliasing() {
    // `var q = p` copies `p`; mutating `q.x` must not change `p` (spec/01 §4 —
    // no shared mutable aliasing of owned values).
    let src = "mod demo\n\nstruct Point { x: i64, y: i64 }\n\npure fn moved(p: Point) -> Point {\n    var q = p\n    q.x = (q.x + 100)\n    q\n}\n\npure fn run() -> i64 {\n    let start = Point { x: 1, y: 2 }\n    let m = moved(start)\n    (m.x - start.x)\n}\n";
    let prog = program_from_source(src);
    let out = prog.run("run", &[], &[]).expect("run");
    // m.x = 101, start.x stays 1 ⇒ difference is 100.
    assert_eq!(out.value, Value::Int(100));
}

#[test]
fn interprets_while_loop_carrying_two_vars() {
    // A `while` carrying `sum` and `i` across iterations (MARV-2): the running
    // sum and the countdown index both update each pass and survive to the next.
    let src = "mod demo\n\npure fn sum_to(n: i64) -> i64 {\n    var sum: i64 = 0\n    var i: i64 = n\n    while (i > 0) {\n        sum = (sum + i)\n        i = (i - 1)\n    }\n    sum\n}\n";
    let prog = program_from_source(src);
    // 5 + 4 + 3 + 2 + 1 = 15.
    let out = prog.run("sum_to", &[], &["5".to_string()]).expect("run");
    assert_eq!(out.value, Value::Int(15));
    assert!(out.effects.is_empty(), "a pure loop performs no effects");
}

#[test]
fn while_loop_runs_zero_times_when_condition_is_initially_false() {
    let src = "mod demo\n\npure fn sum_to(n: i64) -> i64 {\n    var sum: i64 = 0\n    var i: i64 = n\n    while (i > 0) {\n        sum = (sum + i)\n        i = (i - 1)\n    }\n    sum\n}\n";
    let prog = program_from_source(src);
    let out = prog.run("sum_to", &[], &["0".to_string()]).expect("run");
    assert_eq!(out.value, Value::Int(0));
}

#[test]
fn satisfied_loop_invariant_does_not_abort() {
    // `i >= 0` holds at every loop header for a non-negative countdown.
    let src = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = n\n    while (i > 0)\n        invariant (i >= 0)\n    {\n        i = (i - 1)\n    }\n    i\n}\n";
    let prog = program_from_source(src);
    let out = prog.run("run", &[], &["4".to_string()]).expect("run");
    assert_eq!(out.value, Value::Int(0));
}

#[test]
fn violated_loop_invariant_aborts_with_a_structured_report() {
    // `invariant (i > 5)` fails the moment the countdown reaches 5: a Tier-1
    // violation that aborts the run with the offending concrete values rendered.
    let src = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = n\n    while (i > 0)\n        invariant (i > 5)\n    {\n        i = (i - 1)\n    }\n    i\n}\n";
    let prog = program_from_source(src);
    let err = prog
        .run("run", &[], &["10".to_string()])
        .expect_err("the invariant is violated when i reaches 5");
    match err {
        RunError::InvariantViolated(report) => {
            // Concrete values are substituted: at the failing header, i == 5.
            assert_eq!(report, "5 > 5");
        }
        other => panic!("expected an invariant violation, got {other:?}"),
    }
}

#[test]
fn out_of_bounds_slice_read_aborts_with_a_structured_report() {
    // The Tier-1 bounds check (`spec/01` §7, MARV-34): a runtime subscript at
    // `len` (and a negative one) aborts the run with the offending index and
    // the collection's length; an in-bounds read is untouched.
    let src =
        "mod demo\n\npure fn nth(i: i64) -> i64 {\n    let s: []i64 = [5, 6, 7, 8]\n    s[i]\n}\n";
    let prog = program_from_source(src);

    let ok = prog.run("nth", &[], &["3".to_string()]).expect("in bounds");
    assert_eq!(ok.value, Value::Int(8));

    let err = prog
        .run("nth", &[], &["4".to_string()])
        .expect_err("index 4 is past the end of a 4-element slice");
    assert_eq!(err, RunError::BoundsCheckFailed { index: 4, len: 4 });
    assert_eq!(
        err.to_string(),
        "bounds check failed: index 4 out of range for length 4"
    );

    let err = prog
        .run("nth", &[], &["-1".to_string()])
        .expect_err("a negative index is out of bounds");
    assert_eq!(err, RunError::BoundsCheckFailed { index: -1, len: 4 });
}

#[test]
fn out_of_bounds_slice_store_aborts_with_a_structured_report() {
    // The element store `s[i] = e` (`Core::IndexSet`) carries the same Tier-1
    // check as the read: storing at `len` aborts before any element changes.
    let src = "mod demo\n\npure fn set(i: i64) -> i64 {\n    var s: []i64 = [1, 2, 3]\n    s[i] = 10\n    s[0]\n}\n";
    let prog = program_from_source(src);

    let ok = prog.run("set", &[], &["0".to_string()]).expect("in bounds");
    assert_eq!(ok.value, Value::Int(10));

    let err = prog
        .run("set", &[], &["3".to_string()])
        .expect_err("index 3 is past the end of a 3-element slice");
    assert_eq!(err, RunError::BoundsCheckFailed { index: 3, len: 3 });
}

/// A `touch(fs: Fs)` whose body performs `Fs` op 0. Hand-built because the
/// surface has no `perform` form yet.
fn touch_program() -> Program {
    let fs = symbol_hash("Fs");
    let fs_ty = Type::Nominal {
        def: fs,
        args: Vec::new(),
    };
    // touch : Fs -{Fs}-> ()   ;   body = \fs. perform fs.op0("/etc/passwd")
    let row = EffectRow {
        caps: vec![fs],
        errors: Vec::new(),
    };
    let def = Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(fs_ty.clone()),
            ret: Box::new(Type::Unit),
            effects: row.clone(),
        },
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(Core::Lam {
            param: fs_ty,
            effects: row,
            body: Box::new(Core::Perform {
                cap: Atom::Var(0),
                op: OpId(0),
                args: vec![Atom::Lit(Literal::Str("/etc/passwd".to_string()))],
            }),
        }),
    };
    let world = WorldBuilder::new()
        .cap(
            "Fs",
            vec![OpSig {
                params: vec![Type::Str],
                ret: Type::Unit,
                errors: Vec::new(),
            }],
        )
        .build();
    Program::new("sandbox", vec![("touch".to_string(), def)], world)
}

/// An `alloc(a: Alloc)` entry whose body performs Alloc op 0. The interpreter
/// models host capability effects rather than real byte buffers today, so this
/// pins the grant/audit plumbing for MARV-41's allocator capability.
fn alloc_program() -> Program {
    let alloc = symbol_hash("Alloc");
    let alloc_ty = Type::Nominal {
        def: alloc,
        args: Vec::new(),
    };
    let row = EffectRow {
        caps: vec![alloc],
        errors: Vec::new(),
    };
    let def = Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(alloc_ty.clone()),
            ret: Box::new(Type::Unit),
            effects: row.clone(),
        },
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(Core::Lam {
            param: alloc_ty,
            effects: row,
            body: Box::new(Core::Perform {
                cap: Atom::Var(0),
                op: OpId(0),
                args: vec![Atom::Lit(Literal::Int(64))],
            }),
        }),
    };
    let world = WorldBuilder::new()
        .cap(
            "Alloc",
            vec![OpSig {
                params: vec![Type::Int(IntTy::Usize)],
                ret: Type::Slice(Box::new(Type::Int(IntTy::U8))),
                errors: Vec::new(),
            }],
        )
        .build();
    Program::new("sandbox", vec![("alloc".to_string(), def)], world)
}

#[test]
fn granted_capability_is_injected_and_its_effect_recorded() {
    let prog = touch_program();
    let out = prog
        .run("touch", &["Fs".to_string()], &[])
        .expect("run with Fs granted");
    assert_eq!(out.value, Value::Unit);
    assert_eq!(out.effects.len(), 1, "the perform should be recorded");
    let eff = &out.effects[0];
    assert_eq!(eff.cap, "Fs");
    assert_eq!(eff.op, 0);
    assert_eq!(eff.args, vec![Value::Str("/etc/passwd".to_string())]);
}

#[test]
fn alloc_capability_is_grantable_and_audited() {
    let prog = alloc_program();
    let out = prog
        .run("alloc", &["Alloc".to_string()], &[])
        .expect("run with Alloc granted");
    assert_eq!(out.value, Value::Unit);
    assert_eq!(out.effects.len(), 1, "the allocation perform is recorded");
    assert_eq!(out.effects[0].cap, "Alloc");
    assert_eq!(out.effects[0].op, 0);
    assert_eq!(out.effects[0].args, vec![Value::Int(64)]);
}

#[test]
fn ungranted_capability_is_refused_at_the_boundary() {
    let prog = touch_program();
    // No grant: the capability value is never created, so the entry cannot run.
    let err = prog.run("touch", &[], &[]).unwrap_err();
    assert_eq!(err, RunError::UngrantedCapability("Fs".to_string()));
}

#[test]
fn unknown_entry_is_an_error() {
    let prog = touch_program();
    assert_eq!(
        prog.run("nope", &[], &[]).unwrap_err(),
        RunError::NoSuchEntry("nope".to_string())
    );
}

/// Capabilities and narrowing driven from real `.mv` source (MARV-6, `spec/01`
/// §5): `io.fs()` narrows the granted `Io` to an `Fs` value, against which
/// `fs.read(path)` then performs — so granting only `Io` is enough, and both
/// the narrowing op and the read are recorded as effects in order.
#[test]
fn narrowing_from_source_runs_with_only_the_root_granted() {
    let src = "\
mod demo

interface Io {
    fn fs(io: &Io) -> Fs
}

interface Fs {
    fn read(fs: &Fs, path: str) -> ![]u8
}

fn main(io: Io, path: str) -> ![]u8 {
    let fs = io.fs()
    fs.read(path)
}
";
    let prog = program_from_source(src);
    let out = prog
        .run("main", &["Io".to_string()], &["/etc/hosts".to_string()])
        .expect("run with Io granted");
    // Two effects, in order: the narrowing op on `Io`, then the read on `Fs`.
    assert_eq!(out.effects.len(), 2);
    assert_eq!(out.effects[0].cap, "Io");
    assert_eq!(out.effects[1].cap, "Fs");
    assert_eq!(out.effects[1].op, 0);
    assert_eq!(
        out.effects[1].args,
        vec![Value::Str("/etc/hosts".to_string())]
    );
}

#[test]
fn http_request_response_ops_use_the_interpreter_test_host() {
    let src = "\
mod demo

interface Http {
    fn method(http: &Http) -> !str
    fn path(http: &Http) -> !str
    fn body_text(http: &Http) -> !str
    fn respond(http: &Http, status: u16, body: str) -> !
}

fn main(http: Http) -> !str {
    let body = http.body_text()?
    let sent = http.respond((200 as u16), body)?
    body
}
";
    let prog = program_from_source(src);
    let out = prog
        .run("main", &["Http".to_string()], &[])
        .expect("run with Http granted");
    assert_eq!(out.value, Value::Str("marv-http-echo".to_string()));
    assert_eq!(out.effects.len(), 2);
    assert_eq!(out.effects[0].cap, "Http");
    assert_eq!(out.effects[0].op, 2);
    assert_eq!(out.effects[1].cap, "Http");
    assert_eq!(out.effects[1].op, 3);
    assert_eq!(
        out.effects[1].args,
        vec![Value::Int(200), Value::Str("marv-http-echo".to_string())]
    );
}

#[test]
fn spawn_capability_from_source_runs_and_records_scoped_starts() {
    let src = "\
mod demo

interface Spawn {
    fn start(spawn: &Spawn) -> !
}

linear struct TaskI64 { value: i64 }

fn spawn_i64(spawn: Spawn, value: i64) -> !TaskI64 {
    let started: () = spawn.start()?
    TaskI64 { value: value }
}

fn join_i64(task: TaskI64) -> i64 {
    task.value
}

fn exercise(spawn: Spawn) -> !i64 {
    let left = spawn_i64(spawn, 20)?
    let right = spawn_i64(spawn, 22)?
    (join_i64(left) + join_i64(right))
}
";
    let prog = program_from_source(src);
    let out = prog
        .run("exercise", &["Spawn".to_string()], &[])
        .expect("run with Spawn granted");
    assert_eq!(out.value, Value::Int(42));
    assert_eq!(out.effects.len(), 2);
    assert_eq!(out.effects[0].cap, "Spawn");
    assert_eq!(out.effects[0].op, 0);
    assert_eq!(out.effects[1].cap, "Spawn");
    assert_eq!(out.effects[1].op, 0);
}

/// Granting only the narrowed capability (not the root it is narrowed *from*)
/// cannot satisfy an entry that receives the root: the value is never created.
#[test]
fn entry_root_capability_must_be_granted() {
    let src = "\
mod demo

interface Io {
    fn fs(io: &Io) -> Fs
}

interface Fs {
    fn read(fs: &Fs, path: str) -> ![]u8
}

fn main(io: Io, path: str) -> ![]u8 {
    let fs = io.fs()
    fs.read(path)
}
";
    let prog = program_from_source(src);
    let err = prog
        .run("main", &["Fs".to_string()], &["/etc/hosts".to_string()])
        .unwrap_err();
    assert_eq!(err, RunError::UngrantedCapability("Io".to_string()));
}
