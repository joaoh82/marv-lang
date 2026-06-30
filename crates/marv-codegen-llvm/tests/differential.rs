use std::path::PathBuf;
use std::process::Command;

use marv_core::ir::Def;
use marv_core::lower_module;
use marv_interp::{Program, Value};
use marv_types::World;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/run")
        .join(name)
}

fn clang_available() -> bool {
    Command::new("clang").arg("--version").output().is_ok()
}

fn load_source(name: &str) -> (String, Vec<(String, Def)>, World) {
    let path = corpus(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let module = marv_syntax::parse(&src).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    let module_path = module.name.join(".");
    let lowered = lower_module(&module).unwrap_or_else(|e| panic!("lower {name}: {e}"));
    let world = World::from_modules(std::slice::from_ref(&lowered));
    let defs = lowered
        .defs
        .into_iter()
        .map(|entry| (entry.name, entry.def))
        .collect();
    (module_path, defs, world)
}

fn interp_i64(
    module_path: &str,
    defs: Vec<(String, Def)>,
    world: World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let arg_strs = args.iter().map(i64::to_string).collect::<Vec<_>>();
    let outcome = Program::new(module_path, defs, world)
        .run(entry, &[], &arg_strs)
        .unwrap_or_else(|e| panic!("interp {entry}: {e}"));
    match outcome.value {
        Value::Int(n) => n,
        other => panic!("interp {entry}: expected integer, got {other:?}"),
    }
}

fn llvm_i64(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let program = marv_codegen_llvm::compile_reachable(
        module_path,
        defs,
        world,
        &marv_codegen_llvm::Options::default(),
        entry,
    )
    .unwrap_or_else(|e| panic!("llvm compile {entry}: {e}"));
    program
        .run_i64(args)
        .unwrap_or_else(|e| panic!("llvm run {entry}: {e}"))
}

#[test]
fn llvm_agrees_with_interpreter_on_release_slice() {
    if !clang_available() {
        return;
    }
    let cases: &[(&str, &str, &[i64], i64)] = &[
        ("arithmetic.mv", "main", &[], 42),
        ("factorial.mv", "factorial", &[6], 720),
        ("fib.mv", "fib", &[10], 55),
        ("gcd.mv", "gcd", &[48, 36], 12),
        ("ops.mv", "ops", &[20, 6], 165),
        ("unary.mv", "abs", &[-42], 42),
        ("unary.mv", "flip", &[2], 1),
        ("casts.mv", "truncate_u8", &[260], 4),
        ("structs.mv", "manhattan", &[20, 22], 42),
        ("shapes.mv", "rect_area", &[6, 7], 42),
        ("shapes.mv", "circle_area", &[7], 49),
        ("arrays.mv", "sum_all", &[], 15),
        ("arrays.mv", "set_sum", &[1], 14),
        ("loops.mv", "pow", &[2, 10], 1024),
        ("loops.mv", "first_hit", &[9, 4], 4),
    ];
    for (file, entry, args, expected) in cases {
        let (module_path, defs, world) = load_source(file);
        let interp = interp_i64(&module_path, defs.clone(), world.clone(), entry, args);
        assert_eq!(interp, *expected, "{file}:{entry} interpreter");
        let llvm = llvm_i64(&module_path, &defs, &world, entry, args);
        assert_eq!(llvm, interp, "{file}:{entry} llvm");
    }
}

#[test]
fn llvm_reports_reachable_unsupported_constructs() {
    let (module_path, defs, world) = load_source("pruned_sibling.mv");
    let ok = marv_codegen_llvm::compile_reachable(
        &module_path,
        &defs,
        &world,
        &marv_codegen_llvm::Options::default(),
        "double",
    );
    assert!(ok.is_ok(), "unreachable unsupported sibling must be pruned");

    let err = marv_codegen_llvm::compile_reachable(
        &module_path,
        &defs,
        &world,
        &marv_codegen_llvm::Options::default(),
        "nudge",
    )
    .expect_err("reachable unsupported method call should fail");
    assert!(
        err.to_string().contains("unknown global") || err.to_string().contains("unsupported"),
        "unexpected error: {err}"
    );
}
