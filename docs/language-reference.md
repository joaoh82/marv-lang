# marv language reference

This is a practical reference for the marv language: what it is, how each feature behaves,
and — importantly — **what is implemented today versus designed**. The normative source of
truth is the specs ([`spec/01`](../spec/01-design-spec.md) design, [`spec/02`](../spec/02-grammar-and-core-ir.md)
grammar + Core IR); this page distills them for day-to-day use and is honest about the
front-end surface, which is a growing subset.

Legend: **[impl]** works end to end from `.mv` source today · **[core]** supported in the
Core IR / checker / a backend but not yet expressible in surface syntax (reachable via
`*.core.json` snapshots) · **[design]** specified, not yet built.

---

## 1. Philosophy

Every rule serves one of three properties: **explicitness** (types, effects, capabilities,
error sets, ownership are written down or trivially recoverable), **locality** (a function is
understood from its signature plus body — no global inference, no action at a distance), and
**verifiability** (what code does and may do is machine-checkable, and provable for a
subset). See [`spec/01`](../spec/01-design-spec.md) §1.

## 2. Lexical & surface

- Files are UTF-8, extension `.mv`. **[impl]**
- Comments: `//` line, `///` doc. No block comments. **[impl]** — `///` doc comments attach to
  the item below them, are preserved by the canonical formatter (normalized to one `/// text`
  line each), and are excluded from a definition's content hash (`spec/02` §F). `//` and `////…`
  are ordinary comments and are dropped.
- Bindings: `let` (immutable), `var` (mutable). There is no `null`. **[impl]** — `var`
  reassignment (`x = e`), field updates (`p.x = e`), and struct literals all parse, lower, and
  run (see §4). Assigning a `let` (or a parameter) is a compile error.
- Naming is formatter-enforced: `snake_case` values/functions, `UpperCamelCase`
  types/interfaces/capabilities, `SCREAMING_SNAKE` consts.
- Blocks are expressions; the final expression is the block's value. `return` exists for
  early exit. **[impl]**
- **One canonical form.** `marv fmt` is the parser's inverse: exactly one textual form per
  program, full parenthesization, normalized spacing. No style options. **[impl]**

## 3. Types

### Primitives **[impl]**
`i8 i16 i32 i64 i128 isize`, `u8 u16 u32 u64 u128 usize`, `f32 f64`, `bool`, `char`, `str`,
`()`. No implicit conversions — widening/narrowing use explicit `as` (narrowing checked in
debug). **`char` literals** (`'a'`, `'\n'`) and **`as` casts** (`(n as u8)`) parse, lower to
`Core::Cast`, check (scalar↔scalar only — `E0104` otherwise), and run **[impl]**: an integer
target truncates/wraps to its width *identically across the interpreter, Cranelift, and WASM*
(differential-tested in `tests/run/casts.mv`), a `char` is its Unicode code point, and `bool`
maps nonzero→true. The narrowing range check is enforced statically for constant operands
(`256 as u8` → `E0104`). *(Note: the value domain is still 64-bit — sub-width semantics show
up only at the cast boundary; per-width **arithmetic** wrapping remains roadmap.)*

### Aggregates
- **struct** (product): `struct Point { x: f64, y: f64 }`. Value semantics. Declarations,
  field projection, **struct literals** (`Point { x: 1, y: 2 }`, fields in any order), and
  field assignment (`p.x = e`) are **[impl]** — literals lower to `Ctor { tag: 0, … }` with
  fields reordered into declaration order, so write-order does not affect identity.
- **enum** (sum): `enum Shape { Circle(f64), Rect(f64, f64) }`, matched exhaustively with
  `match`. Declarations, constructor application (`Shape.Circle(r)`), and `match` — with
  payload-binding constructor patterns and the `_` wildcard — are **[impl]** end to end:
  parsed, lowered to `Ctor`/`Match`, exhaustiveness-checked, and run by the interpreter. See
  `examples/color.mv` and the `std/` prelude.
- **array** `[N]T`, **slice** `[]T`, **tuple** `(A, B)`. Types parse **[impl]** (both `[]T`
  and the fixed `[N]T` form); **index reads** `a[i]` parse and lower to `Prim{Index}`, and
  `len(x)` lowers to `Prim{Len}` (a builtin, not a call) — both run on the interpreter for
  `str`/aggregate operands **[impl]**. Array/slice *literals*, index *stores* (`a[i] = e`),
  and backend execution of `len`/index over aggregates are **[design]** — they await aggregate
  codegen (MARV-9).
- **optional** `?T` = `Option[T]` — the only way to express absence. `Option`/`Result` are
  written in marv (`std/`) and parse + lower **[impl]**; the `?T`/`!T` *sugar* and the postfix
  `?` propagation operator now parse and lower too **[impl]** (`!T` → `Result[T, error-union]`;
  see §6).
- **function type** `fn(A) -> C`, optionally with an effect row `fn(A) -{Io}-> C`.

