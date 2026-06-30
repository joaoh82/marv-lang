//! M7 self-hosting gate: the first compiler routine ported to marv — the
//! interpreter's total-primitive kernel `eval_prim` (`selfhost/prim_eval.mv`) —
//! must match the Rust Stage-0 kernel, which stays the differential oracle.
//!
//! The **oracle** is the real Rust interpreter evaluating a `Core::Prim{op,[a,b]}`
//! (so it exercises Stage-0's actual `eval_prim`). The **candidate** is the marv
//! `eval_prim(op, a, b)` run through that same interpreter. The operand matrix
//! includes every primitive and the exact operations the M4 corpus performs
//! (`factorial`'s `*`/`-`/`<`, `gcd`'s `%`/`==`, `clamp`'s `<`/`>`, `ops`'
//! `+ - * / %` and `>=`, `classify`'s `and`), so "matches on the M4 corpus"
//! is concrete.

use std::path::PathBuf;

use marv_core::ir::*;
use marv_core::{lower_module, lower_modules};
use marv_interp::{Program, RunError, Value};
use marv_syntax::Module;
use marv_types::{check_def, Severity, World};

/// PrimOp tag (the stable content-encoding tag from `marv-core`) → the op, with
/// flags for how the oracle must be fed.
struct Op {
    code: i64,
    prim: PrimOp,
    /// Operands/result are booleans (And/Or).
    boolean: bool,
}

fn ops() -> Vec<Op> {
    use PrimOp::*;
    vec![
        Op {
            code: 0,
            prim: Add,
            boolean: false,
        },
        Op {
            code: 1,
            prim: Sub,
            boolean: false,
        },
        Op {
            code: 2,
            prim: Mul,
            boolean: false,
        },
        Op {
            code: 3,
            prim: Div,
            boolean: false,
        },
        Op {
            code: 4,
            prim: Rem,
            boolean: false,
        },
        Op {
            code: 5,
            prim: Eq,
            boolean: false,
        },
        Op {
            code: 6,
            prim: Ne,
            boolean: false,
        },
        Op {
            code: 7,
            prim: Lt,
            boolean: false,
        },
        Op {
            code: 8,
            prim: Le,
            boolean: false,
        },
        Op {
            code: 9,
            prim: Gt,
            boolean: false,
        },
        Op {
            code: 10,
            prim: Ge,
            boolean: false,
        },
        Op {
            code: 11,
            prim: And,
            boolean: true,
        },
        Op {
            code: 12,
            prim: Or,
            boolean: true,
        },
    ]
}

