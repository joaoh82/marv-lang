# marv — Language Design Specification

Status: draft 0.1 · Audience: implementers (Claude Code) and human auditors

This document specifies *what marv is and why*. Exact syntax and the Core IR are in
`02-grammar-and-core-ir.md`; the agent interface is in `03-compiler-protocol.md`.

---

## 1. Design philosophy

marv is authored by coding agents and audited by humans. Every rule below serves one of
three properties:

- **Explicitness** — nothing important is implicit. Types, effects, capabilities, error
  sets, and ownership are all written down or trivially recoverable.
- **Locality** — a function is fully understood from its signature plus its body. No global
  type inference, no action at a distance, no ambient state.
- **Verifiability** — what code *does* and what it is *allowed to do* are readable from the
  surface, machine-checkable by the toolchain, and (for a defined subset) provable.

Concrete commitments that follow:

1. **No global inference.** Every function signature is fully annotated. Inference exists
   only *inside* a function body, never across boundaries.
2. **One canonical form.** The formatter is mandatory and is the inverse of the parser.
   There is exactly one textual form of any program. Diffs are minimal; review never spends
   attention on style.
3. **No hidden control flow, no hidden allocation** (from Zig). No exceptions, no implicit
   destructors at a distance, no operator overloading hiding calls, no implicit numeric
   coercion. Heap allocation happens only through an explicit allocator capability.
4. **No ambient authority** (from Austral). There is no global `print`, filesystem, clock,
   randomness, or network. Power is passed in as **capability** parameters, so a function's
   signature bounds everything it can do.
5. **Errors are values with inferred sets** (from Zig). The complete set of errors a
   function can produce is inferred and checkable from its type.
6. **Memory safety without GC and without lifetime annotations** via *mutable value
   semantics* + *second-class references* (from Hylo/Val), plus *linear types* for
   resources (from Austral).
7. **Contracts are first-class** (from Dafny/SPARK), always runtime-checked in debug, and
   statically discharged for a verified subset.

---

## 2. Lexical and surface overview

- Source files use extension `.mv` and are UTF-8.
- Comments: `//` line comment, `///` doc comment. **No block comments** (one way to do it).
- Bindings: `let` (immutable), `var` (mutable). There is no `null`.
- Naming convention is enforced by the formatter: `snake_case` for values/functions,
  `UpperCamelCase` for types/interfaces/capabilities, `SCREAMING_SNAKE` for `const`.
- Blocks are expressions; the final expression is the block's value (no `return` needed,
  though `return` exists for early exit).

The full grammar is in `02-grammar-and-core-ir.md`.

---

## 3. Types

### 3.1 Primitives

`i8 i16 i32 i64 i128 isize`, `u8 u16 u32 u64 u128 usize`, `f32 f64`, `bool`, `char`
(Unicode scalar), `str` (UTF-8 slice), `()` (unit). **No implicit conversions** — widening
and narrowing use explicit `as`, and narrowing `as` is checked in debug.

### 3.2 Aggregates

- **Struct** (product): `struct Point { x: f64, y: f64 }`. Value semantics by default.
- **Enum** (sum / tagged union): `enum Shape { Circle(f64), Rect(f64, f64) }`. Matched
  exhaustively.
- **Array** `[N]T` (fixed), **slice** `[]T` (view; see references), **tuple** `(A, B)`.
- **Optional** `?T` is sugar for `Option[T]` — the *only* way to express absence.
- **Function type** `fn(A, B) -> C`, optionally with an effect row (§5): `fn(A) -{Io}-> C`.

### 3.3 Type aliases, constants, generics

```marv
type Meters = f64
const MAX_RETRIES: u32 = 5

fn map[T, U](xs: []T, f: fn(T) -> U) -> []U { ... }   // generics are explicit, monomorphized
```

Generic parameters may carry interface bounds: `fn sort[T: Ord](xs: []T) -> []T`.

### 3.4 Interfaces (bounded, coherent, explicit)

Interfaces are marv's bounded polymorphism. Implementations are explicit and coherent (one
impl per type per interface); resolution is deterministic, and the toolchain can always
report *which* impl was selected (`marv/resolveImpl`, see `03`). There is no global
inference of impls and no orphan ambiguity.

```marv
interface Ord[T] {
    fn cmp(a: &T, b: &T) -> Ordering
}

impl Ord[i32] {
    fn cmp(a: &i32, b: &i32) -> Ordering { ... }
}
```

---

## 4. Memory model: mutable value semantics + second-class references

marv has **no garbage collector and no lifetime annotations**. The model is *mutable value
semantics*:

