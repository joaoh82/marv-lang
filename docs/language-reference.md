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
- Comments: `//` line, `///` doc. No block comments. (Doc comments are currently dropped by
  the formatter — see the roadmap.) **[impl]**
- Bindings: `let` (immutable), `var` (mutable). There is no `null`. **[impl]** (`var`
  reassignment is **[design]** pending the mutation surface task.)
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
debug). *(Note: the interpreter and backends currently compute integers at 64-bit width;
per-width semantics are roadmap.)*

### Aggregates
- **struct** (product): `struct Point { x: f64, y: f64 }`. Value semantics. Declarations and
  field projection are **[impl]**; struct *literals* are **[design]** (mutation/construction task).
- **enum** (sum): `enum Shape { Circle(f64), Rect(f64, f64) }`, matched exhaustively with
  `match`. Declarations, constructor application (`Shape.Circle(r)`), and `match` — with
  payload-binding constructor patterns and the `_` wildcard — are **[impl]** end to end:
  parsed, lowered to `Ctor`/`Match`, exhaustiveness-checked, and run by the interpreter. See
  `examples/color.mv` and the `std/` prelude.
- **array** `[N]T`, **slice** `[]T`, **tuple** `(A, B)`. Types parse **[impl]**; literals and
  indexing are **[design]**.
- **optional** `?T` = `Option[T]` — the only way to express absence. `Option`/`Result` are
  written in marv (`std/`) and now parse + lower **[impl]**; the `?T`/`!T` *sugar* and `?`
  propagation are still **[design]**.
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
owned values. **References are second-class** (`&T`/`&mut T`): they may be passed *down* into
a call but never stored in a field, returned, or captured — so a reference can never outlive
its call and all aliasing reasoning is local. The checker enforces this (escaping-reference
diagnostics). **`linear`** types must be consumed exactly once (forgetting to `close` a
`File` is a compile error). Allocation is explicit via an `Alloc` capability — a function
with no `Alloc` parameter provably performs no heap allocation.

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

## 6. Errors: inferred sets **[core]** (surface: **[design]**)

Errors are values. `!T` is an error union whose set is **inferred** from the body; `e?`
propagates upward; `match` on errors is exhaustive. No exceptions, no panics-as-control-flow.
The checker infers and checks error sets (and `marv/errorSet` reports them); the `error`/`!T`/`?`
surface is the error-handling task.

```marv
fn load(fs: Fs, path: str) -> !Config {
    let bytes = fs.read(path)?    // FileError flows in
    parse_config(bytes)?          // ParseError flows in  →  inferred set = FileError ∪ ParseError
}
```

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
  values; violations abort with a structured report.
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
lists), `let`/`var` bindings, `if`/`else(-if)`, `match` (constructor + `_` patterns, payload
binding), enum constructor application, generic type arguments (`Option[T]`), the binary
operators (`+ - * / % == != < <= > >= and or`), function calls and recursion, field
projection, and `requires`/`ensures` contracts. That is enough for the
[`examples/`](../examples) that run end to end (`factorial`, `arithmetic`, `clamp`, `color`,
…), the `std/` prelude (`option`, `result`), and the M4/M6 gates. Everything still marked
**[core]**/**[design]** above is the surface roadmap — tracked in the project tracker, ordered
construction/mutation → loops → errors → generics (checking) → capabilities → collections.