### Aliases, constants, generics
`type Meters = f64`, `const MAX: u32 = 5`, `fn map[T, U](xs: []T, f: fn(T) -> U) -> []U`
(explicit, monomorphized). Generic **parameter lists** on `fn`/`enum` and generic type
**arguments** (`Option[T]`, `Result[T, E]`) now parse and lower — a bare parameter becomes a
`Type::Var` de Bruijn index. Generic *checking* (instantiation, equality) and bounds
(`[T: Ord]`) remain **[design]** (generics task).

### Interfaces **[design]**
Bounded, coherent (one impl per type per interface), deterministically resolved. `interface
Ord[T] { fn cmp(a: &T, b: &T) -> Ordering }` + `impl Ord[i32] { … }`.

## 4. Memory model **[core]**

No GC, no lifetime annotations. **Mutable value semantics**: values are conceptually copied
on assignment/pass (compiler optimizes to moves/in-place); no shared mutable aliasing of
owned values. This is **[impl]** through the front end: a `var x = e` reassignment lowers to
ANF *rebinding* (a fresh binding shadows the old — Core has no mutable cell), and a field
update `p.x = e` rebuilds the aggregate from the other fields' projections, so mutating a copy
never affects the original (`examples/mutation.mv`). Because `if`/`match` are terminal block
*tails*, branch-local mutation needs no join lowering; cross-*iteration* mutation is handled by
loops (§4.1). **References are second-class** (`&T`/`&mut T`): they may be passed *down* into
a call but never stored in a field, returned, or captured — so a reference can never outlive
its call and all aliasing reasoning is local. A reference is taken with the prefix `&e`/`&mut e`
expression operator (`f(&x)`); this is **[impl]** through the front end and checker (the
reference-of expression lowers to a `Core::Ref` the checker types as `&T`). The checker enforces
the second-class rule (escaping-reference diagnostics). **`linear`** types must be consumed exactly once (forgetting to `close` a
`File` is a compile error). Allocation is explicit via an `Alloc` capability — a function
with no `Alloc` parameter provably performs no heap allocation.

### 4.1 Loops **[impl]**

`while` and `for` are **statements** (they have no value), so ordinary code follows them in the
same block. Both lower to the Core `Loop { state, invariant, cond, body }` node and run across
the interpreter, Cranelift, and WASM backends (`examples/loops.mv`).

```marv
pure fn sum_to(n: i64) -> i64
    ensures (result >= 0)
{
    var sum: i64 = 0
    var i: i64 = n
    while (i > 0)
        invariant (i >= 0)   // a Tier-1/Tier-2 proof obligation (§7)
    {
        sum = (sum + i)
        i = (i - 1)
    }
    sum
}
```

The loop-carried `var`s (the mutable bindings the body reassigns — here `sum` and `i`) are
threaded functionally: they enter the loop as its `state`, the body computes their next values,
and the loop evaluates to their final values, which the enclosing scope rebinds. There are no
mutable cells in Core; this is the cross-iteration form of mutable value semantics (§4).

A `while` head carries zero or more `invariant` clauses. `for x in xs { … }` desugars to an
index-driven loop (`spec/02` §D); iterating a real slice needs `len`/element indexing
(collections, roadmap), so `for` parses and lowers today but awaits collection support to run.

Today's loop lowering covers **straight-line** bodies (assignments and nested loops). A body
whose tail is an `if`/`match`/`return` — which would need to thread carried `var`s through a
branch join — is rejected for now; lift the branch out or restructure as straight-line updates.

## 5. Effects & capabilities **[core]** (surface: **[design]**)

Side effects are not ambient. A function obtains the power to perform an effect only by
receiving a **capability** parameter, and its **effect row** records which it uses (inferred
in the body, written in signatures). `pure` asserts the empty row.

```marv
fn read_config(fs: Fs, path: str) -> !Config { … }   // can do FS I/O, nothing else
pure fn clamp(x: i32, lo: i32, hi: i32) -> i32 { … }  // no capabilities, no I/O, no allocation
```

Standard capabilities: `Io` (root) and narrower `Fs`, `Net`, `Clock`, `Rand`, `Env`, `Alloc`
(see [`std/`](../std)). Capabilities are **unforgeable** — received or narrowed, never
constructed. The checker enforces capability provenance and effect/error subsumption today
over Core; performing a capability lowers to `Core::Perform`, which surface syntax does not
yet emit (you can express it via `*.core.json`). On WebAssembly a capability is a host import
the page chooses to provide — see [platform support](platform-support.md).

## 6. Errors: inferred sets **[impl]**

Errors are values. You declare an error type with `error E { Variant, ... }`; referencing a
variant (`E.Variant`) **raises** it (lowers to `Core::Raise`). A function returns an error
union `!T` (success type `T`; bare `!` is `!()`), whose error *set* is **inferred** from the
body — never written in the signature. The postfix `e?` propagates: it yields `e`'s success
value and lets its errors flow into the enclosing function. `match` over a caught error value
is exhaustive. No exceptions, no panics-as-control-flow.