/// Build the candidate program from the ported marv source.
fn candidate_program() -> Program {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../selfhost/prim_eval.mv");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let module = marv_syntax::parse(&src).expect("parse prim_eval.mv");
    let lowered = lower_module(&module).expect("lower prim_eval.mv");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    Program::new("selfhost", defs, World::new())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

fn parse_file(path: PathBuf) -> Module {
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    marv_syntax::parse(&src).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

fn selfhost_model_program() -> Program {
    let root = repo_root();
    let model_path = root.join("selfhost/model.mv");
    let model_src =
        std::fs::read_to_string(&model_path).unwrap_or_else(|e| panic!("read {model_path:?}: {e}"));
    let model = marv_syntax::parse(&model_src).expect("parse model.mv");
    assert_eq!(
        marv_syntax::format_module(&model),
        model_src,
        "selfhost/model.mv must be in canonical form"
    );

    let std_io = parse_file(root.join("std/capabilities.mv"));
    let std_collections = parse_file(root.join("std/collections.mv"));
    let modules = vec![model, std_io, std_collections];
    let lowered = lower_modules(&modules).expect("lower selfhost model + std");
    let world = World::from_modules(&lowered);
    let model_lowered = lowered
        .iter()
        .find(|module| module.module == ["selfhost".to_string(), "model".to_string()])
        .expect("lowered selfhost.model");
    let mut errors = Vec::new();
    for entry in &model_lowered.defs {
        errors.extend(
            check_def(&world, &entry.def, Some(&entry.name))
                .into_iter()
                .filter(|d| d.severity == Severity::Error),
        );
    }
    assert!(
        errors.is_empty(),
        "selfhost.model must check clean: {errors:?}"
    );

    let mut defs = Vec::new();
    for (module, lowered_module) in modules.iter().zip(lowered.into_iter()) {
        let prefix = module.name.join(".");
        for entry in lowered_module.defs {
            let name = if prefix == "selfhost.model" {
                entry.name
            } else {
                format!("{prefix}.{}", entry.name)
            };
            defs.push((name, entry.def));
        }
    }
    Program::new("selfhost.model", defs, world)
}

fn selfhost_parser_program() -> Program {
    let root = repo_root();
    let parser_path = root.join("selfhost/parser.mv");
    let parser_src = std::fs::read_to_string(&parser_path)
        .unwrap_or_else(|e| panic!("read {parser_path:?}: {e}"));
    let parser = marv_syntax::parse(&parser_src).expect("parse parser.mv");
    assert_eq!(
        marv_syntax::format_module(&parser),
        parser_src,
        "selfhost/parser.mv must be in canonical form"
    );

    let model = parse_file(root.join("selfhost/model.mv"));
    let std_io = parse_file(root.join("std/capabilities.mv"));
    let std_collections = parse_file(root.join("std/collections.mv"));
    let modules = vec![model, parser, std_io, std_collections];
    let lowered = lower_modules(&modules).expect("lower selfhost parser + model + std");
    let world = World::from_modules(&lowered);
    let parser_lowered = lowered
        .iter()
        .find(|module| module.module == ["selfhost".to_string(), "parser".to_string()])
        .expect("lowered selfhost.parser");
    let mut errors = Vec::new();
    for entry in &parser_lowered.defs {
        errors.extend(
            check_def(&world, &entry.def, Some(&entry.name))
                .into_iter()
                .filter(|d| d.severity == Severity::Error),
        );
    }
    assert!(
        errors.is_empty(),
        "selfhost.parser must check clean: {errors:?}"
    );

    let mut defs = Vec::new();
    for (module, lowered_module) in modules.iter().zip(lowered.into_iter()) {
        let prefix = module.name.join(".");
        for entry in lowered_module.defs {
            let name = if prefix == "selfhost.parser" {
                entry.name
            } else {
                format!("{prefix}.{}", entry.name)
            };
            defs.push((name, entry.def));
        }
    }
    Program::new("selfhost.parser", defs, world)
}

fn run_model_score(prog: &Program, entry: &str) -> i64 {
    match prog
        .run(entry, &["Alloc".to_string()], &[])
        .unwrap_or_else(|e| panic!("run {entry}: {e:?}"))
        .value
    {
        Value::Int(n) => n,
        other => panic!("{entry} produced {other:?}"),
    }
}

fn run_parser_score(prog: &Program, entry: &str, source: &str) -> i64 {
    match prog
        .run(entry, &["Alloc".to_string()], &[source.to_string()])
        .unwrap_or_else(|e| panic!("run {entry}: {e:?}"))
        .value
    {
        Value::Int(n) => n,
        other => panic!("{entry} produced {other:?}"),
    }
}

/// The Rust Stage-0 oracle: evaluate `Prim{op,[a,b]}` through the real
/// interpreter and return its result as an i64 (booleans as 0/1).
fn oracle(prim: PrimOp, a: i64, b: i64, boolean: bool) -> i64 {
    let lit = |v: i64| {
        if boolean {
            Atom::Lit(Literal::Bool(v != 0))
        } else {
            Atom::Lit(Literal::Int(v))
        }
    };
    // fn oracle(()) -> i64 { prim(a, b) }
    let body = Core::Lam {
        param: Type::Unit,
        effects: EffectRow::empty(),
        body: Box::new(Core::Prim {
            op: prim,
            args: vec![lit(a), lit(b)],
        }),
    };
    let def = Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(Type::Unit),
            ret: Box::new(Type::Int(IntTy::I64)),
            effects: EffectRow::empty(),
        },
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(body),
    };
    let program = Program::new("", vec![("oracle".to_string(), def)], World::new());
    match program.run("oracle", &[], &[]).expect("oracle run").value {
        Value::Int(n) => n,
        Value::Bool(b) => b as i64,
        other => panic!("oracle produced {other:?}"),
    }
}

