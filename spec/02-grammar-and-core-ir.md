# marv — Grammar and Canonical Core IR

Status: draft 0.1 · Companion to `01-design-spec.md`

This document defines (A) the lexical grammar, (B) the EBNF surface grammar, (C) the
canonical **Core IR** that everything lowers to, (D) desugaring from surface to Core,
(E) typing/effect judgments, and (F) the content-hashing scheme. The Core IR is the trusted,
verifiable, and content-addressed representation; the surface is a thin sugar over it.

---

## A. Lexical grammar

```
file        ::= UTF-8 text, newline = "\n"
token       ::= keyword | ident | literal | punct
ident       ::= XID_Start XID_Continue*            // Unicode identifiers
keyword     ::= "mod" | "import" | "fn" | "pure" | "struct" | "enum" | "linear"
              | "interface" | "impl" | "type" | "const" | "let" | "var"
              | "if" | "else" | "match" | "while" | "for" | "in" | "return"
              | "error" | "as" | "unsafe" | "true" | "false"
              | "requires" | "ensures" | "invariant" | "assert"
              | "forall" | "exists" | "and" | "or" | "not"
int_lit     ::= dec_int | hex_int | bin_int | oct_int       // underscores allowed: 1_000
float_lit   ::= dec_int "." dec_int (("e"|"E") sign? dec_int)?
char_lit    ::= "'" (char | escape) "'"
str_lit     ::= '"' (char | escape)* '"'
bool_lit    ::= "true" | "false"
comment     ::= "//" .* eol            // line only
doc_comment ::= "///" .* eol           // attaches to the next declaration
punct       ::= "(" | ")" | "{" | "}" | "[" | "]" | "," | ";" | ":" | "::"
              | "." | "->" | "-{" | "}->" | "&" | "&mut" | "?" | "!"
              | "=" | "==" | "!=" | "<" | "<=" | ">" | ">=" | "+" | "-"
              | "*" | "/" | "%" | "|"
```

Notes: `;` is a statement separator the formatter inserts/normalizes; in canonical form
statements are newline-separated and `;` appears only where two statements share a line
(which the formatter avoids). There are no semicolons in canonical output — they exist only
so a tolerant parser can accept agent drafts and *normalize* them.

---

## B. Surface grammar (EBNF)

