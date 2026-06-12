//! Content hashing of Core definitions (`spec/02-grammar-and-core-ir.md` §F).
//!
//! The [`Hash`] of a [`Def`] is `blake3`-256 over a **canonical binary
//! encoding** obeying the four §F rules:
//!
//! 1. **De Bruijn, no names.** The Core IR carries no identifiers, so
//!    alpha-equivalent definitions encode — and hash — identically.
//! 2. **Resolved children.** Each [`Atom::Global`] / [`Type::Nominal`] already
//!    carries the 32-byte hash of its target, so a `Def`'s hash transitively
//!    commits to its entire dependency DAG.
//! 3. **Canonical field order.** Set-like collections (effect-row capabilities
//!    and error sets) are sorted before encoding; positional aggregates
//!    (tuples, argument lists, match branches) keep their order, which is
//!    semantically significant.
//! 4. **No incidental data.** Formatting, comments, doc strings and source
//!    spans never reach the encoder.
//!
//! ## Stable tags
//!
//! Every enum is encoded with an explicit one-byte tag chosen here, *decoupled*
//! from the variant's declaration order in [`crate::ir`]. Reordering a Rust enum
//! therefore cannot silently change a content hash — only editing a tag in this
//! file can, which is exactly where such a decision belongs. The whole encoding
//! is additionally domain-separated by a version prefix.

use crate::ir::*;

/// Domain-separation prefix for a `Def` encoding. Bump the version suffix to
/// intentionally invalidate every previously computed hash.
const DEF_DOMAIN: &[u8] = b"marv-core-def-v0";

/// Domain-separation prefix for a *symbol* hash (the stable identity of a
/// cross-definition reference resolved by name; see [`symbol_hash`]).
const SYMBOL_DOMAIN: &[u8] = b"marv-core-sym-v0";