- Values have value semantics: assignment and passing by value are conceptual copies (the
  compiler optimizes to moves/in-place updates). There is **no shared mutable aliasing** of
  owned values.
- **References are second-class.** `&T` (shared) and `&mut T` (unique) may be *passed down*
  into a call, but **may not be stored in a struct field, put in a collection, or
  returned**. This single restriction removes the need for lifetime annotations entirely
  while preserving memory safety, because every reference provably cannot outlive the call
  that created it. Aliasing reasoning is therefore always **local**.

```marv
fn sum(xs: &[]i32) -> i64 {     // borrows a view; cannot store or return xs
    var total: i64 = 0
    for x in xs { total = total + (x as i64) }
    total
}
```

`&mut T` follows exclusive-mutation rules (one active unique reference at a time), checked
locally because references cannot escape.

### 4.1 Linear (resource) types

For things that must not be silently dropped or duplicated — file handles, sockets, locks,
unique buffers — use `linear`:

```marv
linear struct File { fd: i32 }

fn open(fs: Fs, path: str) -> !File { ... }
fn close(f: File) -> ()  { ... }   // CONSUMES f
fn read(f: &mut File, buf: &mut []u8) -> !usize { ... }
```

A `linear` value must be used **exactly once** (passed on or consumed). Forgetting to
`close` a `File` is a *compile error*, not a leak. This gives RAII-style safety with zero
hidden destructor calls — the consumption is visible in the code.

### 4.2 Allocation is explicit

There is no implicit heap. Growable structures take an `Alloc` capability:

```marv
fn collect[T](alloc: Alloc, it: Iter[T]) -> !List[T] { ... }
```

This makes allocation auditable, supports arena/region strategies trivially, and means a
function with no `Alloc` parameter provably performs no heap allocation.

---

## 5. Effects and capabilities

Side effects are not ambient. A function obtains the power to perform an effect only by
receiving a **capability** value as a parameter, and the function's **effect row** records
which capabilities it uses. The effect row is part of the type and is *inferred within a
body but written in signatures*.

```marv
// Signature alone proves: this can do filesystem I/O, nothing else.
fn read_config(fs: Fs, path: str) -> !Config { ... }

// `pure` asserts the empty effect row: no capabilities, no I/O, no allocation.
pure fn clamp(x: i32, lo: i32, hi: i32) -> i32 { ... }
```

Standard capability types: `Io` (root), and narrower derived ones `Fs`, `Net`, `Clock`,
`Rand`, `Env`, `Alloc`. Capabilities are **unforgeable** — you cannot construct one; you can
only receive one and optionally *narrow* it:

```marv
fn handler(io: Io) -> ! () {
    let fs = io.fs()          // narrow Io -> Fs (attenuation)
    write_log(fs, "started")  // can only touch the filesystem now
}
```

**Why this matters for marv specifically:** a human auditor verifies "this transform cannot
exfiltrate data" by reading one line (no `Net` parameter). And it *is* the browser sandbox
model (§9): to run untrusted, agent-generated code safely, simply don't hand it the
capabilities you don't want it to have.

---

## 6. Error handling: inferred error sets

Errors are ordinary values. `!T` is an error union whose error *set* is **inferred** from
the body; `?` propagates errors upward.

```marv
error FileError { NotFound, Permission }
error ParseError { Syntax, Eof }

// inferred error set = FileError ∪ ParseError, exposed in the type as `!Config`
fn load(fs: Fs, path: str) -> !Config {
    let bytes = fs.read(path)?       // may raise FileError
    parse_config(bytes)?             // may raise ParseError
}
```

The toolchain can report the exact inferred set (`marv/errorSet`, see `03`) so an agent
knows the complete failure surface without guessing. `match` on errors is exhaustive. There
are no exceptions and no panics-as-control-flow; `panic` exists only for truly unrecoverable
invariant violations and aborts.

---

## 7. Contracts and layered verification

Functions and loops carry machine-checkable specifications.

```marv
pure fn clamp(x: i32, lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures  result >= lo and result <= hi
{
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn fill(buf: &mut []u8, val: u8) -> ()
    ensures forall i in 0..len(buf): buf[i] == val
{
    var i: usize = 0
    while i < len(buf)
        invariant i <= len(buf)
        invariant forall j in 0..i: buf[j] == val
    {
        buf[i] = val
        i = i + 1
    }
}
```

Contracts use `requires`, `ensures` (`result` for the return value, `old(e)` for pre-state),
`invariant` (loops), `assert`, plus `forall`/`exists` over finite ranges.