The error set is inferred with **full cross-call propagation**: a caller that uses `?` on a
fallible function inherits that function's entire inferred set, computed to a fixpoint over the
call graph. `marv/errorSet` reports the result (see `docs/query-server.md`).

```marv
error ParseError { Empty, Overflow }

fn digit(b: i64) -> !i64 {
    if (b < 0) { ParseError.Empty } else { b }   // raises ParseError → inferred set {ParseError}
}

fn sum_two(x: i64, y: i64) -> !i64 {
    let a = digit(x)?                              // ParseError flows in
    let b = digit(y)?                              // (already present)
    (a + b)                                        // sum_two's inferred set = {ParseError}
}
```

See `examples/errors.mv`. Status notes: the error union's value type lowers faithfully to
`Result[T, error-union]`, but the inferred set is carried as the function's effect row (the
`error-union` type slot is a fixed marker), so `?` is a success-value pass-through and a `!T`
value behaves as its success `T`. Errors propagate at runtime by unwinding (a `Raise` aborts),
so error programs run on the interpreter; aggregate/`Result` codegen is MARV-9. Capability-op
error sets and cross-*module* propagation arrive with MARV-6 / MARV-14.

## 7. Contracts & layered verification **[impl]**

Functions carry `requires`/`ensures`; loops carry `invariant`. `ensures` may mention `result`
(and `old(e)` — **[design]**). `forall`/`exists` over finite ranges exist in the predicate
language (surface **[design]**).

```marv
pure fn clamp(x: i32, lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures result >= lo and result <= hi
{ if x < lo { lo } else if x > hi { hi } else { x } }
```

Three tiers, and the toolchain is honest about which gave an answer:

- **Tier 0** — types/effects/capabilities/error-sets/linearity. Always statically guaranteed.
- **Tier 1** — runtime contracts. `marv run` checks every `requires`/`ensures` against actual
  values, and every loop `invariant` each time the condition is tested (loop entry and every
  re-entry); violations abort with a structured report showing the offending concrete values.
- **Tier 2** — SMT proof for the verified subset (pure functions over ints/bools today; ADTs,
  arrays, bounded quantifiers, loop invariants are roadmap). `marv verify` returns `proved`,
  `failed` with a **counterexample**, or `unsupported` (→ falls back to Tier 1). See
  [verification.md](verification.md).

## 8. Modules & content-addressed reuse **[impl]**

Source uses `mod`/`import` and ordinary names, but the **unit of identity is the content hash
of a definition's Core IR** (`spec/01` §8). Consequences: reproducible builds (lockfile pins
hashes), no dependency hell (different versions are different hashes that coexist), **free
renames** (a name is a label over a hash — renaming, even of a recursive function, changes no
hash), and dedup + provenance ("has this exact hash been reviewed?"). `marv commit` freezes
definitions into the store. See [store.md](store.md).

## 9. Compilation targets

Native via **Cranelift** (JIT today; AOT + an LLVM release backend are roadmap) and
**WebAssembly** (capabilities as host imports; component/WIT packaging is roadmap). The
tree-walking **interpreter** is the reference semantics oracle every backend is differentially
tested against. See [run-and-codegen.md](run-and-codegen.md) and [platform-support.md](platform-support.md).

## 10. Concurrency **[design]**

Structured and capability-gated: tasks spawned through a `Spawn` capability within a scope
that joins children before returning; message-passing channels of `linear`/value types; data
races excluded by the same no-shared-mutable-aliasing rule. Deferred past the current
milestones.

## 11. The escape hatch **[design]**

`unsafe` is the explicit, auditable boundary (FFI, raw pointers, custom synchronization). It
is visible in the signature, requires a `SAFETY:` justification comment, and is greppable
(`marv/unsafeSites`).

---

## What you can actually write today

The parser accepts: `mod`/`import`, `struct`/`enum`/`fn` (incl. `pure fn`, generic parameter
lists), `let`/`var` bindings, assignment (`x = e`, `p.x = e`), `if`/`else(-if)`, `match`
(constructor + `_` patterns, payload binding), enum constructor application, struct literals
(`Name { f: e, … }`), index reads (`a[i]`), `len(x)`, `char` literals (`'a'`) and `as` casts
(`(n as u8)`), array/slice types (`[N]T`, `[]T`), `while`/`for` loops with `invariant` clauses,
generic type arguments (`Option[T]`), the binary
operators (`+ - * / % == != < <= > >= and or`), the prefix unary operators
(`-e`, `not e`, `&e`, `&mut e` — `&`/`&mut` take a second-class reference, `spec/01` §4),
function calls and recursion, field
projection, and `requires`/`ensures` contracts. That is enough for the
[`examples/`](../examples) that run end to end (`factorial`, `arithmetic`, `clamp`, `color`,
`mutation`, `loops`, `casts`, …), the `std/` prelude (`option`, `result`), and the M4/M6 gates.
Everything still marked **[core]**/**[design]** above is the surface roadmap — tracked in the
project tracker, ordered errors → generics (checking) → capabilities → collections.
