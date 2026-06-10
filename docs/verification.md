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
  Tier 1 also carries the **bounds check** on every runtime array/slice element
  read and slice element store (MARV-34): a subscript outside `0..len` aborts
  with the offending index and length — in the interpreter *and* in debug
  Cranelift/wasm codegen (`marv build --release` omits it; see
  [run-and-codegen.md](run-and-codegen.md)).
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

## Loops: proving `invariant`s (MARV-22)

A `while` loop is discharged with the standard Hoare-style verification
conditions over its `invariant` (the conjunction of its `invariant` clauses):

1. **Initiation** — under the `requires` (and the `if` branches guarding the
   loop), the invariant holds on the initial values of the loop-carried `var`s.
2. **Consecution** — for an *arbitrary* carried state satisfying the invariant
   and the loop condition, one pass through the body re-establishes the
   invariant on the next values (a body whose tail is an `if`/`else` joins
   branchwise, mirroring MARV-21).
3. **Use** — after the loop, exactly `invariant ∧ ¬cond` is known about the
   final carried values; the `ensures` are then discharged from that.

A failed initiation or consecution reports `failed` with a counterexample, e.g.:

```sh
$ marv verify wrong.mv
FAILED   wrong.bad_preserve  — loop invariant `s0 <= 0` is not preserved by the loop body
    obligation: s0 <= 0
    counterexample: { n = 1, s0 = 0, s0' = 1 }
```

Core erases names, so carried slots are labeled positionally (`s0`, `s1`, … in
the order the loop carries them; primed values are post-iteration). Two honest
caveats:

- **The invariant is all the prover keeps.** An invariant that holds but is too
  weak to imply an `ensures` yields a counterexample for that postcondition —
  e.g. `sum_to` with only `invariant (i >= 0)` cannot prove
  `ensures (result >= 0)`; adding `invariant (sum >= 0)` makes it prove (see
  [`examples/loops.mv`](../examples/loops.mv)). Such a counterexample is
  relative to the invariant abstraction: it picks an exit state the invariant
  *allows*, which a real execution may never reach. Strengthening the invariant
  is the fix either way.
- **Proofs are partial correctness.** Tier 2 does not prove termination; a loop
  that never exits satisfies its `ensures` vacuously.

A loop *without* an invariant still verifies soundly — nothing beyond `¬cond` is
assumed about its exit state. And a loop `invariant` is an obligation in its own
right: a function with an invariant but no `requires`/`ensures` is still checked
(and reported) by `marv verify`.

## The verified subset (and honest boundaries)

Tier 2 currently covers: **pure** functions; parameters/result of integer or
boolean type; bodies of arithmetic (`+ - *`), comparisons, boolean `and`/`or`/
`not`, `let`, `if`/`else`, and `while` loops (with or without `invariant`s);
contracts built from those comparisons and `and`/`or`/`not`.

Outside that subset, `verify` returns `unsupported` with a reason and the
**fallback** to Tier-1 runtime checks — it never guesses. Notable current
exclusions (each a deliberate `unsupported`, not an unsound `proved`):

- **Integer `/` and `%`** — marv truncates toward zero while SMT `div`/`mod` are
  Euclidean; rather than emit an unsound encoding, division is out-of-subset.
- **Function calls, aggregates/ADTs, bounded quantifiers, `old(e)`, floats** —
  future subset extensions (the rest of MARV-11).
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

Loop-invariant atoms are the exception: they are de Bruijn *indices* into the
loop-header environment (parameters, enclosing `let`s, then the carried slots
innermost), the same convention the Tier-1 interpreter evaluates — which is why
their carried slots render positionally (`s0`, `s1`, …) rather than by name.
