//! M0 acceptance gate: the round-trip property `parse ∘ format == id`.
//!
//! A tiny deterministic LCG generator (no external crates — proptest can be
//! swapped in later) produces many random in-subset ASTs. For each one we assert:
//!
//! - **Round-trip:** `parse(format_module(ast)) == ast`. The canonical text the
//!   formatter emits parses back to the *identical* AST — text ⇄ AST is bijective.
//! - **Idempotence:** `format(format_module(ast)) == format_module(ast)`.
//!   Formatting canonical output is a no-op.
//!
//! The generator only emits constructs the M0 subset supports, and respects the
//! two designed-out ambiguities: it never emits a bare expression-statement (a
//! standalone expression is only ever a block tail), and `return` is terminal
//! (nothing is generated after it).

use marv_syntax::ast::*;
use marv_syntax::{format, format_module, parse};

// ---- deterministic LCG ---------------------------------------------------

/// A minimal linear-congruential generator. Deterministic and dependency-free;
/// good enough to drive structural fuzzing of the AST.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Mix the seed so small seeds still produce well-spread output.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u32(&mut self) -> u32 {
        // Knuth/PCG-style multiplier + odd increment; take high bits, which have
        // the best statistical quality in a bare LCG.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    /// A value in `[0, n)`.
    fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n.max(1)
    }

    /// True with probability `num / den`.
    fn chance(&mut self, num: u32, den: u32) -> bool {
        self.below(den) < num
    }

    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        xs[self.below(xs.len() as u32) as usize]
    }
}

// ---- name / token pools --------------------------------------------------

const IDENTS: &[&str] = &[
    "a", "b", "c", "x", "y", "z", "foo", "bar", "baz", "n", "sum", "item", "count", "value", "tmp",
    "out", "lo", "hi",
];
const TYPE_NAMES: &[&str] = &["i32", "i64", "u32", "usize", "str", "bool", "Foo", "Bar", "Sale", "Io"];
const STRUCT_NAMES: &[&str] = &["Point", "Sale", "Config", "Pair", "Record", "Span"];
const FN_NAMES: &[&str] = &["main", "run", "total", "add", "step", "build", "load", "clamp"];
const MOD_PARTS: &[&str] = &["std", "core", "io", "mem", "fmt", "collections", "demo", "app"];
// Characters that exercise the string escaper/unescaper round-trip.
const STR_CHARS: &[char] = &['a', 'Z', '0', '9', ' ', '"', '\\', '\n', '\t'];

const ALL_BINOPS: &[BinOp] = &[
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::Rem,
    BinOp::Eq,
    BinOp::Ne,
    BinOp::Lt,
    BinOp::Le,
    BinOp::Gt,
    BinOp::Ge,
    BinOp::And,
    BinOp::Or,
];

// ---- generators ----------------------------------------------------------

fn gen_module(rng: &mut Rng) -> Module {
    let name = gen_path(rng, MOD_PARTS, 1, 2);

    let mut imports = Vec::new();
    for _ in 0..rng.below(4) {
        imports.push(gen_import(rng));
    }

    let mut items = Vec::new();
    for _ in 0..(1 + rng.below(3)) {
        items.push(gen_item(rng));
    }

    Module {
        name,
        imports,
        items,
    }
}

fn gen_path(rng: &mut Rng, pool: &[&str], min: u32, max: u32) -> Path {
    let len = min + rng.below(max - min + 1);
    (0..len).map(|_| rng.pick(pool).to_string()).collect()
}

fn gen_import(rng: &mut Rng) -> Import {
    let path = gen_path(rng, MOD_PARTS, 1, 3);
    let names = if rng.chance(1, 2) {
        let count = 1 + rng.below(3);
        Some((0..count).map(|_| rng.pick(TYPE_NAMES).to_string()).collect())
    } else {
        None
    };
    Import { path, names }
}

fn gen_item(rng: &mut Rng) -> Item {
    if rng.chance(1, 2) {
        Item::Struct(gen_struct(rng))
    } else {
        Item::Fn(gen_fn(rng))
    }
}

fn gen_struct(rng: &mut Rng) -> StructDecl {
    let linear = rng.chance(1, 4);
    let name = rng.pick(STRUCT_NAMES).to_string();
    let fields = (0..rng.below(4))
        .map(|_| Field {
            name: rng.pick(IDENTS).to_string(),
            ty: gen_type(rng, 2),
        })
        .collect();
    StructDecl {
        linear,
        name,
        fields,
    }
}

fn gen_fn(rng: &mut Rng) -> FnDecl {
    let is_pure = rng.chance(1, 3);
    let name = rng.pick(FN_NAMES).to_string();
    let params = (0..rng.below(4))
        .map(|_| Param {
            name: rng.pick(IDENTS).to_string(),
            ty: gen_type(rng, 2),
        })
        .collect();
    let ret = if rng.chance(1, 2) {
        Some(gen_type(rng, 2))
    } else {
        None
    };
    let body = gen_block(rng, 3);
    FnDecl {
        is_pure,
        name,
        params,
        ret,
        body,
    }
}