/// The marv candidate: run `eval_prim(op, a, b)` via the interpreter.
fn candidate(prog: &Program, code: i64, a: i64, b: i64) -> i64 {
    let args = [code.to_string(), a.to_string(), b.to_string()];
    match prog
        .run("eval_prim", &[], &args)
        .expect("candidate run")
        .value
    {
        Value::Int(n) => n,
        other => panic!("candidate produced {other:?}"),
    }
}

#[test]
fn ported_eval_prim_matches_the_rust_oracle() {
    let prog = candidate_program();
    // Operands include the M4 corpus's values and a spread of signs/magnitudes.
    let operands: [i64; 13] = [-5, -1, 0, 1, 2, 3, 6, 7, 8, 10, 15, 36, 48];

    for op in ops() {
        for &a in &operands {
            for &b in &operands {
                // Skip division/remainder by zero (a runtime trap, not a kernel
                // disagreement — both backends would fault identically).
                if (op.prim == PrimOp::Div || op.prim == PrimOp::Rem) && b == 0 {
                    continue;
                }
                let (oa, ob) = if op.boolean {
                    (a.signum().abs().min(1), b.signum().abs().min(1)) // 0/1
                } else {
                    (a, b)
                };
                let want = oracle(op.prim, oa, ob, op.boolean);
                let got = candidate(&prog, op.code, oa, ob);
                assert_eq!(
                    got, want,
                    "eval_prim(op={}, {oa}, {ob}): marv={got}, oracle={want}",
                    op.code
                );
            }
        }
    }
}

#[test]
fn selfhost_model_constructs_and_traverses_representative_ast_and_core_values() {
    let prog = selfhost_model_program();

    assert_eq!(run_model_score(&prog, "representative_ast_score"), 37);
    assert_eq!(run_model_score(&prog, "representative_core_score"), 22);
    assert_eq!(run_model_score(&prog, "selfhost_model_score"), 59);
}

const TINY_FRONTEND_SRC: &str = "mod demo\n\npure fn id(n: i64) -> i64 {\n    n\n}\n";

#[test]
fn selfhost_parser_tiny_slice_matches_stage0_for_supported_fixture() {
    let oracle = marv_syntax::parse(TINY_FRONTEND_SRC).expect("stage-0 parses tiny fixture");
    assert_eq!(oracle.name, ["demo".to_string()]);
    assert!(oracle.imports.is_empty());
    let f = match oracle.items.as_slice() {
        [marv_syntax::Item::Fn(f)] => f,
        other => panic!("expected one function, got {other:?}"),
    };
    assert!(f.is_pure);
    assert_eq!(f.name, "id");
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.params[0].name, "n");
    assert_eq!(
        f.params[0].ty,
        marv_syntax::Type::Named(vec!["i64".to_string()])
    );
    assert_eq!(
        f.ret,
        Some(marv_syntax::Type::Named(vec!["i64".to_string()]))
    );
    let body = f.body.as_ref().expect("function body");
    match body.tail.as_ref().expect("body tail") {
        marv_syntax::Tail::Expr(marv_syntax::Expr::Var(name)) => assert_eq!(name, "n"),
        other => panic!("expected `n` tail expression, got {other:?}"),
    }

    let prog = selfhost_parser_program();
    let token_count = run_parser_score(&prog, "lex_tiny_fingerprint", TINY_FRONTEND_SRC);
    assert_eq!(token_count, 16);
    assert_eq!(
        run_parser_score(&prog, "parse_tiny_fingerprint", TINY_FRONTEND_SRC),
        4 + 2 + token_count + 10 + 2 + TINY_FRONTEND_SRC.len() as i64
    );
}

#[test]
fn selfhost_parser_unsupported_forms_fail_with_frontend_error() {
    let prog = selfhost_parser_program();
    let err = prog
        .run(
            "unsupported_tiny_parse",
            &["Alloc".to_string()],
            &["mod demo\n\nstruct Point { x: i64 }\n".to_string()],
        )
        .expect_err("unsupported grammar must fail honestly");
    assert_eq!(err, RunError::Uncaught("FrontendError".to_string()));
}
