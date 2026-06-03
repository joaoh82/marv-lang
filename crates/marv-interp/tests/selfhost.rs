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
use marv_core::lower_module;
use marv_interp::{Program, Value};
use marv_types::World;

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
