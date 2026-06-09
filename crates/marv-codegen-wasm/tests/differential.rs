//! M5 acceptance gate: the WebAssembly backend agrees with the interpreter
//! oracle on the M4 corpus (run here through wasmtime), and a pure module
//! imports nothing while a capability-using module surfaces exactly the
//! capabilities it needs as host imports (`spec/01` §9).

use std::path::PathBuf;

use marv_core::ir::*;
use marv_core::{lower_module, symbol_hash};
use marv_interp::{Program, Value};
use marv_types::{OpSig, World, WorldBuilder};
use wasmtime::{Engine, Instance, Module, Store, Val};

/// Absolute path into the repository-level `tests/run/` corpus.
fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/run")
        .join(name)
}

/// Parse and lower a `.mv` file into `(module_path, defs, world)`.
fn load_source(name: &str) -> (String, Vec<(String, Def)>, World) {
    let src = std::fs::read_to_string(corpus(name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let module = marv_syntax::parse(&src).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    let lowered = lower_module(&module).unwrap_or_else(|e| panic!("lower {name}: {e}"));
    let world = World::from_module(&lowered);
    let module_path = module.name.join(".");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    (module_path, defs, world)
}

fn interp_i64(
    module_path: &str,
    defs: Vec<(String, Def)>,
    world: World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let arg_strs: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let program = Program::new(module_path, defs, world);
    match program.run(entry, &[], &arg_strs).expect("interp").value {
        Value::Int(n) => n,
        other => panic!("interp {entry}: expected integer, got {other:?}"),
    }
}

/// Compile to wasm, instantiate under wasmtime (no imports), and call `entry`.
fn wasm_i64(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let artifact = marv_codegen_wasm::compile(module_path, defs, world).expect("wasm compile");
    let engine = Engine::default();
    let module = Module::new(&engine, &artifact.bytes).expect("wasmtime module");
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).expect("instantiate (no imports)");
    let qualified = if module_path.is_empty() || entry.contains('.') {
        entry.to_string()
    } else {
        format!("{module_path}.{entry}")
    };
    let func = instance
        .get_func(&mut store, &qualified)
        .unwrap_or_else(|| panic!("export `{qualified}` not found"));
    let params: Vec<Val> = args.iter().map(|a| Val::I64(*a)).collect();
    let mut results = [Val::I64(0)];
    func.call(&mut store, &params, &mut results)
        .unwrap_or_else(|e| panic!("call {qualified}: {e}"));
    results[0].unwrap_i64()
}

fn corpus_cases() -> Vec<(&'static str, &'static str, Vec<i64>, i64)> {
    vec![
        ("arithmetic.mv", "main", vec![], 42),
        ("factorial.mv", "factorial", vec![0], 1),
        ("factorial.mv", "factorial", vec![5], 120),
        ("factorial.mv", "factorial", vec![10], 3_628_800),
        ("fib.mv", "fib", vec![0], 0),
        ("fib.mv", "fib", vec![1], 1),
        ("fib.mv", "fib", vec![10], 55),
        ("fib.mv", "fib", vec![15], 610),
        ("gcd.mv", "gcd", vec![48, 36], 12),
        ("gcd.mv", "gcd", vec![17, 5], 1),
        ("gcd.mv", "gcd", vec![100, 0], 100),
        ("clamp.mv", "clamp", vec![5, 0, 10], 5),
        ("clamp.mv", "clamp", vec![-3, 0, 10], 0),
        ("clamp.mv", "clamp", vec![99, 0, 10], 10),
        ("classify.mv", "classify", vec![5], 1),
        ("classify.mv", "classify", vec![-1], 0),
        ("classify.mv", "classify", vec![10], 0),
        ("ops.mv", "ops", vec![20, 6], 165),
        ("ops.mv", "ops", vec![3, 8], 16),
        // `while` loops carrying two `var`s (sum, i) → `Core::Loop` (MARV-2)
        ("loops.mv", "sum_to", vec![0], 0),
        ("loops.mv", "sum_to", vec![1], 1),
        ("loops.mv", "sum_to", vec![5], 15),
        ("loops.mv", "sum_to", vec![10], 55),
        ("loops.mv", "sum_to", vec![100], 5050),
        ("loops.mv", "pow", vec![2, 10], 1024),
        ("loops.mv", "pow", vec![3, 4], 81),
        ("loops.mv", "pow", vec![5, 0], 1),
        // single-carried-variable loop with no invariant (the `k == 1` path)
        ("loops.mv", "count_down", vec![7], 0),
        ("loops.mv", "count_down", vec![0], 0),
        // `as` casts (MARV-7): width truncation/wrapping must agree with the
        // interpreter's `eval_cast` bit-for-bit.
        ("casts.mv", "truncate_u8", vec![300], 44),
        ("casts.mv", "truncate_u8", vec![255], 255),
        ("casts.mv", "truncate_u8", vec![256], 0),
        ("casts.mv", "truncate_i8", vec![200], -56),
        ("casts.mv", "truncate_i8", vec![127], 127),
        ("casts.mv", "truncate_i8", vec![128], -128),
        ("casts.mv", "truncate_u16", vec![70000], 4464),
        ("casts.mv", "truncate_i32", vec![4_294_967_301], 5),
        ("casts.mv", "bool_cast", vec![0], 0),
        ("casts.mv", "bool_cast", vec![7], 1),
        ("casts.mv", "char_round", vec![65], 65),
        ("casts.mv", "chained", vec![300], 44),
        // prefix unary operators (MARV-23): `-e` and `not e`.
        ("unary.mv", "neg", vec![5], -5),
        ("unary.mv", "neg", vec![-3], 3),
        ("unary.mv", "abs", vec![-7], 7),
        ("unary.mv", "abs", vec![4], 4),
        ("unary.mv", "flip", vec![5], 1),
        ("unary.mv", "flip", vec![0], 0),
        // Aggregates & enums (MARV-9): boxed `[tag, fields…]` in linear memory,
        // crossing boundaries, projected, and matched. interp == cranelift == wasm.
        ("structs.mv", "manhattan", vec![3, 4], 7),
        ("structs.mv", "manhattan", vec![10, 20], 30),
        ("structs.mv", "manhattan", vec![-5, 5], 0),
        ("color.mv", "rank_of", vec![0], 1),
        ("color.mv", "rank_of", vec![1], 2),
        ("color.mv", "rank_of", vec![2], 3),
        ("shapes.mv", "circle_area", vec![5], 25),
        ("shapes.mv", "circle_area", vec![0], 0),
        ("shapes.mv", "rect_area", vec![3, 4], 12),
        ("shapes.mv", "rect_area", vec![7, 6], 42),
        // Monomorphized generics (MARV-26 / MARV-5): `max[T: Ord]` matching on the
        // `Ordering` enum, specialized to `i64` and dispatched to `impl Ord[i64]`.
        // Runnable here only since MARV-9 gave the enum a linear-memory layout.
        ("generics.mv", "max_of", vec![3, 7], 7),
        ("generics.mv", "max_of", vec![7, 3], 7),
        ("generics.mv", "max_of", vec![5, 5], 5),
        ("generics.mv", "max_of", vec![-4, -9], -4),
        // Arrays (MARV-30): array literals box to `[len, e0, …]`; `len` reads the
        // header word and `index` loads `[i + 1]`; an index *store* is a functional
        // element update (unrolled over the static length). interp == cranelift == wasm.
        // literal + indexed reads + arithmetic.
        ("arrays.mv", "sum3", vec![], 42),
        // index with a runtime subscript.
        ("arrays.mv", "nth", vec![0], 5),
        ("arrays.mv", "nth", vec![3], 8),
        // `len` over an array (the header word).
        ("arrays.mv", "length", vec![], 4),
        // `len` + index driving a `while` loop.
        ("arrays.mv", "sum_all", vec![], 15),
        // index store `a[i] = e` with a constant subscript, then read back.
        ("arrays.mv", "set_get", vec![], 42),
        // index store with a runtime subscript, then sum every element.
        ("arrays.mv", "set_sum", vec![0], 15),
        ("arrays.mv", "set_sum", vec![1], 14),
        ("arrays.mv", "set_sum", vec![2], 13),
        // `for x in a` over an array (desugared len/index loop).
        ("arrays.mv", "sum_for", vec![], 20),
        // Runtime-length slices (MARV-33): a slice shares the array's `[len, e0, …]`
        // layout but with a length known only at run time. `len`/`index` reads fall
        // out of that layout; the element *store* goes through `Core::IndexSet` —
        // an allocate-copy-store over the runtime length, not the array's static
        // unroll. interp == cranelift == wasm.
        // literal + indexed reads.
        ("slices.mv", "sum3", vec![], 42),
        // index with a runtime subscript.
        ("slices.mv", "nth", vec![0], 5),
        ("slices.mv", "nth", vec![3], 8),
        // `len` over a slice (the header word).
        ("slices.mv", "length", vec![], 4),
        // `len` + index driving a `while` loop.
        ("slices.mv", "sum_all", vec![], 15),
        // runtime-length element store with a constant subscript, then read back.
        ("slices.mv", "set_get", vec![], 42),
        // runtime-length element store with a runtime subscript, then sum back.
        ("slices.mv", "set_sum", vec![0], 15),
        ("slices.mv", "set_sum", vec![1], 14),
        ("slices.mv", "set_sum", vec![2], 13),
        // `examples/report.mv`'s `total`: a `while` over `len(sales)` reading
        // `sales[i].amount` from a slice of structs (MARV-33 + MARV-20 slice half).
        ("slices.mv", "total", vec![], 42),
    ]
}

#[test]
fn wasm_agrees_with_interpreter() {
    for (file, entry, args, expected) in corpus_cases() {
        let (module_path, defs, world) = load_source(file);
        let interp = interp_i64(&module_path, defs.clone(), world.clone(), entry, &args);
        let wasm = wasm_i64(&module_path, &defs, &world, entry, &args);
        assert_eq!(
            interp, wasm,
            "interp/wasm disagree on {file}:{entry}({args:?}): interp={interp}, wasm={wasm}"
        );
        assert_eq!(
            interp, expected,
            "{file}:{entry}({args:?}) = {interp}, expected {expected}"
        );
    }
}

#[test]
fn a_pure_module_imports_nothing() {
    let (module_path, defs, world) = load_source("factorial.mv");
    let artifact = marv_codegen_wasm::compile(&module_path, &defs, &world).unwrap();
    assert!(
        artifact.imports.is_empty(),
        "a pure module must import no capabilities, got {:?}",
        artifact.imports
    );
    // And it validates / instantiates with no imports at all.
    let engine = Engine::default();
    let module = Module::new(&engine, &artifact.bytes).unwrap();
    assert!(Instance::new(&mut Store::new(&engine, ()), &module, &[]).is_ok());
}

/// A `fetch(net: Net)` that performs `Net` op 0 must surface exactly one host
/// import — `Net`. This is the capability manifest the sandbox is built on.
#[test]
fn a_capability_using_module_imports_that_capability() {
    let net = symbol_hash("Net");
    let net_ty = Type::Nominal {
        def: net,
        args: Vec::new(),
    };
    let row = EffectRow {
        caps: vec![net],
        errors: Vec::new(),
    };
    // fetch : Net -{Net}-> () ; body = \net. perform net.op0()
    let def = Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(net_ty.clone()),
            ret: Box::new(Type::Unit),
            effects: row.clone(),
        },
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(Core::Lam {
            param: net_ty,
            effects: row,
            body: Box::new(Core::Perform {
                cap: Atom::Var(0),
                op: OpId(0),
                args: Vec::new(),
            }),
        }),
    };
    let world = WorldBuilder::new()
        .cap(
            "Net",
            vec![OpSig {
                params: Vec::new(),
                ret: Type::Unit,
                errors: Vec::new(),
            }],
        )
        .build();

    let artifact =
        marv_codegen_wasm::compile("sandbox", &[("fetch".to_string(), def)], &world).unwrap();
    assert_eq!(artifact.imports.len(), 1);
    assert_eq!(artifact.imports[0].cap, "Net");
    assert_eq!(artifact.imports[0].op, 0);

    // wasmtime confirms the module declares an import named ("Net", "op0").
    let engine = Engine::default();
    let module = Module::new(&engine, &artifact.bytes).unwrap();
    let imports: Vec<(String, String)> = module
        .imports()
        .map(|i| (i.module().to_string(), i.name().to_string()))
        .collect();
    assert_eq!(imports, vec![("Net".to_string(), "op0".to_string())]);
}