**Layered guarantee** (be honest about what is and isn't automatic):

- **Tier 0 — types/effects/capabilities/error sets/linearity.** Always statically
  guaranteed for *all* marv code. This alone bounds a function's power and failure surface.
- **Tier 1 — runtime contracts.** In debug builds every `requires`/`ensures`/`invariant`
  is checked at runtime; violations abort with a structured report.
- **Tier 2 — static proof (verified subset).** For a defined decidable-ish subset
  (`pure` functions over integers, bools, arrays, algebraic data; bounded quantifiers),
  contracts are discharged statically by an SMT backend (`marv-verify`). Failures return a
  **counterexample** the agent can iterate against (see `03` `marv/verify`).

This continuum is deliberate: full automatic proof of a general-purpose language is *not*
solved. marv gives total static guarantees where they're cheap (Tier 0), runtime safety
everywhere (Tier 1), and real proofs where tractable (Tier 2).

---

## 8. Modules and content-addressed reuse

Source uses ordinary names and `mod` / `import`, but the *unit of identity* is the
**content hash of a definition's Core IR**, not its name or path (from Unison).

```marv
mod geometry
import std.io (Io, Fs)
import std.collections (List)
```

Consequences, all of which matter when an agent generates large volumes of code:

- **Reproducibility by construction.** A build is pinned to a set of hashes via a lockfile;
  the same hashes always produce the same program.
- **No dependency hell.** Two libraries depending on different versions of a third are just
  different hashes; both coexist.
- **Free, non-breaking renames.** A name is a label on a hash; renaming changes no hashes.
- **Automatic dedup & provenance.** Identical functions collapse to one hash; a human can
  ask "has this exact hash been reviewed before?" — a powerful audit primitive when most
  code is machine-written.

Hashing is alpha-canonical (de Bruijn Core IR), so semantically identical definitions hash
identically regardless of local variable names or formatting. Details in `02`.

---

## 9. Compilation targets

Two backends, both first-class:

- **Native.** Cranelift for fast dev/debug builds; LLVM (via `inkwell`) for optimized
  release builds. Same Core IR feeds both.
- **WebAssembly** via the **component model** / WASI, used for *both* server and browser.

The capability model and WASM compose cleanly: pure code needs no imports and is trivially
sandboxable; a module that wants the network must have been *handed* a `Net` capability,
which on the web is just a host import the page chooses to provide (or not). The property
that makes marv auditable (explicit capabilities) is the same property that makes it safe to
run untrusted agent-generated marv in a browser.

---

## 10. Concurrency (sketch, for a later milestone)

Concurrency is **structured** and **capability-gated**. Tasks are spawned through a `Spawn`
capability within a scope that joins all children before returning (no detached tasks, no
ambient executor). Communication is by message-passing channels of `linear` or value types;
because owned values have no shared mutable aliasing, data races are excluded by the same
rules that give single-threaded safety. Shared mutable state requires an explicit `unsafe`
region (§11) with documented invariants. (Full semantics deferred past M6.)

---

## 11. The escape hatch

`unsafe` is the explicit, auditable boundary for the rare cases the safe subset forbids
(self-referential structures, custom synchronization, FFI, raw pointers):

```marv
unsafe fn from_raw(ptr: *mut u8, len: usize) -> []u8
    // SAFETY: caller guarantees ptr is valid for `len` bytes for the call's duration.
{ ... }
```

`unsafe` is visible in the signature, requires a `SAFETY:` justification comment (enforced),
and is greppable/queryable (`marv/unsafeSites`) so audits can focus exactly there.

---

## 12. Worked example

```marv
mod report
import std.io (Fs)
import std.collections (List)

struct Sale { region: str, amount: i64 }

error LoadError { NotFound, BadFormat }

/// Pure aggregation: provably no I/O, no allocation beyond the given allocator.
pure fn total(sales: &[]Sale) -> i64
    ensures result >= 0    // (assuming non-negative amounts; checked at runtime)
{
    var sum: i64 = 0
    var i: usize = 0
    while i < len(sales)
        invariant i <= len(sales)
    {
        sum = sum + sales[i].amount
        i = i + 1
    }
    sum
}

// Effect row in the signature proves: touches the filesystem, nothing else.
fn load_and_total(fs: Fs, path: str) -> !i64 {
    let bytes = fs.read(path)?         // FileError flows into the inferred error set
    let sales = parse_sales(bytes)?    // ParseError flows in too
    total(&sales)
}
```

An auditor reading only the two signatures knows: `total` is pure and returns a non-negative
value; `load_and_total` can read files and nothing else, and its failures are exactly the
union of file and parse errors. No function body required.
