# The Core IR and content hashing

> **The Core IR is identity.** Surface syntax is thin sugar; the unit of identity
> is the `blake3`-256 content hash of a definition's Core IR (ANF + de Bruijn,
> names erased, children resolved to hashes → a Merkle DAG of code).
> — `spec/README.md`, architecture concepts

This is what gives free renames, automatic deduplication of identical code,
reproducible builds, and "has this hash been audited before?" as a first-class
query. The normative description is
[`../spec/02-grammar-and-core-ir.md`](../spec/02-grammar-and-core-ir.md) §§C–F.

## Where it lives

- `marv_core::ir` — the Core IR data model (`Type`, `EffectRow`, `Atom`, `Core`,
  `Branch`, `Pred`, `Def`, `Hash`), mirroring `spec/02` §C exactly.
- `marv_core::lower::lower_module` — AST → Core lowering: desugaring, ANF
  normalization, de Bruijn conversion. Returns a `LoweredModule` of `DefEntry`
  values, each pairing a `Def` with its content `Hash`.
- `marv_core::hash` — the canonical encoding and `blake3` content hash of a
  `Def` (`spec/02` §F), plus `symbol_hash` for cross-definition references.

## Current status: M1 (front end → Core)

M1 lowers the bounded AST the M0 parser actually produces (module headers,
imports, `struct`/`fn`, the small type language, `let`/`var`, `return`/`if`/expr
tails, and value expressions) into the canonical Core IR, and content-hashes
each definition.

### What lowering does

| Step | Effect |
|------|--------|
| Desugaring (`spec/02` §D) | `if`/`else` → `Match` on a `bool` (branches ordered false-then-true); `enum` → `DefKind::Enum`; constructor application → `Ctor { ty, tag, fields }`; `match` → `Match` with branches ordered by variant tag and `binds` = the pattern's bound arity; method call `a.m(x)` → curried `App(App(m, a), x)`; multi-argument calls curried; nullary call/`fn` over a synthesized `()`. |
| ANF normalization | every non-atomic subexpression is hoisted into a `let`, left-to-right, so all operands are atomic and evaluation order is explicit. |
| de Bruijn conversion | variable names are erased; bound variables become indices. Built with stable de Bruijn *levels*, then finalized to indices in one pass. |
| Content hashing (`spec/02` §F) | `blake3`-256 over a canonical, version-prefixed binary encoding: no names, set-like effect/error rows sorted, positional aggregates kept in order, `Global`/`Nominal` children carried by their 32-byte hash. |

### Acceptance gate (met)

Alpha-equivalent surface programs — same logic, different *local* names or
formatting — lower to **identical** Core hashes. The gate (plus hand-written
lowering goldens) lives in `crates/marv-core/tests/golden.rs`. Consequences the
tests pin down:

- Renaming parameters/`let` bindings or reformatting does **not** change a hash.
- A definition's own name is **not** part of its identity (`add` and `plus` with
  identical bodies hash the same), but a *called* function's name **is** (calling
  `neg` vs `negate` differs).
- Structurally identical structs deduplicate (field *names* are erased; field
  order and `linear`-ness are significant).

### Deferred, and honest about it

Two spec features are intentionally not in M1 and are documented at their call
sites in the source:

- **Effect/error-row inference.** Every lowered arrow currently carries the empty
  row; a declared `pure fn` is therefore already exact, while a plain `fn` uses
  the empty row as a placeholder. Inferring real capability/error rows is M2.
- **Reference resolution to content hashes.** Cross-definition references
  (`Global` values, `Nominal` types) resolve via `symbol_hash` — a stable hash of
  the resolved, module-qualified *name* — rather than the target's own Core hash.
  This keeps lowering total and side-steps cyclic hashing for recursive
  definitions. Promoting these to true content hashes (so identical code
  deduplicates transitively) is content-store work (M7). Both honour the §F
  encoding rules as written.

Constructors and `match` resolve against an enum registry built from the modules
being lowered: `lower_module` sees the current module's enums, while
`lower_modules` shares one registry across a set (a prelude plus its dependents)
so a `match`/constructor can reference an enum imported from another file — which
is how `std/result.mv` resolves the `Option` it imports. The variant *names* the
checker needs for exhaustiveness travel as non-hashed `DefEntry::enum_variants`
metadata, since the names-erased `Def` cannot carry them.

Other `spec/02` §D desugarings (`?`, `while`, `for`, optional/error sugar)
concern surface forms the parser does not yet produce; they slot into the same
lowering machinery as the grammar grows.
