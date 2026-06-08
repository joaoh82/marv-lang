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

/// A top-level declaration. Covers `struct`, `enum`, `error`, `fn`, `interface`,
/// and `impl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Struct(StructDecl),
    Enum(EnumDecl),
    Error(ErrorDecl),
    Fn(FnDecl),
    Interface(InterfaceDecl),
    Impl(ImplDecl),
}

impl Item {
    /// The declaration's source name. For an `impl` this is the interface name it
    /// implements (the `impl` block itself is anonymous; its methods carry their
    /// own names).
    pub fn name(&self) -> &str {
        match self {
            Item::Struct(d) => &d.name,
            Item::Enum(d) => &d.name,
            Item::Error(d) => &d.name,
            Item::Fn(d) => &d.name,
            Item::Interface(d) => &d.name,
            Item::Impl(d) => d.interface.last().map(String::as_str).unwrap_or(""),
        }
    }

    /// The doc-comment lines attached to this item (see [`ErrorDecl::docs`]).
    pub fn docs(&self) -> &[String] {
        match self {
            Item::Struct(d) => &d.docs,
            Item::Enum(d) => &d.docs,
            Item::Error(d) => &d.docs,
            Item::Fn(d) => &d.docs,
            Item::Interface(d) => &d.docs,
            Item::Impl(d) => &d.docs,
        }
    }
}

/// One generic type parameter, optionally carrying an interface bound
/// (`spec/02` §B `generic = ident , [ ":" , bound ]`). `T` is unbounded; `T: Ord`
/// constrains `T` to types that implement the [`Bound`] interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generic {
    pub name: String,
    pub bound: Option<Bound>,
}

/// An interface bound on a generic parameter (`spec/02` §B `bound = path , [ "["
/// , type , { "," , type } , "]" ]`). `path` names the interface (e.g. `Ord`);
/// `args` are any *extra* type arguments beyond the constrained parameter itself
/// (empty for a single-parameter interface like `Ord[T]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub path: Path,
    pub args: Vec<Type>,
}

/// The parameter names of a generic list, dropping any bounds — what lowering's
/// de Bruijn `Type::Var` resolution keys on.
pub fn generic_names(generics: &[Generic]) -> Vec<String> {
    generics.iter().map(|g| g.name.clone()).collect()
}

/// `interface Name[generics] { fn sig; ... }` (`spec/02` §B `interface_decl`,
/// `spec/01` §3.4). An interface is bounded polymorphism: it declares abstract
/// method signatures over its type parameter(s); concrete types supply bodies via
/// an [`ImplDecl`]. The grammar requires a non-empty generic list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceDecl {
    /// Doc-comment lines preceding the declaration (see [`ErrorDecl::docs`]).
    pub docs: Vec<String>,
    pub name: String,
    /// Generic type parameter names, e.g. `["T"]` for `interface Ord[T]`. Always
    /// non-empty (the grammar requires generics on an interface).
    pub generics: Vec<Generic>,
    /// The method signatures the interface declares (bodies live in `impl`s).
    pub methods: Vec<FnSig>,
}

/// An abstract method signature inside an `interface` (`spec/02` §B `fn_sig`):
/// like an [`FnDecl`] but with no body and no contract clauses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnSig {
    /// Doc-comment lines preceding the signature (see [`ErrorDecl::docs`]).
    pub docs: Vec<String>,
    pub name: String,
    /// Per-method generic parameters (rare; usually empty — the interface's own
    /// type parameter does the work).
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
}

/// `impl Interface[Type, ...] { fn ... }` (`spec/02` §B `impl_decl`,
/// `spec/01` §3.4). A coherent (one per interface-per-type), explicit
/// implementation of an interface for a concrete type. Its methods are ordinary
/// functions whose signatures match the interface's, with the type parameter
/// replaced by the impl's concrete type argument(s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplDecl {
    /// Doc-comment lines preceding the declaration (see [`ErrorDecl::docs`]).
    pub docs: Vec<String>,
    /// The interface being implemented, e.g. `["Ord"]`.
    pub interface: Path,
    /// The concrete type argument(s) the interface is implemented for, e.g.
    /// `[i32]` in `impl Ord[i32]`. Always non-empty (the grammar requires
    /// brackets).
    pub args: Vec<Type>,
    /// The method bodies, as full function declarations.
    pub methods: Vec<FnDecl>,
}

