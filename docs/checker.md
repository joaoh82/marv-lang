# The checker: types, effects, capabilities, errors, references, linearity

> **Tier 0 — types/effects/capabilities/error sets/linearity.** Always
> statically guaranteed for *all* marv code. This alone bounds a function's
> power and failure surface.
> — `spec/01-design-spec.md` §7

This is marv's always-on guarantee: every program, before it runs, has a checked
type, a known set of capabilities it may exercise, a known set of errors it may
raise, and a proof that its references never escape and its `linear` resources
are used exactly once. The normative description is
[`../spec/02-grammar-and-core-ir.md`](../spec/02-grammar-and-core-ir.md) §E
(typing/effect judgments) and [`../spec/01-design-spec.md`](../spec/01-design-spec.md)
§§3–7.

## Where it lives

- `marv_types::check` — the checker over the Core IR. One pass
  (`check_def`) performs all six families of check, because they share one
  traversal and one typing environment.
- `marv_types::world` — the **`World`**: the declaration environment the
  judgments are open over (signatures of other definitions, capability/error/
  enum/struct declarations), keyed by the `symbol_hash` a Core term references.
- `marv_types::diagnostic` — the `Diagnostic` type (`spec/03` §2): stable
  `Code`, severity, span, message, related notes, and fix-carrying repairs.
- `marv_types::check_module` — the whole-module entry point: build the `World`
  from a `LoweredModule`, then check every definition.

## Current status: M2 (checker)

The checker operates **over the Core IR** (`marv-core`), exactly as `spec/02` §E
specifies — surface names are already erased, operands are atomic (ANF), and
binders are de Bruijn indices. There is **no global inference**: a definition's
signature (its arrow type and declared effect/error row) is taken as given, and
inference happens only *inside* the body (`spec/01` §1).

### The six checks

| Family | What it enforces | Rule in `spec/02` §E |
|--------|------------------|----------------------|
| **Types** | every node synthesizes a type; arguments match parameters; the body's type is the return type; primitives are well-typed | `(Var)` `(Global)` `(App)` `(Let)` `(Match)` |
| **Effect-row inference** | the capabilities a body may exercise are unioned up the ANF spine (`App` folds in the callee's row, `perform` adds its capability) and must be a **subset** of the declared row | `(App)` `(Perform)`, "effect & error subsumption" |
| **Capabilities** | a `perform` needs a capability *value in scope* (no ambient authority), and capabilities are **unforgeable** — never produced by `Ctor`/`Prim`, only received or narrowed | `(Perform)`, "capability provenance" |
| **Error-set inference** | the errors a body may raise (`raise`, propagated `perform`/call errors) must be a **subset** of the declared error set | `(Raise)`, "effect & error subsumption" |
| **Second-class references** | a `&T`/`&mut T` may be passed *down* but never stored in a struct field, placed in an aggregate, or returned | "second-class references" |
| **Linearity** | each `linear` binding is consumed **exactly once on every control path**, tracked as per-binder `(min, max)` use counts that compose across `let` sequencing and `match` branches | "linearity" |

### Diagnostics carry fixes

Every diagnostic follows `spec/03` §2 and, for the five mechanically-derivable
cases that section names, ships a best-first `Fix`:

| Case | Code | Fix |
|------|------|-----|
| missing capability parameter | `E0110` | add `name: Cap` to the parameter list |
| missing error in the declared set | `E0120` | add the error to the declared set |
| non-exhaustive `match` | `E0130` | add the missing arm(s), named |
| unused / duplicated `linear` value | `E0140` / `E0141` / `E0142` | consume it once (or remove the extra use) |
| escaping reference | `E0150` | store/return the referent by value |

### Error-code catalog

Codes are **stable** (`spec/03` §6): the string never changes meaning, so an
agent may key behaviour on it. The full set is `marv_types::Code::catalog()`.

| Code | Meaning |
|------|---------|
| `E0101` | a value's type does not match the type its context requires |
| `E0102` | a non-function value is applied as if it were a function |
| `E0103` | a primitive operation is applied to an operand of the wrong type |
| `E0104` | an `as` cast is applied between types with no defined conversion (or a constant that does not fit its narrowing target) |
| `E0110` | a function exercises a capability its declared effect row does not list |
| `E0111` | a `perform` names a capability that is not a capability value in scope |
| `E0112` | a capability was produced by construction instead of being received or narrowed |
| `E0120` | a function can raise an error its declared error set does not list |
| `E0130` | a `match` does not cover every variant of its scrutinee |
| `E0140` | a `linear` value is never consumed |
| `E0141` | a `linear` value is consumed more than once along some path |
| `E0142` | a `linear` value is consumed on some control paths but not all |
| `E0150` | a second-class reference escapes its call frame |
| `E0160` | a generic type argument does not implement its parameter's interface bound (no `impl`) |
| `E0161` | two impls exist for the same interface and type (coherence requires one) |

The `E016x` codes (MARV-5) come from monomorphization (`spec/01` §§3.3–3.4): they
are raised over a module's generics/impl metadata, not its Core, so they fire for
every generic *instantiation* whose bounded type argument has no coherent `impl`,
and for any duplicate `impl Iface[Type]`.

### Acceptance gate (met)

A table of `(program → expected diagnostic-with-fix)` covering each rule, plus
positive tests that well-typed programs check clean
(`crates/marv-types/tests/rules.rs` and `tests/wellformed.rs`).

## Scope honesty

Two boundaries are worth stating plainly, in the spirit of the M1 scope note:

1. **Spans are definition-granular.** `spec/02` §F rule 4 excludes source spans
   from the Core IR, so the checker — which runs over Core — emits `None` for
   `Diagnostic.span`/`Edit.span`. Real source spans are stamped one layer up: the
   `marv-db` analysis pass pairs each diagnostic with its definition's header span
   (from the parser's `ItemSpan`s) and resolves a fix's insertion point where one
   is mechanically derivable (MARV-12). So on the wire a *source* diagnostic
   points at its **definition's header**, not yet at an exact sub-expression —
   that finer grain would need a Core→source map the identity model omits.
   Core-ingested files have no source text, so their spans stay `null`. The
   `code`, `message`, and fix `title`/`new_text` are always populated regardless.

2. **Which rules the front end can reach.** The front end now emits `fn`/`struct`/`enum`/
   `interface`/`impl` over arithmetic, `if`, `match`, calls, enum constructors, field access,
   **struct literals and field/`var` assignment** (lowered to `Ctor`/`Proj`), **index reads**
   (`Prim{Index}`), **`error`/`!T`/`?`** (lowered to `Raise` with inferred error sets, MARV-3),
   and — since MARV-6 — **capability `perform` and narrowing**: a method call on a value of a
   (non-generic) capability `interface` lowers to `Core::Perform`, a non-`pure` function's
   declared effect row is the set of its capability parameters, and the body's inferred row is
   checked against it (narrowing authorizes what a held capability attenuates to). So the
   capability rules are now reachable from real `.mv` source — `E0110` (missing capability),
   plus `E0111`/`E0112` over constructed Core — alongside the type (`E0101`/`E0102`/`E0103`),
   returned-reference, struct-field-reference, **`match` exhaustiveness (`E0130`)**, `Ctor`
   field-type, and `Prim` index/operand (`BadPrimOperand`) families. Linear consumption is now
   reachable from source through `linear struct` handles and `linear interface` resource
   capabilities such as `File`, `Listener`, and `Conn`; forgotten close, double close, and
   branch-only close paths report `E0140`/`E0141`/`E0142`. **The checker itself is complete over the whole Core IR**, independent of which
   surface forms the parser accepts.