/// A growable canonical-encoding sink. All multi-byte integers are
/// little-endian and all variable-length data is length-prefixed, so the byte
/// stream is unambiguous and platform-independent.
struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Encoder { buf: Vec::new() }
    }

    fn u8(&mut self, b: u8) {
        self.buf.push(b);
    }

    fn u32(&mut self, n: u32) {
        self.buf.extend_from_slice(&n.to_le_bytes());
    }

    fn u64(&mut self, n: u64) {
        self.buf.extend_from_slice(&n.to_le_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// Length-prefixed raw bytes.
    fn blob(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.buf.extend_from_slice(b);
    }

    fn str(&mut self, s: &str) {
        self.blob(s.as_bytes());
    }

    fn bool(&mut self, b: bool) {
        self.u8(b as u8);
    }

    fn hash(&mut self, h: &Hash) {
        self.buf.extend_from_slice(&h.0);
    }
}

/// The content hash of a definition (`spec/02` §F).
pub fn content_hash(def: &Def) -> Hash {
    let mut e = Encoder::new();
    e.bytes(DEF_DOMAIN);
    encode_def(&mut e, def);
    Hash(*blake3::hash(&e.buf).as_bytes())
}

/// The stable *symbol identity* of a cross-definition reference, used by M1 to
/// resolve free value references ([`Atom::Global`]) and nominal type references
/// ([`Type::Nominal`]) to a deterministic 32-byte hash.
///
/// M1 keys this on the resolved (module-qualified) name rather than the target's
/// own content hash: that keeps lowering total and side-steps the cyclic-hashing
/// problem for (mutually) recursive definitions. Replacing symbol hashes with
/// the callee's true Core hash — so structurally identical code deduplicates
/// transitively — is content-store work (M7). Either way the §F encoding rule is
/// honoured: a `Global`/`Nominal` child contributes exactly 32 committed bytes.
pub fn symbol_hash(qualified_name: &str) -> Hash {
    let mut e = Encoder::new();
    e.bytes(SYMBOL_DOMAIN);
    e.str(qualified_name);
    Hash(*blake3::hash(&e.buf).as_bytes())
}

impl Def {
    /// Convenience wrapper around [`content_hash`].
    pub fn content_hash(&self) -> Hash {
        content_hash(self)
    }
}

// ---- encoders -----------------------------------------------------------
//
// Each `encode_*` writes a stable tag byte (where the type is a sum) followed by
// its fields. Tags are explicit constants in the match arms; never reuse a tag.

fn encode_def(e: &mut Encoder, def: &Def) {
    encode_defkind(e, def.kind);
    encode_type(e, &def.ty);
    encode_preds(e, &def.requires);
    encode_preds(e, &def.ensures);
    match &def.body {
        None => e.u8(0),
        Some(body) => {
            e.u8(1);
            encode_core(e, body);
        }
    }
}

fn encode_defkind(e: &mut Encoder, k: DefKind) {
    e.u8(match k {
        DefKind::Fn => 0,
        DefKind::Struct => 1,
        DefKind::Enum => 2,
        DefKind::Interface => 3,
        DefKind::Impl => 4,
        DefKind::Const => 5,
        DefKind::Cap => 6,
        DefKind::Error => 7,
    });
}

fn encode_intty(e: &mut Encoder, t: IntTy) {
    e.u8(match t {
        IntTy::I8 => 0,
        IntTy::I16 => 1,
        IntTy::I32 => 2,
        IntTy::I64 => 3,
        IntTy::Isize => 4,
        IntTy::U8 => 5,
        IntTy::U16 => 6,
        IntTy::U32 => 7,
        IntTy::U64 => 8,
        IntTy::Usize => 9,
    });
}

fn encode_floatty(e: &mut Encoder, t: FloatTy) {
    e.u8(match t {
        FloatTy::F32 => 0,
        FloatTy::F64 => 1,
    });
}

fn encode_type(e: &mut Encoder, t: &Type) {
    match t {
        Type::Unit => e.u8(0),
        Type::Bool => e.u8(1),
        Type::Int(it) => {
            e.u8(2);
            encode_intty(e, *it);
        }
        Type::Float(ft) => {
            e.u8(3);
            encode_floatty(e, *ft);
        }
        Type::Str => e.u8(4),
        Type::Char => e.u8(5),
        Type::Array(inner, n) => {
            e.u8(6);
            encode_type(e, inner);
            e.u64(*n);
        }
        Type::Slice(inner) => {
            e.u8(7);
            encode_type(e, inner);
        }
        Type::Tuple(elems) => {
            e.u8(8);
            e.u64(elems.len() as u64);
            for el in elems {
                encode_type(e, el);
            }
        }
        Type::Arrow {
            param,
            ret,
            effects,
        } => {
            e.u8(9);
            encode_type(e, param);
            encode_type(e, ret);
            encode_effectrow(e, effects);
        }
        Type::Nominal { def, args } => {
            e.u8(10);
            e.hash(def);
            e.u64(args.len() as u64);
            for a in args {
                encode_type(e, a);
            }
        }
        Type::Ref { mutable, of } => {
            e.u8(11);
            e.bool(*mutable);
            encode_type(e, of);
        }
        Type::Linear(inner) => {
            e.u8(12);
            encode_type(e, inner);
        }
        Type::Var(idx) => {
            e.u8(13);
            e.u32(*idx);
        }
    }
}

fn encode_effectrow(e: &mut Encoder, row: &EffectRow) {
    // Rule 3: set-like members are sorted into one canonical order.
    encode_sorted_hashes(e, &row.caps);
    encode_sorted_hashes(e, &row.errors);
}

fn encode_sorted_hashes(e: &mut Encoder, hashes: &[Hash]) {
    let mut sorted: Vec<&Hash> = hashes.iter().collect();
    sorted.sort();
    e.u64(sorted.len() as u64);
    for h in sorted {
        e.hash(h);
    }
}

fn encode_literal(e: &mut Encoder, lit: &Literal) {
    match lit {
        Literal::Unit => e.u8(0),
        Literal::Bool(b) => {
            e.u8(1);
            e.bool(*b);
        }
        Literal::Int(n) => {
            e.u8(2);
            e.u64(*n as u64);
        }
        Literal::Float(bits) => {
            e.u8(3);
            e.u64(*bits);
        }
        Literal::Str(s) => {
            e.u8(4);
            e.str(s);
        }
        Literal::Char(c) => {
            e.u8(5);
            e.u32(*c as u32);
        }
    }
}

fn encode_atom(e: &mut Encoder, a: &Atom) {
    match a {
        Atom::Var(idx) => {
            e.u8(0);
            e.u32(*idx);
        }
        Atom::Global(h) => {
            e.u8(1);
            e.hash(h);
        }
        Atom::Lit(lit) => {
            e.u8(2);
            encode_literal(e, lit);
        }
    }
}

fn encode_atoms(e: &mut Encoder, atoms: &[Atom]) {
    e.u64(atoms.len() as u64);
    for a in atoms {
        encode_atom(e, a);
    }
}

fn encode_primop(e: &mut Encoder, op: PrimOp) {
    e.u8(match op {
        PrimOp::Add => 0,
        PrimOp::Sub => 1,
        PrimOp::Mul => 2,
        PrimOp::Div => 3,
        PrimOp::Rem => 4,
        PrimOp::Eq => 5,
        PrimOp::Ne => 6,
        PrimOp::Lt => 7,
        PrimOp::Le => 8,
        PrimOp::Gt => 9,
        PrimOp::Ge => 10,
        PrimOp::And => 11,
        PrimOp::Or => 12,
        PrimOp::Not => 13,
        PrimOp::Len => 14,
        PrimOp::Index => 15,
        PrimOp::Neg => 16,
    });
}

fn encode_cmpop(e: &mut Encoder, op: CmpOp) {
    e.u8(match op {
        CmpOp::Eq => 0,
        CmpOp::Ne => 1,
        CmpOp::Lt => 2,
        CmpOp::Le => 3,
        CmpOp::Gt => 4,
        CmpOp::Ge => 5,
    });
}

fn encode_core(e: &mut Encoder, c: &Core) {
    match c {
        Core::Atom(a) => {
            e.u8(0);
            encode_atom(e, a);
        }
        Core::Let { value, body } => {
            e.u8(1);
            encode_core(e, value);
            encode_core(e, body);
        }
        Core::Lam {
            param,
            effects,
            body,
        } => {
            e.u8(2);
            encode_type(e, param);
            encode_effectrow(e, effects);
            encode_core(e, body);
        }
        Core::App { func, arg } => {
            e.u8(3);
            encode_atom(e, func);
            encode_atom(e, arg);
        }
        Core::Ctor { ty, tag, fields } => {
            e.u8(4);
            e.hash(ty);
            e.u32(*tag);
            encode_atoms(e, fields);
        }
        Core::Proj { base, idx } => {
            e.u8(5);
            encode_atom(e, base);
            e.u32(*idx);
        }
        Core::Match {
            scrutinee,
            branches,
        } => {
            e.u8(6);
            encode_atom(e, scrutinee);
            e.u64(branches.len() as u64);
            for b in branches {
                e.u32(b.binds);
                encode_core(e, &b.body);
            }
        }
        Core::Prim { op, args } => {
            e.u8(7);
            encode_primop(e, *op);
            encode_atoms(e, args);
        }
        Core::Perform { cap, op, args } => {
            e.u8(8);
            encode_atom(e, cap);
            e.u32(op.0);
            encode_atoms(e, args);
        }
        Core::Raise { error, args } => {
            e.u8(9);
            e.hash(error);
            encode_atoms(e, args);
        }
        Core::Loop {
            state,
            invariant,
            cond,
            body,
        } => {
            e.u8(10);
            encode_atoms(e, state);
            match invariant {
                None => e.u8(0),
                Some(p) => {
                    e.u8(1);
                    encode_pred(e, p);
                }
            }
            encode_core(e, cond);
            encode_core(e, body);
        }
        Core::Cast { value, to } => {
            e.u8(11);
            encode_atom(e, value);
            encode_type(e, to);
        }
        Core::Ref { mutable, of } => {
            e.u8(12);
            e.u8(*mutable as u8);
            encode_atom(e, of);
        }
        Core::Array { elem, items } => {
            e.u8(13);
            encode_type(e, elem);
            encode_atoms(e, items);
        }
        Core::IndexSet { base, index, value } => {
            e.u8(14);
            encode_atom(e, base);
            encode_atom(e, index);
            encode_atom(e, value);
        }
    }
}

fn encode_preds(e: &mut Encoder, preds: &[Pred]) {
    e.u64(preds.len() as u64);
    for p in preds {
        encode_pred(e, p);
    }
}

fn encode_pred(e: &mut Encoder, p: &Pred) {
    match p {
        Pred::True => e.u8(0),
        Pred::False => e.u8(1),
        Pred::Cmp(op, l, r) => {
            e.u8(2);
            encode_cmpop(e, *op);
            encode_cexpr(e, l);
            encode_cexpr(e, r);
        }
        Pred::And(l, r) => {
            e.u8(3);
            encode_pred(e, l);
            encode_pred(e, r);
        }
        Pred::Or(l, r) => {
            e.u8(4);
            encode_pred(e, l);
            encode_pred(e, r);
        }
        Pred::Not(inner) => {
            e.u8(5);
            encode_pred(e, inner);
        }
        Pred::Forall { domain, body } => {
            e.u8(6);
            encode_cexpr(e, &domain.0);
            encode_cexpr(e, &domain.1);
            encode_pred(e, body);
        }
        Pred::Exists { domain, body } => {
            e.u8(7);
            encode_cexpr(e, &domain.0);
            encode_cexpr(e, &domain.1);
            encode_pred(e, body);
        }
    }
}

/// Encode a contract expression ([`CExpr`], MARV-11). The encoding extends
/// [`encode_atom`]'s prefix code: an atom operand emits exactly the bytes it
/// always did (tags 0–2), so every pre-MARV-11 definition keeps its content
/// hash; compound nodes claim the next tag bytes (3+).
fn encode_cexpr(e: &mut Encoder, x: &CExpr) {
    match x {
        CExpr::Atom(a) => encode_atom(e, a),
        CExpr::Node(n) => match &**n {
            CNode::Bin(op, l, r) => {
                e.u8(3);
                encode_arithop(e, *op);
                encode_cexpr(e, l);
                encode_cexpr(e, r);
            }
            CNode::Neg(inner) => {
                e.u8(4);
                encode_cexpr(e, inner);
            }
            CNode::Len(inner) => {
                e.u8(5);
                encode_cexpr(e, inner);
            }
            CNode::Index(base, index) => {
                e.u8(6);
                encode_cexpr(e, base);
                encode_cexpr(e, index);
            }
            CNode::Proj(base, idx) => {
                e.u8(7);
                encode_cexpr(e, base);
                e.u32(*idx);
            }
        },
    }
}

fn encode_arithop(e: &mut Encoder, op: ArithOp) {
    e.u8(match op {
        ArithOp::Add => 0,
        ArithOp::Sub => 1,
        ArithOp::Mul => 2,
        ArithOp::Div => 3,
        ArithOp::Rem => 4,
    })
}
