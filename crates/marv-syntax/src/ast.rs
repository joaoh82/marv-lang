//! The marv surface AST (milestone M0).
//!
//! This models the *bounded but real* subset of the grammar in
//! `spec/02-grammar-and-core-ir.md` §B that M0 implements end to end
//! (lex → parse → format): module headers, imports, `struct` and `fn`
//! declarations, a small type language, block bodies, `let`/`var`/`return`
//! statements, and value expressions with binary operators and `if`/`else`.
//!
//! Two grammar ambiguities are designed out of the AST up front so that
//! text ⇄ AST is bijective (the M0 round-trip gate):
//!
//! 1. **No expression-statements.** A standalone expression in a block is only
//!    ever the block's *tail* ([`Tail::Expr`]); it is never a statement. So
//!    [`Stmt`] is exactly `let` or `var`.
//! 2. **`return` is terminal.** A block has at most one [`Tail`], and
//!    [`Tail::Return`] carries everything a `return` can; nothing may follow it.
//!    This removes the "valueless `return` vs `return <expr>`" ambiguity.
//!
//! The formatter ([`crate::format_module`]) is the inverse of the parser
//! ([`crate::parse`]) on every value of these types — see the round-trip
//! property test in `tests/roundtrip.rs`.

/// A dotted name, e.g. `std.io` → `["std", "io"]`. Always non-empty.
pub type Path = Vec<String>;

/// A whole compilation unit: `mod` header, imports, then items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: Path,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

/// `import path` or `import path (Name, Name, ...)`.
///
/// `names`, when `Some`, is always non-empty (the grammar requires at least one
/// name inside the parentheses); `None` means the bare `import path` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub path: Path,
    pub names: Option<Vec<String>>,
}

/// A top-level declaration. Covers `struct`, `enum`, `error`, and `fn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Struct(StructDecl),
    Enum(EnumDecl),
    Error(ErrorDecl),
    Fn(FnDecl),
}

/// `error Name { Variant, Variant, ... }` (`spec/02` §B `error_decl`,
/// `spec/01` §6). An error type is an enum-like sum whose variants are bare
/// (payload-free) names; a function's *error set* is inferred from the errors
/// its body can raise (`!T` return type) and surfaced via `marv/errorSet`.
/// Variants are kept in declaration order, which fixes their tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDecl {
    pub name: String,
    /// Variant names in declaration order; always non-empty (the grammar
    /// requires at least one).
    pub variants: Vec<String>,
}

/// `[linear] struct Name { field: Type, ... }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    pub linear: bool,
    pub name: String,
    pub fields: Vec<Field>,
}

/// One `name: Type` field of a struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

/// `enum Name[generics] { Variant, Variant(T, ...), ... }` (`spec/02` §B
/// `enum_decl`). Variants are kept in declaration order, which fixes their Core
/// tag (`spec/02` §C — `Match` branches are ordered by variant tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDecl {
    pub name: String,
    /// Generic type parameter names, e.g. `["T"]` for `enum Option[T]`. Empty
    /// when the enum is monomorphic.
    pub generics: Vec<String>,
    pub variants: Vec<Variant>,
}

/// One variant of an enum: a name and zero or more positional payload types. A
/// nullary variant (`None`) has an empty `fields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Type>,
}

/// `[pure] fn name(params) [-> ret] [requires e]* [ensures e]* { body }`.
///
/// Contract clauses (`spec/01` §7) sit between the signature and the body, each
/// on its own line. `requires` expressions may mention the parameters;
/// `ensures` expressions may additionally mention `result`. They are boolean
/// expressions in the ordinary expression language (lowered to `Pred`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnDecl {
    pub is_pure: bool,
    pub name: String,
    /// Generic type parameter names, e.g. `["T"]` for `fn is_some[T](...)`.
    /// Empty for a non-generic function.
    pub generics: Vec<String>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// Preconditions, in source order (`requires` clauses).
    pub requires: Vec<Expr>,
    /// Postconditions, in source order (`ensures` clauses; may mention `result`).
    pub ensures: Vec<Expr>,
    pub body: Block,
}