/// Real source spans for one top-level item, in UTF-8 byte offsets (MARV-12).
///
/// These are produced by [`crate::parse_with_spans`] and live *outside* the AST
/// proper, so adding them never disturbs the `parse ∘ format == id` round-trip
/// (which compares ASTs) nor the Core content hash (which never sees source
/// text). They let the checker's diagnostics, `marv/typeAt`, `marv/verify`, and
/// `marv/applyFix` report and resolve real offsets (`spec/03` §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSpan {
    /// The item's source name, e.g. `"clamp"`. Matches the lowered `DefEntry`.
    pub name: String,
    /// Byte range of the declaration header — from the leading keyword/modifier
    /// (`pure fn`, `linear struct`, `enum`, `error`) through the name. The anchor
    /// a diagnostic or `typeAt` points at.
    pub header: (u32, u32),
    /// Byte offset just *inside* the parameter list's opening `(`, i.e. where a
    /// new leading parameter is inserted (the `MissingCapability` fix's resolved
    /// insertion point). `None` for non-`fn` items.
    pub param_insert: Option<u32>,
}

/// `error Name { Variant, Variant, ... }` (`spec/02` §B `error_decl`,
/// `spec/01` §6). An error type is an enum-like sum whose variants are bare
/// (payload-free) names; a function's *error set* is inferred from the errors
/// its body can raise (`!T` return type) and surfaced via `marv/errorSet`.
/// Variants are kept in declaration order, which fixes their tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDecl {
    /// Doc-comment lines (`///`) immediately preceding the declaration, in order,
    /// each without its `///` prefix. Preserved by the formatter but excluded from
    /// the Core content hash (`spec/02` §F — not part of identity).
    pub docs: Vec<String>,
    pub name: String,
    /// Variant names in declaration order; always non-empty (the grammar
    /// requires at least one).
    pub variants: Vec<String>,
}

/// `[linear] struct Name { field: Type, ... }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    /// Doc-comment lines preceding the declaration (see [`ErrorDecl::docs`]).
    pub docs: Vec<String>,
    pub linear: bool,
    pub name: String,
    /// Generic type parameters, e.g. `[T]` for `struct Pair[T]`. Empty when the
    /// struct is monomorphic (`spec/02` §B `struct_decl`).
    pub generics: Vec<Generic>,
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
    /// Doc-comment lines preceding the declaration (see [`ErrorDecl::docs`]).
    pub docs: Vec<String>,
    pub name: String,
    /// Generic type parameters, e.g. `[T]` for `enum Option[T]`. Empty when the
    /// enum is monomorphic.
    pub generics: Vec<Generic>,
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
    /// Doc-comment lines preceding the declaration (see [`ErrorDecl::docs`]).
    pub docs: Vec<String>,
    pub is_pure: bool,
    pub name: String,
    /// Generic type parameters, e.g. `[T]` (or `[T: Ord]`) for `fn sort[T](...)`.
    /// Empty for a non-generic function.
    pub generics: Vec<Generic>,
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
    /// `[e0, e1, ...]` — an array literal (`spec/02` §B `primary`). A homogeneous,
    /// fixed-length product whose type is `[N]T`; lowers to a `Core::Array`. The
    /// empty form `[]` parses but needs a type annotation to fix its element type.
    Array(Vec<Expr>),
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
    /// A prefix unary operator applied to an operand (`spec/02` §B `unary`):
    /// `-e`, `not e`, `&e`, `&mut e`. Unary binds tighter than every binary
    /// operator and is right-associative, so a stacked form like `not not p`
    /// or `- -x` nests outermost-first.
    Unary(UnOp, Box<Expr>),
}

/// The prefix unary operators (`spec/02` §B `unary`). `&`/`&mut` here are the
/// *expression* reference-of operators (distinct from the `&T`/`&mut T` type
/// prefixes parsed by `parse_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `-e` — arithmetic negation.
    Neg,
    /// `not e` — logical negation.
    Not,
    /// `&e` — take a (shared, second-class) reference to `e`.
    Ref,
    /// `&mut e` — take a mutable second-class reference to `e`.
    RefMut,
}

impl UnOp {
    /// The canonical spelling of the operator (without any trailing separator;
    /// see [`crate::format`] for how `not`/`&mut` get their space).
    pub fn as_str(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "not",
            UnOp::Ref => "&",
            UnOp::RefMut => "&mut",
        }
    }
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