fn gen_type(rng: &mut Rng, depth: u32) -> Type {
    if depth == 0 {
        return gen_atom_type(rng);
    }
    match rng.below(5) {
        0 => Type::Slice(Box::new(gen_type(rng, depth - 1))),
        1 => Type::Ref {
            mutable: rng.chance(1, 2),
            // References wrap a base type (`spec/02` §B `ref_type`); no `&&T`.
            inner: Box::new(gen_atom_type(rng)),
        },
        _ => gen_atom_type(rng),
    }
}

fn gen_atom_type(rng: &mut Rng) -> Type {
    if rng.chance(1, 6) {
        Type::Unit
    } else {
        Type::Named(gen_path(rng, TYPE_NAMES, 1, 1))
    }
}

fn gen_block(rng: &mut Rng, depth: u32) -> Block {
    let stmts = (0..rng.below(4)).map(|_| gen_stmt(rng)).collect();

    let tail = match rng.below(5) {
        0 => None,
        1 => Some(Tail::Return(if rng.chance(1, 2) {
            Some(gen_expr(rng, 2))
        } else {
            None
        })),
        2 if depth > 0 => Some(Tail::If(Box::new(gen_if(rng, depth - 1)))),
        _ => Some(Tail::Expr(gen_expr(rng, 2))),
    };

    Block { stmts, tail }
}

fn gen_stmt(rng: &mut Rng) -> Stmt {
    let name = rng.pick(IDENTS).to_string();
    let ty = if rng.chance(1, 2) {
        Some(gen_type(rng, 1))
    } else {
        None
    };
    let value = gen_expr(rng, 2);
    if rng.chance(1, 2) {
        Stmt::Let { name, ty, value }
    } else {
        Stmt::Var { name, ty, value }
    }
}

fn gen_if(rng: &mut Rng, depth: u32) -> IfExpr {
    let cond = gen_expr(rng, 2);
    let then = gen_block(rng, depth);
    let els = match rng.below(3) {
        0 => None,
        1 if depth > 0 => Some(Else::If(Box::new(gen_if(rng, depth - 1)))),
        _ => Some(Else::Block(gen_block(rng, depth))),
    };
    IfExpr { cond, then, els }
}

fn gen_expr(rng: &mut Rng, depth: u32) -> Expr {
    if depth == 0 {
        return gen_atom_expr(rng);
    }
    match rng.below(6) {
        0 => Expr::Binary(
            Box::new(gen_expr(rng, depth - 1)),
            *pick_binop(rng),
            Box::new(gen_expr(rng, depth - 1)),
        ),
        1 => {
            let callee = gen_postfix(rng, depth - 1);
            let args = (0..rng.below(3)).map(|_| gen_expr(rng, depth - 1)).collect();
            Expr::Call(Box::new(callee), args)
        }
        2 => Expr::Field(
            Box::new(gen_postfix(rng, depth - 1)),
            rng.pick(IDENTS).to_string(),
        ),
        _ => gen_atom_expr(rng),
    }
}

/// A var/field/call chain — always a valid callee or field base.
fn gen_postfix(rng: &mut Rng, depth: u32) -> Expr {
    let mut e = Expr::Var(rng.pick(IDENTS).to_string());
    for _ in 0..rng.below(3) {
        if depth == 0 || rng.chance(1, 2) {
            e = Expr::Field(Box::new(e), rng.pick(IDENTS).to_string());
        } else {
            let args = (0..rng.below(3)).map(|_| gen_atom_expr(rng)).collect();
            e = Expr::Call(Box::new(e), args);
        }
    }
    e
}

fn gen_atom_expr(rng: &mut Rng) -> Expr {
    match rng.below(6) {
        0 => Expr::Int(rng.below(10000) as i64),
        1 => Expr::Bool(rng.chance(1, 2)),
        2 => Expr::Str(gen_string(rng)),
        3 => Expr::Unit,
        _ => Expr::Var(rng.pick(IDENTS).to_string()),
    }
}

fn gen_string(rng: &mut Rng) -> String {
    (0..rng.below(6))
        .map(|_| STR_CHARS[rng.below(STR_CHARS.len() as u32) as usize])
        .collect()
}

fn pick_binop(rng: &mut Rng) -> &BinOp {
    &ALL_BINOPS[rng.below(ALL_BINOPS.len() as u32) as usize]
}

// ---- the gates -----------------------------------------------------------

const ITERATIONS: usize = 5000;

#[test]
fn parse_format_roundtrip() {
    let mut rng = Rng::new(0x0DDB1A5E5BAD5EED);
    for i in 0..ITERATIONS {
        let ast = gen_module(&mut rng);
        let text = format_module(&ast);
        let parsed = parse(&text).unwrap_or_else(|e| {
            panic!("iteration {i}: parse failed: {e}\n--- text ---\n{text}\n--- ast ---\n{ast:#?}")
        });
        assert_eq!(
            parsed, ast,
            "iteration {i}: parse(format(ast)) != ast\n--- text ---\n{text}"
        );
    }
}

#[test]
fn format_is_idempotent() {
    let mut rng = Rng::new(0xCAFEF00DDEADBEEF);
    for i in 0..ITERATIONS {
        let ast = gen_module(&mut rng);
        let once = format_module(&ast);
        let twice = format(&once);
        assert_eq!(once, twice, "iteration {i}: format not idempotent\n{once}");
    }
}
