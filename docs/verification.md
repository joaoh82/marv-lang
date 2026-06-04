# Contracts and layered verification (M6)

marv functions carry machine-checkable contracts — `requires` preconditions and
`ensures` postconditions (`spec/01` §7). They are written between the signature
and the body, each on its own line; `ensures` may mention `result`:

```marv
pure fn clamp(x: i32, lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures result >= lo and result <= hi
{
    if x < lo { lo } else if x > hi { hi } else { x }
}
```

Verification is **layered**, and the toolchain is honest about which tier gave
an answer:

- **Tier 0 — types/effects/capabilities/error sets/linearity.** Always statically
  guaranteed for *all* code (the M2 checker). This is not about contracts.
- **Tier 1 — runtime contracts.** In the debug runner (`marv run`, the
  interpreter), every `requires` is checked before the body executes and every
  `ensures` after, against the actual argument and result values; every loop
  `invariant` is checked at the loop header (entry and each re-entry). A violation
  aborts with a structured report (showing the offending concrete values). This
  holds for *all* contracts, regardless of whether Tier 2 can reason about them.
- **Tier 2 — static proof (verified subset).** `marv verify` (`marv-verify`)
  discharges contracts with an SMT solver (z3, over SMT-LIB via `easy-smt`) for a
  decidable-ish subset, returning a **proof** or a **counterexample**.

## How a function becomes a proof

For a pure function over integers and booleans, the body is symbolically
evaluated into an SMT term for `result`: `if`/`else` becomes `ite`, arithmetic
and comparisons map to their SMT operators, `let` binds, parameters are SMT
constants. A constant `res` is set equal to that term, the preconditions are
asserted, and then — per postcondition `P` — the solver is asked whether
`requires ∧ res = body ∧ ¬P` is satisfiable:

- **unsat** ⇒ no input violates `P` ⇒ **proved**.
- **sat** ⇒ the model is a concrete **counterexample** (`spec/03` §4.3):

```sh
$ marv verify buggy_clamp.mv
FAILED   math.clamp  — postcondition `(result >= lo and result <= hi)` can be violated
    obligation: (result >= lo and result <= hi)
    counterexample: { x = -1, lo = 0, hi = 0, result = -1 }
```

The agent now has a failing input and the offending clause to iterate against.

## The verified subset (and honest boundaries)

Tier 2 currently covers: **pure** functions; parameters/result of integer or
boolean type; bodies of arithmetic (`+ - *`), comparisons, boolean `and`/`or`/
`not`, `let`, and `if`/`else`; contracts built from those comparisons and
`and`/`or`/`not`.

Outside that subset, `verify` returns `unsupported` with a reason and the
**fallback** to Tier-1 runtime checks — it never guesses. Notable current
exclusions (each a deliberate `unsupported`, not an unsound `proved`):

- **Integer `/` and `%`** — marv truncates toward zero while SMT `div`/`mod` are
  Euclidean; rather than emit an unsound encoding, division is out-of-subset.
- **Function calls, aggregates/ADTs, loops, bounded quantifiers, floats** —
  future subset extensions.
- **No `z3` on `PATH`** — reported as `unsupported` (solver unavailable), same
  fallback.

The fallback is real: a function `verify` calls `unsupported` is still fully
contract-checked at runtime under `marv run`. For example `half(x) = x / 2` with
`ensures result <= x` is `unsupported` at Tier 2 (division), but `marv run half
-3` aborts with `postcondition violated: ensures result <= arg0`.

## The protocol

The same logic is exposed as `marv/verify` (`spec/03` §3.3): given a snapshot and
a `def`, it returns `{ "status": "proved", "tier": 2 }`, or `{ "status":
"failed", "obligation", "counterexample": {…}, "message" }`, or `{ "status":
"unsupported", "reason", "fallback": "runtime-checked (Tier 1)" }`.

## The encoding convention

Contract atoms are names-erased like the rest of the Core IR, using a **flat**
convention (distinct from the body's de Bruijn spine): `Var(k)` is the k-th
parameter and `Var(n)` (n = arity) is `result`. Lowering
(`marv_core::lower`), the Tier-1 interpreter, and the Tier-2 verifier all share
it, and `marv_core::render_pred` turns a predicate back into readable text.