```ebnf
program        = mod_decl , { import_decl } , { item } ;

mod_decl       = "mod" , path ;
import_decl    = "import" , path , [ "(" , ident , { "," , ident } , ")" ] ;
path           = ident , { "." , ident } ;

item           = fn_decl | struct_decl | enum_decl | interface_decl
               | impl_decl | type_decl | const_decl | error_decl ;

(* ---- functions ---- *)
fn_decl        = [ "pure" ] , [ "unsafe" ] , "fn" , ident , [ generics ] ,
                 "(" , [ params ] , ")" , [ "->" , type ] ,
                 { contract } , block ;
params         = param , { "," , param } ;
param          = ident , ":" , type ;
generics       = "[" , generic , { "," , generic } , "]" ;
generic        = ident , [ ":" , bound ] ;
bound          = path , [ "[" , type , { "," , type } , "]" ] ;
contract       = "requires" , expr
               | "ensures"  , expr
               | "invariant" , expr ;        (* invariant only on loops, see while *)

(* ---- types ---- *)
type           = ref_type | base_type ;
ref_type       = "&" , [ "mut" ] , base_type ;
base_type      = "?" , base_type                                  (* optional sugar *)
               | "!" , [ base_type ]                              (* error union; payload optional *)
               | "[" , int_lit , "]" , type                       (* fixed array *)
               | "[" , "]" , type                                 (* slice *)
               | "(" , [ type , { "," , type } ] , ")"            (* tuple / unit *)
               | "fn" , "(" , [ type , { "," , type } ] , ")" ,
                       [ effect_row ] , "->" , type                (* function type *)
               | path , [ "[" , type , { "," , type } , "]" ] ;    (* named / generic *)
effect_row     = "-{" , [ path , { "," , path } ] , "}->" ;        (* capabilities used *)

(* ---- type/data items ---- *)
struct_decl    = [ "linear" ] , "struct" , ident , [ generics ] ,
                 "{" , [ field , { "," , field } , [ "," ] ] , "}" ;
field          = ident , ":" , type ;
enum_decl      = "enum" , ident , [ generics ] ,
                 "{" , variant , { "," , variant } , [ "," ] , "}" ;
variant        = ident , [ "(" , type , { "," , type } , ")" ] ;
error_decl     = "error" , ident , "{" , ident , { "," , ident } , [ "," ] , "}" ;
type_decl      = "type" , ident , [ generics ] , "=" , type ;
const_decl     = "const" , ident , ":" , type , "=" , expr ;
interface_decl = "interface" , ident , generics , "{" , { fn_sig } , "}" ;
fn_sig         = "fn" , ident , [ generics ] , "(" , [ params ] , ")" ,
                 [ "->" , type ] ;
impl_decl      = "impl" , path , "[" , type , { "," , type } , "]" ,
                 "{" , { fn_decl } , "}" ;

(* ---- statements & expressions (blocks are expressions) ---- *)
block          = "{" , { stmt } , [ expr ] , "}" ;
stmt           = let_stmt | var_stmt | assign_stmt | expr_stmt | while_stmt
               | for_stmt | assert_stmt | return_stmt ;
let_stmt       = "let" , ident , [ ":" , type ] , "=" , expr ;
var_stmt       = "var" , ident , [ ":" , type ] , "=" , expr ;
assign_stmt    = lvalue , "=" , expr ;
lvalue         = ident , { "." , ident | "[" , expr , "]" } ;
expr_stmt      = expr ;
return_stmt    = "return" , [ expr ] ;
assert_stmt    = "assert" , expr ;
while_stmt     = "while" , expr , { "invariant" , expr } , block ;
for_stmt       = "for" , ident , "in" , expr , block ;

expr           = if_expr | match_expr | bin_expr ;
if_expr        = "if" , expr , block , [ "else" , ( if_expr | block ) ] ;
match_expr     = "match" , expr , "{" , { arm } , "}" ;
arm            = pattern , "=>" , ( expr | block ) , "," ;
pattern        = "_" | literal | ident
               | path , [ "(" , pattern , { "," , pattern } , ")" ] ;  (* ctor pattern *)
bin_expr       = unary , { binop , unary } ;     (* precedence resolved by formatter *)
binop          = "+"|"-"|"*"|"/"|"%"|"=="|"!="|"<"|"<="|">"|">="
               | "and"|"or" ;
unary          = [ "not" | "-" | "&" | "&mut" ] , postfix ;
postfix        = primary , { "." , ident                      (* field / method *)
                           | "(" , [ args ] , ")"             (* call *)
                           | "[" , expr , "]"                 (* index *)
                           | "?"                              (* error propagate *)
                           | "as" , type } ;                  (* cast *)
primary        = literal | path | "(" , expr , ")" | block
               | path , "{" , [ field_init , { "," , field_init } ] , "}" ; (* struct literal *)
field_init     = ident , ":" , expr ;
args           = expr , { "," , expr } ;
literal        = int_lit | float_lit | str_lit | char_lit | bool_lit | "(" , ")" ;
quant_expr     = ( "forall" | "exists" ) , ident , "in" , expr , ":" , expr ; (* contracts only *)
```

The grammar is intentionally **LL(k)-friendly and unambiguous** so a hand-written
recursive-descent parser can give precise error recovery (which the agent loop in `03`
depends on). Operator precedence is fixed; the canonical formatter inserts the unique
parenthesization, so agents never need to reason about precedence.

---

## C. Canonical Core IR

Everything lowers to a small typed IR in **A-normal form (ANF)** with **de Bruijn indices**.
ANF means every operand is *atomic* (a variable, literal, or global reference), which makes
evaluation order explicit, optimization simple, and hashing canonical. De Bruijn indices
mean variable *names* never appear, so alpha-equivalent programs are *identical* Core terms.

Presented as the Rust data model the implementation should use (this is also the
serialization shape hashed in §F):

