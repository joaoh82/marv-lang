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

/// A top-level declaration. M0 covers `struct` and `fn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Struct(StructDecl),
    Fn(FnDecl),
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
    /// `[]T` — a slice of `T`.
    Slice(Box<Type>),
    /// `&T` / `&mut T` — a second-class reference.
    Ref { mutable: bool, inner: Box<Type> },
}

/// A brace-delimited block: zero or more statements, then an optional tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Tail>,
}

/// A block statement. Only bindings exist in M0 (see the module docs: there are
/// no expression-statements).
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
    /// A bare identifier. Dotted access is [`Expr::Field`], not a path.
    Var(String),
    /// `base.name`
    Field(Box<Expr>, String),
    /// `callee(arg, ...)`
    Call(Box<Expr>, Vec<Expr>),
    /// `(lhs op rhs)` — always fully parenthesized in canonical form.
    Binary(Box<Expr>, BinOp, Box<Expr>),
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