/// One `name: Type` function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

/// The M0 type language: named/generic-free paths, slices, second-class
/// references, and unit. (`spec/02` §B `type`, restricted.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// `()`
    Unit,
    /// A named type, possibly dotted: `i32`, `Sale`, `std.io.Io`.
    Named(Path),
    /// A generic application `Name[T, ...]`, e.g. `Option[T]`, `Result[T, E]`
    /// (`spec/02` §B `base_type` with type arguments). `args` is non-empty; the
    /// no-argument form is [`Type::Named`].
    Generic { path: Path, args: Vec<Type> },
    /// `[]T` — a slice of `T`.
    Slice(Box<Type>),
    /// `[N]T` — a fixed-length array of `N` elements of `T` (`spec/02` §B
    /// `base_type`, `spec/01` §3.2).
    Array { len: u64, elem: Box<Type> },
    /// `&T` / `&mut T` — a second-class reference.
    Ref { mutable: bool, inner: Box<Type> },
    /// `!T` (or bare `!`, i.e. `!()`) — an error union over success type `T`
    /// whose error *set* is inferred from the body (`spec/02` §B `base_type`,
    /// `spec/01` §6). `None` is the bare `!` form, a union over `()`.
    ErrorUnion(Option<Box<Type>>),
    /// `?T` — the optional sugar, desugaring to `Option[T]` (`spec/02` §B,
    /// §D).
    Optional(Box<Type>),
}

/// A brace-delimited block: zero or more statements, then an optional tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Tail>,
}

/// A block statement: a binding or an assignment. There are still no
/// expression-statements (see the module docs); a standalone expression is only
/// ever a block tail. An [`Stmt::Assign`], by contrast, is *not* an expression —
/// it has no value — so it is unambiguously a statement and never a tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Let {
        name: String,
        ty: Option<Type>,
        value: Expr,
    },
    Var {
        name: String,
        ty: Option<Type>,
        value: Expr,
    },
    /// `lvalue = expr` (`spec/02` §B `assign_stmt`). Reassigns a mutable `var`
    /// binding, a field of one (`p.x = e`), or an element (`a[i] = e`), under the
    /// mutable-value-semantics model (`spec/01` §4).
    Assign { target: LValue, value: Expr },
    /// `while cond { invariant e }* block` (`spec/02` §B `while_stmt`). A loop is
    /// a statement — it has no value — so it sits in [`Stmt`], never a [`Tail`].
    /// Each `invariant` clause is a boolean expression that must hold whenever the
    /// condition is tested (a Tier-1/Tier-2 proof obligation, `spec/01` §7); they
    /// are kept in source order and lowered to a `Pred` carried on `Core::Loop`.
    While {
        cond: Expr,
        invariants: Vec<Expr>,
        body: Block,
    },
    /// `for binder in iter block` (`spec/02` §B `for_stmt`). Desugars to an
    /// index-driven loop over `iter` (`spec/02` §D); the binder is immutable
    /// within the body.
    For {
        binder: String,
        iter: Expr,
        body: Block,
    },
}

/// An assignment target (`spec/02` §B `lvalue`): a root binding name, optionally
/// followed by field projections and index accesses. The root is always a bare
/// identifier; aliasing therefore stays local (`spec/01` §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LValue {
    /// `name`
    Var(String),
    /// `base.field`
    Field(Box<LValue>, String),
    /// `base[index]`
    Index(Box<LValue>, Box<Expr>),
}

/// The terminal element of a block: its value. Exactly one of these may appear,
/// and nothing may follow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tail {
    /// A value expression that is the block's result.
    Expr(Expr),
    /// `return` or `return <expr>` (terminal).
    Return(Option<Expr>),
    /// An `if`/`else` chain producing the block's result.
    If(Box<IfExpr>),
    /// A `match` expression producing the block's result. Like [`Tail::If`],
    /// `match` appears only at a block tail, which keeps formatting
    /// line-oriented and the grammar unambiguous.
    Match(Box<MatchExpr>),
}