```rust
/// A content hash of a Core definition (blake3-256 over its canonical encoding).
pub struct Hash([u8; 32]);

pub enum Type {
    Unit, Bool, Int(IntTy), Float(FloatTy), Str, Char,
    Array(Box<Type>, u64),
    Slice(Box<Type>),
    Tuple(Vec<Type>),
    /// Arrow carries an effect row: the set of capabilities it may use.
    Arrow { param: Box<Type>, ret: Box<Type>, effects: EffectRow },
    /// Nominal type referenced by content hash (struct/enum/interface decl).
    Nominal { def: Hash, args: Vec<Type> },
    Ref { mutable: bool, of: Box<Type> },     // second-class; never stored, see §E
    Linear(Box<Type>),                        // resource type, used exactly once
    Var(u32),                                 // generic type variable (de Bruijn)
}

pub struct EffectRow {
    /// Capabilities this computation may exercise, as content hashes of cap decls.
    pub caps: Vec<Hash>,
    /// Errors this computation may raise (the inferred error set).
    pub errors: Vec<Hash>,
}

pub enum Atom {
    Var(u32),            // de Bruijn index into the local environment
    Global(Hash),        // reference to another content-addressed definition
    Lit(Literal),
}

pub enum Core {
    Atom(Atom),
    /// let-binding; binds exactly one variable (index 0 shifts) in `body`. ANF spine.
    Let { value: Box<Core>, body: Box<Core> },
    /// lambda; binds one parameter; records its declared effect row.
    Lam { param: Type, effects: EffectRow, body: Box<Core> },
    /// application; both positions atomic (ANF).
    App { func: Atom, arg: Atom },
    /// construct a product or a sum variant (tag selects the variant; products use tag 0).
    Ctor { ty: Hash, tag: u32, fields: Vec<Atom> },
    /// project field `idx` from an aggregate atom.
    Proj { base: Atom, idx: u32 },
    /// exhaustive case over a sum; branches are ordered by variant tag.
    Match { scrutinee: Atom, branches: Vec<Branch> },
    /// total primitive operation (arithmetic, comparison, cast, len, index, ...).
    Prim { op: PrimOp, args: Vec<Atom> },
    /// perform a capability operation: `cap` identifies the capability, `op` the method.
    Perform { cap: Atom, op: OpId, args: Vec<Atom> },
    /// raise into the error union.
    Raise { error: Hash, args: Vec<Atom> },
    /// loop with explicit loop-carried state and a recorded invariant (proof obligation);
    /// desugars from while/for. `state` = initial values of the carried variables (evaluated
    /// in the enclosing scope); within `invariant`/`cond`/`body` they are the innermost
    /// `state.len()` de Bruijn slots; `body` evaluates to their next values (a tuple) and the
    /// `Loop` to their final values (the same tuple). Mutable value semantics has no cells, so
    /// cross-iteration mutation is this functional state-threading (§D, `spec/01` §4).
    Loop { state: Vec<Atom>, invariant: Option<Box<Pred>>, cond: Box<Core>, body: Box<Core> },
}

pub struct Branch { pub binds: u32, pub body: Core }   // `binds` = ctor arity introduced

/// First-order predicate language used by contracts (Tier 2 proof obligations).
pub enum Pred {
    True, False,
    Cmp(CmpOp, Atom, Atom),
    And(Box<Pred>, Box<Pred>), Or(Box<Pred>, Box<Pred>), Not(Box<Pred>),
    Forall { domain: (Atom, Atom), body: Box<Pred> },   // bounded range [lo, hi)
    Exists { domain: (Atom, Atom), body: Box<Pred> },
}

/// A top-level, content-addressed definition. Its `Hash` is computed over this struct
/// with all `Hash` children already resolved — forming a Merkle DAG of code.
pub struct Def {
    pub kind: DefKind,          // Fn | Struct | Enum | Interface | Impl | Const | Cap | Error
    pub ty: Type,
    pub requires: Vec<Pred>,    // preconditions
    pub ensures: Vec<Pred>,     // postconditions (may mention `result`, `old(_)`)
    pub body: Option<Core>,     // None for abstract interface signatures
}
```

`pure` in the surface is exactly `EffectRow { caps: [], errors: [] }` on the arrow.

---

## D. Desugaring (surface → Core)

Deterministic and total. Key rules:

- `?T` → `Nominal { def: hash(Option), args: [T] }`.
- `!T` with inferred error set `E` → `Nominal { def: hash(Result), args: [T, error-union(E)] }`.
- `if c { a } else { b }` → `Match` on a `bool` scrutinee with two branches.
- `e?` → `Match` on the `Result`/`Option`, returning early on the error/none branch.
- `a.method(x)` where `method` resolves to a free function → `App(App(method, a), x)`,
  curried; multi-arg functions are curried in Core.
- `while c { invariant I }* { body }` → `Loop { state, invariant, cond: c, body' }`, where
  `state` is the initial values of the loop-carried `var`s (those the body reassigns),
  `invariant` conjoins the clauses, and `body'` evaluates to the carried vars' next values (a
  tuple); the enclosing scope rebinds each carried var from a projection of the loop's result.
