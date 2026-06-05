//! The canonical **Core IR** data model (`spec/02-grammar-and-core-ir.md` §C).
//!
//! Everything in marv lowers to this small typed IR in **A-normal form (ANF)**
//! with **de Bruijn indices**. ANF means every operand is *atomic* (a variable,
//! literal, or global reference); de Bruijn means variable *names* never appear,
//! so alpha-equivalent surface programs are *identical* Core terms. The unit of
//! identity is the `blake3`-256 content hash of a [`Def`] (see [`crate::hash`]
//! and `spec/02` §F).
//!
//! The enums here mirror the Rust data model presented in `spec/02` §C exactly.
//! A few auxiliary types the spec names but does not spell out (`Literal`,
//! `IntTy`, `FloatTy`, `PrimOp`, `CmpOp`, `OpId`, `DefKind`) are defined here
//! with the minimal-but-forward-looking shape M1 needs. Their *encoding* — and
//! therefore the hash — is pinned explicitly in [`crate::hash`] via stable tag
//! bytes, so reordering a variant in this file never changes a content hash.
//!
//! ## Serde / the protocol wire form (`spec/03` §4.4)
//!
//! Every Core node derives `serde::{Serialize, Deserialize}` with serde's default
//! *externally-tagged* representation, which is exactly the JSON the agent
//! protocol's `marv/core` query emits: a struct variant `Lam` becomes
//! `{"Lam": { … }}`, a newtype variant `Var(0)` becomes `{"Var": 0}`, and a unit
//! variant `I32` becomes `"I32"` (see `spec/03` §4.4). [`Hash`] is the one
//! exception — it (de)serializes as the spec's `"b3:<hex>"` string rather than a
//! byte array, so content identities are human-readable on the wire.

use serde::{Deserialize, Serialize};

/// A content hash of a Core definition: `blake3`-256 over its canonical encoding
/// (`spec/02` §F). Also used for content-addressed references to other
/// definitions ([`Atom::Global`], [`Type::Nominal`]).
///
/// `Ord` is derived so set-like collections (effect rows, error sets) can be
/// sorted into a single canonical order before hashing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Lowercase hex rendering of all 32 bytes.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// The protocol wire spelling: the `b3:` algorithm tag followed by the full
    /// lowercase hex (`spec/03` §4.4, e.g. `"b3:9f2c1a…"`).
    pub fn to_b3(&self) -> String {
        format!("b3:{}", self.to_hex())
    }

    /// Parse a `b3:<hex>` (or bare `<hex>`) string back into a [`Hash`]. Returns
    /// `None` on a wrong-length or non-hex body.
    pub fn from_b3(s: &str) -> Option<Hash> {
        let hex = s.strip_prefix("b3:").unwrap_or(s);
        if hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Hash(bytes))
    }
}

impl serde::Serialize for Hash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_b3())
    }
}

impl<'de> serde::Deserialize<'de> for Hash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
        let s = String::deserialize(d)?;
        Hash::from_b3(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid b3 content hash: {s:?}")))
    }
}

impl std::fmt::Debug for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Short prefix keeps Core debug dumps readable; full value via `to_hex`.
        write!(f, "Hash({}…)", &self.to_hex()[..16])
    }
}

impl std::fmt::Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Width of an integer type. Names match the surface spellings (`i32`, `usize`…).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
}

/// Width of a floating-point type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FloatTy {
    F32,
    F64,
}

/// A canonical Core type (`spec/02` §C `Type`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Type {
    Unit,
    Bool,
    Int(IntTy),
    Float(FloatTy),
    Str,
    Char,
    Array(Box<Type>, u64),
    Slice(Box<Type>),
    Tuple(Vec<Type>),
    /// Arrow carries an effect row: the set of capabilities it may use.
    Arrow {
        param: Box<Type>,
        ret: Box<Type>,
        effects: EffectRow,
    },
    /// Nominal type referenced by content hash (struct/enum/interface decl).
    Nominal {
        def: Hash,
        args: Vec<Type>,
    },
    /// Second-class reference; never stored (see `spec/02` §E).
    Ref {
        mutable: bool,
        of: Box<Type>,
    },
    /// Resource type, used exactly once.
    Linear(Box<Type>),
    /// Generic type variable (de Bruijn).
    Var(u32),
}

/// The set of capabilities a computation may exercise and the errors it may
/// raise — the effect/error row carried by an [`Type::Arrow`] (`spec/02` §C).
///
/// Both fields are *set-like*: their declaration order is incidental, so the
/// encoder sorts them into a single canonical order before hashing (`spec/02`
/// §F rule 3). `pure` is exactly the empty row.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EffectRow {
    /// Capabilities this computation may exercise, as content hashes of cap decls.
    pub caps: Vec<Hash>,
    /// Errors this computation may raise (the inferred error set).
    pub errors: Vec<Hash>,
}

impl EffectRow {
    /// The empty row — the meaning of `pure` (`spec/02` §C).
    pub fn empty() -> Self {
        EffectRow::default()
    }

    /// Whether this is the empty (`pure`) row.
    pub fn is_empty(&self) -> bool {
        self.caps.is_empty() && self.errors.is_empty()
    }
}

/// A literal value. The M0 surface produces `Unit`/`Bool`/`Int`/`Str`; the rest
/// are defined for forward compatibility and pinned in the encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Literal {
    Unit,
    Bool(bool),
    Int(i64),
    /// Raw IEEE-754 bits, so the encoding is exact and deterministic.
    Float(u64),
    Str(String),
    Char(char),
}

/// An atomic operand (`spec/02` §C `Atom`). ANF guarantees every operand is one
/// of these three.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Atom {
    /// de Bruijn index into the local environment.
    Var(u32),
    /// Reference to another content-addressed definition.
    Global(Hash),
    Lit(Literal),
}