/// `if cond { .. } [else (if .. | { .. })]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfExpr {
    pub cond: Expr,
    pub then: Block,
    pub els: Option<Else>,
}

/// The `else` arm: either a chained `else if` or a final `else { .. }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Else {
    If(Box<IfExpr>),
    Block(Block),
}

/// `match scrutinee { arm, ... }` (`spec/02` §B `match_expr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchExpr {
    pub scrutinee: Expr,
    pub arms: Vec<Arm>,
}

/// One `pattern => body,` arm of a `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    pub pat: Pattern,
    pub body: ArmBody,
}

/// The right-hand side of a `match` arm: either a single expression
/// (`pat => expr,`) or a block (`pat => { .. },`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmBody {
    Expr(Expr),
    Block(Block),
}

/// A match pattern. The supported subset (`spec/02` §B `pattern`) is the
/// wildcard `_` and constructor patterns `Path[(field, ...)]` whose fields are
/// themselves a binder or `_` — enough for exhaustive matches over enums (and
/// `bool`, whose variants are `false`/`true`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// `_` — matches anything, binds nothing.
    Wildcard,
    /// `Enum.Variant`, bare `Variant`, or `Variant(p, ...)`. `path` is the
    /// (possibly dotted) constructor name; `fields` are its sub-patterns
    /// (empty for a nullary variant).
    Ctor { path: Path, fields: Vec<FieldPat> },
}

/// A constructor pattern's field sub-pattern: a fresh binder or `_`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldPat {
    /// Binds the field to a name in the arm body.
    Bind(String),
    /// `_` — ignores the field.
    Wildcard,
}

/// A value expression. `if`/`else` is intentionally *not* here — it only occurs
/// at a block tail ([`Tail::If`]) in M0, which keeps formatting line-oriented and
/// the grammar unambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// `()`
    Unit,
    Int(i64),
    Bool(bool),
    Str(String),
    /// A character literal `'a'` — a single Unicode scalar (`spec/01` §3.1,
    /// `spec/02` §B `char_lit`).
    Char(char),
    /// A bare identifier. Dotted access is [`Expr::Field`], not a path.
    Var(String),
    /// `base.name`
    Field(Box<Expr>, String),
    /// `callee(arg, ...)`
    Call(Box<Expr>, Vec<Expr>),
    /// `base[index]` — index into a slice/array/aggregate (`spec/02` §B `postfix`).
    Index(Box<Expr>, Box<Expr>),
    /// `Name { field: expr, ... }` — a struct literal (product construction,
    /// `spec/02` §B `primary` struct-literal form). `path` names the struct;
    /// `fields` are the field initializers, written in any order (lowering
    /// reorders them into declaration order for the `Ctor`).
    Struct {
        path: Path,
        fields: Vec<FieldInit>,
    },
    /// `expr?` — postfix error propagation (`spec/02` §B `postfix`, §D). On a
    /// value of error-union/optional type it yields the success value and
    /// propagates the error/none case to the enclosing function.
    Try(Box<Expr>),
    /// `expr as Type` — an explicit scalar conversion (`spec/02` §B `postfix`,
    /// `spec/01` §3.1). There are no implicit numeric coercions; widening and
    /// narrowing both go through `as`, and narrowing is checked in debug builds.
    Cast(Box<Expr>, Type),
    /// `(lhs op rhs)` — always fully parenthesized in canonical form.
    Binary(Box<Expr>, BinOp, Box<Expr>),
}

/// One `name: expr` initializer of a struct literal (`spec/02` §B `field_init`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
}

/// The M0 binary operators (`spec/02` §B `binop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
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
    And,
    Or,
}

impl BinOp {
    /// The canonical spelling of the operator.
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
        }
    }

    /// Binding power for precedence climbing: higher binds tighter. The
    /// canonical formatter fully parenthesizes, so precedence only affects how
    /// *unparenthesized* (non-canonical) drafts are grouped on the way in.
    pub fn precedence(self) -> u8 {
        match self {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
            BinOp::Add | BinOp::Sub => 4,
            BinOp::Mul | BinOp::Div | BinOp::Rem => 5,
        }
    }
}