- `for x in xs { body }` → an index-driven `Loop`: `var i = 0; while i < len(xs) { let x = xs[i];
  body; i = i + 1 }` (the iterator protocol generalizes this later).
- Every non-atomic subexpression is hoisted into a `Let` (ANF normalization) in
  left-to-right evaluation order, making evaluation order explicit and total.
- Names are replaced by de Bruijn indices; doc comments and formatting are dropped (they are
  *not* part of identity).

Because desugaring is deterministic and the formatter is canonical, `format ∘ parse` and
`lower ∘ parse` are both functions — the M0/M1 acceptance gates.

---

## E. Typing and effect judgments (sketch)

Judgment: `Γ ; Δ ⊢ e : τ ! ε` — under value context `Γ` and linear context `Δ`, expression
`e` has type `τ` with effect row `ε`.

```
(Var)        Γ,x:τ ; Δ ⊢ x : τ ! ∅

(LinVar)     Γ ; Δ,x:τ ⊢ x : τ ! ∅            and x is removed from Δ (used once)

(Global)     def(h) : τ ; ─────────────────   Γ ; Δ ⊢ Global(h) : τ ! ∅

(App)        Γ;Δ ⊢ f : (τ1 -{εf}-> τ2) ! ∅     Γ;Δ ⊢ a : τ1 ! ∅
             ───────────────────────────────────────────────────
             Γ;Δ ⊢ App(f,a) : τ2 ! εf

(Let)        Γ;Δ ⊢ v : τ1 ! ε1     Γ,x:τ1;Δ ⊢ b : τ2 ! ε2
             ─────────────────────────────────────────────
             Γ;Δ ⊢ let x=v in b : τ2 ! (ε1 ∪ ε2)

(Perform)    cap : Cap_c        op(c) : (τa -> τr)
             ─────────────────────────────────────────────
             Γ;Δ ⊢ Perform(cap,op,a) : τr ! {c}

(Raise)      ──────────────────────────────────────────────
             Γ;Δ ⊢ Raise(E, a) : τ ! {error E}

(Match)      Γ;Δ ⊢ s : Nominal(enum) ! ∅   ∀ branch_i covering variant_i :
                 Γ, binds_i ; Δ ⊢ body_i : τ ! ε_i      (exhaustive)
             ─────────────────────────────────────────────────────────────
             Γ;Δ ⊢ Match(s, branches) : τ ! (⋃ ε_i)

(Pure)       Γ;Δ ⊢ body : τ ! ε      ε = ∅
             ─────────────────────────────────       (well-formedness of a `pure fn`)
             pure fn : τ  is well-typed
```

Additional static checks performed by `marv-types`:

- **Second-class references.** A value of type `Ref{..}` may appear as a function argument or
  be consumed within the same call frame, but the checker rejects any `Core` where a `Ref`
  flows into a `Ctor` field, a returned position, or a captured `Lam` environment. This is a
  purely local check — no lifetime inference, no annotations.
- **Linearity.** Each `Linear(_)` binding must be consumed exactly once along every control
  path (tracked through `Match` branches). Unused linear value ⇒ error; duplicated ⇒ error.
- **Capability provenance.** A `Perform` requires a capability *atom in scope*; capabilities
  are never produced by `Ctor`/`Prim`, only received as parameters or narrowed via designated
  attenuation ops, which is what makes "no ambient authority" enforceable.
- **Effect & error subsumption.** A declared signature row must be a superset of the inferred
  body row; otherwise the checker reports the exact missing capabilities/errors as a fix.

---

## F. Content hashing (identity)

The `Hash` of a `Def` is `blake3-256` over a canonical binary encoding with these rules:

1. **De Bruijn, no names.** Bound variables are indices; identifiers never enter the hash, so
   alpha-equivalent definitions hash identically.
2. **Resolved children.** Each `Global(h)` already carries the hash of its target, so a
   `Def`'s hash transitively commits to its entire dependency graph — a Merkle DAG of code.
3. **Canonical field order.** Struct fields, enum variants, and effect-row/error-set members
   are sorted by a fixed total order before encoding, so set-like data has one encoding.
4. **No incidental data.** Formatting, comments, doc strings, and source spans are excluded.

Properties this buys (see `01` §8): reproducible builds, dependency conflicts as mere
distinct hashes, free renames, automatic deduplication of identical code, and "has this exact
hash been audited before?" as a first-class audit query.