/// A total primitive operation (`spec/02` §C `Core::Prim`). The M0 binary
/// operators map here; `Not`/`Len`/`Index` round out the total set the later
/// milestones need. Scalar conversion is *not* a `PrimOp` — it carries a target
/// type a flat op cannot, so it is its own [`Core::Cast`] node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrimOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Strict logical conjunction (operands are already ANF-evaluated, so this
    /// is total — there is no hidden short-circuit control flow; `spec/01` §2).
    And,
    /// Strict logical disjunction (see [`PrimOp::And`]).
    Or,
    Not,
    /// Arithmetic negation `-x` (the prefix `-` operator, `spec/02` §B `unary`).
    /// Unary, like [`PrimOp::Not`]; the operand is numeric and the result has the
    /// operand's type.
    Neg,
    Len,
    Index,
}

/// Comparison operator used inside contract predicates ([`Pred`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Identifies a capability method at a [`Core::Perform`] site (`spec/02` §C).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpId(pub u32);

/// A Core term (`spec/02` §C `Core`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Core {
    Atom(Atom),
    /// let-binding; binds exactly one variable (index 0) in `body`. ANF spine.
    Let {
        value: Box<Core>,
        body: Box<Core>,
    },
    /// lambda; binds one parameter; records its declared effect row.
    Lam {
        param: Type,
        effects: EffectRow,
        body: Box<Core>,
    },
    /// application; both positions atomic (ANF).
    App {
        func: Atom,
        arg: Atom,
    },
    /// construct a product or a sum variant (tag selects the variant; products
    /// use tag 0).
    Ctor {
        ty: Hash,
        tag: u32,
        fields: Vec<Atom>,
    },
    /// project field `idx` from an aggregate atom.
    Proj {
        base: Atom,
        idx: u32,
    },
    /// exhaustive case over a sum; branches are ordered by variant tag.
    Match {
        scrutinee: Atom,
        branches: Vec<Branch>,
    },
    /// total primitive operation (arithmetic, comparison, len, index, …).
    Prim {
        op: PrimOp,
        args: Vec<Atom>,
    },
    /// explicit scalar conversion `value as to` (`spec/01` §3.1). Unlike a
    /// [`Core::Prim`], a cast carries its *target type* (the destination width /
    /// representation), which a flat `PrimOp` cannot. There are no implicit
    /// numeric coercions; widening and narrowing both lower to this node, and a
    /// narrowing cast is range-checked in debug builds (Tier 1).
    Cast {
        value: Atom,
        to: Type,
    },
    /// take a second-class reference to `of` (`&e` / `&mut e`, `spec/02` §B
    /// `unary`; `spec/01` §4). Its type is [`Type::Ref`]; the second-class rules
    /// (a reference is never stored in a field, returned, or captured) are
    /// enforced by the checker over that type. There are no mutable cells in Core
    /// (mutable value semantics, `spec/01` §4), so at runtime a reference *is* its
    /// referent's value — the backends evaluate `of` and pass it through.
    Ref {
        mutable: bool,
        of: Atom,
    },
    /// perform a capability operation: `cap` identifies the capability, `op` the
    /// method.
    Perform {
        cap: Atom,
        op: OpId,
        args: Vec<Atom>,
    },
    /// raise into the error union.
    Raise {
        error: Hash,
        args: Vec<Atom>,
    },
    /// Loop with explicit loop-carried state and a recorded invariant (a proof
    /// obligation); desugars from `while`/`for` (`spec/02` §D).
    ///
    /// `state` holds the initial values of the loop-carried variables (the `var`s
    /// the body reassigns), evaluated in the enclosing scope. Within `invariant`,
    /// `cond`, and `body` those variables are bound as the innermost `state.len()`
    /// de Bruijn slots; `body` evaluates to their *next* values (a tuple `Ctor`
    /// over them), and the `Loop` itself evaluates to their *final* values (the
    /// same tuple), so the enclosing scope can rebind each one by projection. This
    /// is the functional/SSA encoding of mutable value semantics across iterations
    /// (`spec/01` §4) — there are no mutable cells in Core.
    Loop {
        state: Vec<Atom>,
        invariant: Option<Box<Pred>>,
        cond: Box<Core>,
        body: Box<Core>,
    },
}

/// One arm of a [`Core::Match`]. `binds` is the constructor arity introduced
/// into scope for the branch body (0 for nullary variants such as `bool`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub binds: u32,
    pub body: Core,
}

/// First-order predicate language used by contracts (Tier-2 proof obligations,
/// `spec/02` §C `Pred`). Defined in full for forward compatibility; M1 does not
/// yet parse `requires`/`ensures`, so lowered defs carry empty contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pred {
    True,
    False,
    Cmp(CmpOp, Atom, Atom),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
    /// bounded range `[lo, hi)`.
    Forall {
        domain: (Atom, Atom),
        body: Box<Pred>,
    },
    Exists {
        domain: (Atom, Atom),
        body: Box<Pred>,
    },
}

/// The kind of a top-level definition (`spec/02` §C `Def.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefKind {
    Fn,
    Struct,
    Enum,
    Interface,
    Impl,
    Const,
    Cap,
    Error,
}

/// A top-level, content-addressed definition (`spec/02` §C `Def`). Its [`Hash`]
/// is computed over this struct with all `Hash` children already resolved —
/// forming a Merkle DAG of code (see [`crate::hash`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Def {
    pub kind: DefKind,
    pub ty: Type,
    /// preconditions
    pub requires: Vec<Pred>,
    /// postconditions (may mention `result`, `old(_)`)
    pub ensures: Vec<Pred>,
    /// `None` for abstract interface signatures.
    pub body: Option<Core>,
}
