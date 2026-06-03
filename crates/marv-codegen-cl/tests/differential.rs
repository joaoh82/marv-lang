//! M4 acceptance gate: the interpreter (`marv-interp`) and the Cranelift backend
//! (`marv-codegen-cl`) must agree on a corpus of small marv programs — and a
//! program that uses a capability absent from its effect row must fail to
//! compile (`spec/01` §9, the milestone gate).
//!
//! Each source case is loaded the way the CLI loads it (parse → lower → build a
//! [`World`]), then run through both backends; the two results must be equal to
//! each other *and* to a hand-computed golden value. The capability case is a
//! Core-IR snapshot (`tests/run/uses_ungranted_cap.core.json`); the real M2
//! checker must reject it, so it can never reach codegen.

use std::path::PathBuf;

use marv_core::ir::Def;
use marv_core::lower_module;
use marv_db::CoreModuleSpec;
use marv_interp::{Program, Value};
use marv_types::{check_def, Code, Severity, World};

/// Absolute path to a file in the repository-level `tests/run/` corpus.
fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/run")
        .join(name)
}

/// Parse and lower a `.mv` file into the triple the backends consume.
fn load_source(name: &str) -> (String, Vec<(String, Def)>, World) {
    let path = corpus(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let module = marv_syntax::parse(&src).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    let lowered = lower_module(&module).unwrap_or_else(|e| panic!("lower {name}: {e}"));
    let world = World::from_module(&lowered);
    let module_path = module.name.join(".");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    (module_path, defs, world)
}

/// Interpret `entry(args)` and extract its integer result (the oracle).
fn interp_i64(
    module_path: &str,
    defs: Vec<(String, Def)>,
    world: World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let arg_strs: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let program = Program::new(module_path, defs, world);
    let outcome = program
        .run(entry, &[], &arg_strs)
        .unwrap_or_else(|e| panic!("interp {entry}: {e}"));
    match outcome.value {
        Value::Int(n) => n,
        other => panic!("interp {entry}: expected an integer, got {other:?}"),
    }
}

/// JIT-compile the module and call `entry(args)` natively.
fn cranelift_i64(module_path: &str, defs: &[(String, Def)], entry: &str, args: &[i64]) -> i64 {
    let jit = marv_codegen_cl::compile(module_path, defs)
        .unwrap_or_else(|e| panic!("cranelift compile: {e}"));
    jit.run_i64(entry, args)
        .unwrap_or_else(|e| panic!("cranelift run {entry}: {e}"))
}

/// The differential corpus: `(file, entry, args, expected)`. Every entry returns
/// an integer and uses only the integer/boolean Core both backends support.
fn corpus_cases() -> Vec<(&'static str, &'static str, Vec<i64>, i64)> {
    vec![
        // nullary entry + curried cross-function calls
        ("arithmetic.mv", "main", vec![], 42),
        // recursion + a single `if`
        ("factorial.mv", "factorial", vec![0], 1),
        ("factorial.mv", "factorial", vec![5], 120),
        ("factorial.mv", "factorial", vec![10], 3_628_800),
        // recursion with two self-calls
        ("fib.mv", "fib", vec![0], 0),
        ("fib.mv", "fib", vec![1], 1),
        ("fib.mv", "fib", vec![10], 55),
        ("fib.mv", "fib", vec![15], 610),
        // mutual-tail recursion via remainder
        ("gcd.mv", "gcd", vec![48, 36], 12),
        ("gcd.mv", "gcd", vec![17, 5], 1),
        ("gcd.mv", "gcd", vec![100, 0], 100),
        // nested `if`/`else if`/`else`
        ("clamp.mv", "clamp", vec![5, 0, 10], 5),
        ("clamp.mv", "clamp", vec![-3, 0, 10], 0),
        ("clamp.mv", "clamp", vec![99, 0, 10], 10),
        // boolean `and`, comparisons
        ("classify.mv", "classify", vec![5], 1),
        ("classify.mv", "classify", vec![-1], 0),
        ("classify.mv", "classify", vec![10], 0),
        // every arithmetic prim + comparisons in one body
        ("ops.mv", "ops", vec![20, 6], 165),
        // a < b, so the else arm: sum - diff = (3+8) - (3-8) = 11 - (-5) = 16
        ("ops.mv", "ops", vec![3, 8], 16),
    ]
}

#[test]
fn interpreter_and_cranelift_agree() {
    for (file, entry, args, expected) in corpus_cases() {
        let (module_path, defs, world) = load_source(file);
        let interp = interp_i64(&module_path, defs.clone(), world, entry, &args);
        let native = cranelift_i64(&module_path, &defs, entry, &args);

        assert_eq!(
            interp, native,
            "backends disagree on {file}:{entry}({args:?}): interp={interp}, cranelift={native}"
        );
        assert_eq!(
            interp, expected,
            "{file}:{entry}({args:?}) produced {interp}, expected {expected}"
        );
    }
}

/// The gate's negative case: a function that `perform`s `Fs` while declaring the
/// empty (`pure`) effect row must be rejected by the checker — so it can never
/// be handed to either backend.
#[test]
fn capability_outside_effect_row_fails_to_compile() {
    let path = corpus("uses_ungranted_cap.core.json");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let spec: CoreModuleSpec = serde_json::from_str(&src).expect("fixture is valid Core JSON");
    let world = spec.world.build();

    let mut diags = Vec::new();
    for d in &spec.defs {
        diags.extend(check_def(&world, &d.def, Some(&d.name)));
    }

    let missing_cap = diags
        .iter()
        .find(|d| d.code == Code::MissingCapability)
        .expect("checker must report the missing capability");
    assert_eq!(missing_cap.severity, Severity::Error);
    assert!(
        diags.iter().any(|d| d.severity == Severity::Error),
        "the snapshot must fail to compile"
    );
}
