# Running marv: interpreter + Cranelift backend (M4)

Milestone M4 makes marv programs *execute*, two ways, both over the same
canonical Core IR (`spec/02` §C):

- **`marv-interp`** — a tree-walking interpreter, the **semantics oracle**. It
  is the reference meaning of a program and is kept permanently as the thing the
  native backends are tested against.
- **`marv-codegen-cl`** — a **Cranelift** backend that JIT-compiles Core to
  native code and can also emit AOT object/executable artifacts (`spec/01` §9
  — "same Core IR feeds both").

The acceptance gate for M4 is that the two **agree** on a corpus of programs,
plus one program that *fails to compile* because it uses a capability absent
from its effect row.

## The execution model

Both backends consume the same shape the front end produces: a set of
content-addressed definitions keyed by **symbol hash** (`marv_core::symbol_hash`,
the key a body's `Atom::Global` carries) plus the declaration `World`. There is
no separate "linker" step — calls resolve by that hash.

Core is **curried** and in **ANF**: `f(a, b)` lowers to
`let t = App(Global f, a); App(t, b)`. Each backend resolves the application
spine the same way — accumulate arguments until the function is *saturated*
(one per curried lambda), then perform the call. The interpreter models a
not-yet-saturated function as a `Value::Partial`; the backend models it as a
compile-time `Slot::Partial` that lowers to a single direct native `call`. The
surface never produces a partially-applied function *value*, so neither backend
needs a heap closure.

### What executes today

The integer/boolean core the M0/M1 front end can express and lower:

- arithmetic (`+ - * / %`), comparisons (`== != < <= > >=`), boolean `and`/`or`;
- `if`/`else` (a two-arm `bool` `Match`, `spec/02` §D) and `let` bindings;
- curried calls between top-level functions, including recursion;
- `while` loops over scalar loop-carried state (`Core::Loop`): a header/body/exit
  block with the carried `var`s as SSA block parameters in Cranelift, and as
  mutable locals under a `block { loop { … } }` in WASM. The carried-state tuple
  the body/loop produce is a compile-time register/local bundle (no heap), so
  multi-variable loops work without aggregate layout;
- **aggregates and enums (MARV-9):** `struct`/tuple products and `enum` tagged
  unions — `Ctor`, field `Proj`, and an n-way `Match` that binds a variant's
  fields (`binds > 0`). See the representation below.
- **arrays (MARV-30):** array literals `[e0, …]` (`Core::Array`), `len(a)`, the
  index read `a[i]`, and the index store `a[i] = e`. An array boxes to a
  `[len, e0, …]` block — the length sits in the header word (where a `struct`/
  `enum` keeps its tag), so `len` is one header load and `index` loads `[i + 1]`.
  An element store is a functional update under mutable value semantics
  (`spec/01` §4): with the array's length statically known it rebuilds the array,
  taking the new value at the written position and the old element elsewhere.
- **runtime-length slices (MARV-33):** a slice `[]T` shares the array's
  `[len, e0, …]` layout — only its length is a runtime value — so `len`/index
  reads fall straight out of the array codegen, and a fixed-length array
  **coerces** to a slice (`[N]T` → `[]T`, also through a `&` reference). The
  element store `s[i] = e` cannot use the array's static unroll, so it lowers to
  `Core::IndexSet`: the backends read the count from the header, allocate a fresh
  `[len, …]` block, copy it with a **runtime loop**, and overwrite element `i`
  (the source block is left untouched). `tests/run/slices.mv` asserts
  interp == Cranelift == wasm.
- **strings (MARV-43):** string literals, `+` concatenation, `len(s)`, `s[i]`,
  `s[a..b]`, `str == str` / `str != str` content equality, `for c in s`, and
  `std.str.from_chars(alloc, chars)` run on the
  interpreter, Cranelift, and WASM. Native backends store strings as one-word
  pointers to `[len, codepoint0, …]` blocks, so indexing and iteration yield
  `char` code points and dynamic string creation allocates a fresh block.
  `tests/run/strings.mv` asserts interp == Cranelift == wasm.
- **Map/Set first slice (MARV-50):** `std.collections.Map[K, V]` and `Set[T]`
  are currently runnable for string keys/elements through list-backed,
  insertion-ordered std functions (`map_*`, `set_*`) with explicit `Alloc` for
  growth/rebuilds. `tests/run/map_set.mv` asserts interpreter == Cranelift ==
  WASM, including dynamically-built string keys.
- **monomorphized generics (MARV-26):** monomorphization is a lowering-time pass
  (`spec/01` §§3.3–3.4), so a generic call has already specialized to a concrete
  def (`max@i64`) with its interface methods dispatched to the coherent `impl`
  before codegen runs — no backend-specific generics work is needed. Both backends
  **skip** the generic *templates* themselves (defs whose signature still mentions
  a `Type::Var`, via `Type::is_polymorphic`): a template has no concrete ABI and is
  never called directly, and its body references unresolved interface methods. The
  interpreter skips them implicitly via lazy, by-need evaluation.

Every scalar lives in a 64-bit register in *both* backends, so their wrapping
arithmetic matches — the property that makes the differential test meaningful.
Constructs the Cranelift backend cannot lower (`perform` — now expressible from
source, MARV-6, and lowered by the WASM backend — first-class closures, floats)
are interpreted where the interpreter can, and Cranelift returns an honest
`unsupported` rather than emitting wrong code. Native AOT uses that same lowering
path, so an unsupported reachable construct fails before any object or linked
executable is written. New constructs land in *both* backends together so
agreement is preserved.

### Cranelift AOT objects and executables (MARV-68)

`marv build --emit object` emits a relocatable native object for the entry's
reachable closure. Function symbols are derived from content hashes, the module
name is fixed, and repeated builds of the same checked source produce identical
object bytes on the same host target. The object imports the small runtime ABI:
`marv_rt_alloc`, `marv_rt_heap_mark`, `marv_rt_heap_reset`, and, in debug builds,
`marv_rt_bounds_fail`.

`marv build --out app` (or `--emit exe --out app`) links that object with a
generated C runtime wrapper. The wrapper supplies the allocation/arena hooks,
parses up to four integer entry arguments, calls the selected entry, resets the
runtime heap, and prints the integer result. This is intentionally still a
backend-supported pure/value entrypoint story: capability-hosted programs should
use `marv run --grant ...` or the WASM host-import model until the production
native host runtime grows a capability ABI.

### Reachability-pruned builds (MARV-8)

`marv build` compiles **only the definitions reachable from the entry point**:
`marv_core::reach::reachable_mask` resolves the entry (explicit `--entry`, else
`main`, else the sole function) and walks its transitive dependency closure —
the same `Global`/`Nominal` edges the content store links into the Merkle DAG
(`marv-store::resolve`). Both backends (`compile_reachable` in
`marv-codegen-cl` and `marv-codegen-wasm`) declare and compile only that
closure, and the wasm artifact exports only it. So a module that mixes
supported functions with not-yet-supported ones builds as long as the entry
never references the unsupported ones — `examples/geometry.mv`'s `max` builds
and runs even though its sibling `translate` does not lower yet.

When no entry resolves (no `main` among several functions, or a `--entry` name
that matches nothing), the whole module is compiled, preserving the usual
`NoSuchEntry`/unsupported errors. Whole-module compilation (`compile` /
`compile_with`) remains the API for audit flows and the differential corpus —
pruning never changes what `commit` freezes or what the checker checks (every
definition, always).

### Aggregate & enum representation (MARV-9)

Every marv value is one machine word. A scalar *is* that word; an aggregate is a
**pointer** to `(1 + arity)` contiguous `i64` words laid out as
`[tag, field_0, …, field_{n-1}]` (`spec/02` §C). Products (`struct`/tuple) use
tag 0; an `enum` variant uses its tag. The layout is **identical across all
three backends** — the interpreter's tagged `Value::Agg`, Cranelift's heap
block, and the WASM linear-memory block — so "interp == Cranelift == wasm" stays
a checkable statement.

- **Cranelift** boxes via a host `marv_rt_alloc` symbol backed by a runtime arena;
  `Proj` is a load, and an enum `Match` loads the tag from word 0 and dispatches
  through a `br_table`, binding each arm's fields by loading them from the
  payload.
- **WASM** boxes into a growable linear-memory arena (one memory + a mutable heap
  pointer global, both module-internal so a *pure* module's import manifest is
  unchanged); the enum `Match` is the same tag-load + dispatch over the payload.
- Boxing is **lazy**: a `Ctor` is a compile-time register/local bundle and is
  only spilled to the heap when it must cross a function boundary, be returned, or
  be matched as a runtime value — so loops (whose carried state never escapes)
  allocate nothing. The backend tells a scalar `bool` `Match` (the `if`/`else`
  desugaring) from a boxed `enum` one by the scrutinee's *type*
  (`marv_types::layout`), the one fact the type-erased Core does not carry.

Both compiled backends now reclaim compiler-managed boxes whose lifetime is
bounded by a scalar-carried loop iteration: they mark the heap before the loop
and reset it on each backedge/exit, so a loop that repeatedly builds and consumes
a struct runs in bounded memory. This is an arena strategy, not a general
ownership/RC system: boxes that escape through aggregate-carried loop state or
long-lived values remain live until the surrounding run ends.

An **array** (MARV-30) reuses this boxed shape with one twist: the header word
holds the element **count** instead of a tag, so the block is `[len, e0, …]`.
That single convention serves both queries — `len(a)` is the header load and
`a[i]` loads word `i + 1` — and keeps arrays on the same lazy-boxing path as the
other aggregates (an array that never escapes stays a register/local bundle).
Arrays are *structural* (`Core::Array` carries the element type, not a nominal
hash) and are measured/indexed rather than projected.

A **slice** `[]T` (MARV-33) is the *same* `[len, e0, …]` block — the only
difference is that its length is a runtime value, not a compile-time one, so
`len`/index need no new machinery. What the array path cannot reuse is the
element store: with an unknown length the static unroll is impossible, so a slice
store lowers to `Core::IndexSet` and the backends emit an **allocate-copy-store**
— read the count from the header, allocate a fresh `len + 1`-word block, copy it
with a runtime loop, then overwrite the one element. The result is a new block (a
functional update; the source is untouched, `spec/01` §4). A fixed-length array
coerces to a slice at no runtime cost (the layout is identical).

A **list** `List[T]` (MARV-42) is the growable cousin with layout
`[len, cap, e0, …]`. `new(alloc)` / `with_capacity(alloc, n)` and
`push(alloc, list, value)` require an explicit `Alloc` capability in source; the
compiled backends allocate through their existing arenas, so no hidden ambient
allocator appears. `push`, `set`, and `pop` return the updated list value; compiled
backends update the block in place when capacity allows and allocate-copy only on
growth. `len(list)` reads word 0, and
`list[i]`/`get(list, i)` bounds-check against `len` then load from word `i + 2`.
The existing `for x in collection` desugar works unchanged because it is already
defined in terms of `len` and index.

The first `Iter[T]` protocol slice (MARV-52) keeps those direct indexed paths for
arrays, slices, strings, and `List[T]`, but lets `std.iter.IndexIter[T]` opt into
protocol lowering. A `for x in it` where `it: IndexIter[i64]` lowers through the
generic `std.iter.iter_len` / `std.iter.iter_get` wrappers; their specialized
instances dispatch via the `Iter[i64]` impl and then use the same backend-safe
list operations. No executor, allocation, or ambient authority is introduced by
iteration itself.

Collection literals (MARV-51) introduce no new backend primitive. `List { alloc:
alloc, items: [e0, e1, …] }` lowers to `Core::ListNew` with capacity set to the
item count, then one `Core::ListPush` per item. `Set { alloc: alloc, items:
[...] }` lowers through `std.collections.set_with_capacity` and `set_insert`, so
duplicates follow ordinary set semantics. `Map { alloc: alloc, keys: [...],
values: [...] }` lowers to the current list-backed map entry storage. The
explicit `Alloc` field is required, so these literals remain visible allocation
sites.

### The Tier-1 bounds check (MARV-34)

A runtime subscript outside `0..len` — an element read `a[i]`/`s[i]` or a slice
element store `s[i] = e` — is a **Tier-1 contract violation** (`spec/01` §7) in
debug builds, not a trap or an adjacent-memory access. The check is one unsigned
comparison against the header word (`i (u64) < len` covers both ends, since a
negative `i64` is a huge `u64`):

- the **interpreter** aborts the run with a structured
  `RunError::BoundsCheckFailed { index, len }` report, exactly like a violated
  `requires`/`invariant`;
- **Cranelift** branches to a host `marv_rt_bounds_fail(index, len)` hook that
  prints the same report to stderr and aborts the process (an abort hook rather
  than a bare trap, so the report can carry the offending values);
- **WASM** emits an `unreachable` trap, which the embedding surfaces as a failed
  call. The trap carries no message by design: an abort *hook* would be a host
  import, and a pure module must keep importing nothing (the sandbox manifest).

`marv build --release` omits the check from both codegen backends; release-mode
in-bounds codegen is byte-identical to the pre-check output. The interpreter is
the debug runner and always checks (as it does for contracts). One honest gap:
a **fixed-length array** store `a[i] = e` with a runtime `i` is unrolled at
lowering time into per-element selects, so an out-of-range `i` there silently
leaves the array unchanged on all three backends — memory-safe by construction,
but a no-op rather than an abort. Guarding it means changing the lowering (and
every in-bounds program's Core hash), so it stays a follow-up.

## Capabilities are injected, never ambient

`marv run --grant CAP,…` is the sandbox (`spec/03` §4.5). The entry point's
capability parameters are filled *only* from the host's grant set; the
interpreter records every `perform` as an effect, and refuses to materialize an
ungranted capability. This is the runtime mirror of the static guarantee: the
checker already rejects a function that performs a capability outside its
declared effect row *before* it can run, so the runtime grant check is
defense-in-depth, not the primary line of defense.

`Spawn` follows the same path in the interpreter. `std.spawn.spawn_i64` performs
`Spawn.start` and returns a `linear TaskI64`; `join_i64` consumes it. Running
`examples/spawn.mv` with `--grant Spawn` returns `42` and records two
`Spawn` effects. Without the grant, the entry is refused at the boundary; if a
task handle is not joined, `marv check` reports the linearity error before run.

## The differential test (the M4 gate)

`crates/marv-codegen-cl/tests/differential.rs` loads each program in
[`../tests/run/`](../tests/run), runs it through **both** backends, and asserts
the results are equal to each other and to a hand-computed golden value:

| Program | Exercises |
|---------|-----------|
| `arithmetic.mv` | nullary entry, curried cross-function calls |
| `factorial.mv`  | recursion + a single `if` |
| `fib.mv`        | recursion with two self-calls |
| `gcd.mv`        | tail recursion through `%` |
| `clamp.mv`      | nested `if`/`else if`/`else` |
| `classify.mv`   | boolean `and`, comparisons |
| `ops.mv`        | every arithmetic prim + comparisons in one body |
| `loops.mv`      | `while` loops + `invariant` (`sum_to`, `pow`, `count_down`) — `Core::Loop` |
| `casts.mv` / `unary.mv` | `as` width truncation/wrapping; prefix `-`/`not` |
| `structs.mv`    | `struct` `Ctor`/`Proj`; a struct returned from and passed to a function (boxed across the boundary) — MARV-9 |
| `color.mv`      | n-way `enum` `Match` (jump table on tag) over a boxed enum built behind a call and through `if`/`else` — MARV-9 |
| `shapes.mv`     | payload-carrying variants + `Match` arms that bind fields (`binds > 0`) — MARV-9 |
| `generics.mv`   | a monomorphized generic (`max[T: Ord]` matching on `Ordering`, specialized to `i64` and dispatched to `impl Ord[i64]`) — runnable on all three backends since the enum got a layout (MARV-9); closes the gap noted in MARV-5 — MARV-26 |
| `arrays.mv`     | array literals + `len` + index read `a[i]` + index store `a[i] = e` (functional element update); a `len`-bounded `while` loop over an array — MARV-30 |
| `slices.mv`     | runtime-length slices `[]T`: construct (array→slice), `len`/index, a `Core::IndexSet` element store over a runtime length, and `total` over a slice of structs (`sales[i].amount`) — MARV-33; `for x in s` over a slice and over a slice of structs, nested `for`s (depth-keyed index names), and sequential `for`s — MARV-20 |
| `iter.mv`       | `std.iter.IndexIter[i64]` over a `List[i64]`; `for x in it` lowers through the `Iter[i64]` protocol wrappers instead of direct `len`/index — MARV-52 |
| `json.mv`       | `std.json` first slice: scalar serialization with explicit `Alloc` runs three-way; parser/typed-error paths are interpreter-smoked — MARV-55 |
| `json_dom.mv`   | `std.json` recursive/materialized DOM: backend-safe nested construction + deterministic serialization run three-way; recursive parse/error paths are interpreter/check covered until raise lowering reaches WASM — MARV-66 |

Both differential harnesses also carry an **out-of-bounds corpus** (MARV-34):
slice reads at `len` and at `-1`, a slice store at `len`, and an array read at
`len` must *abort* on every backend in debug mode — the interpreter with the
structured `BoundsCheckFailed` report, Cranelift by aborting a child process
with the report on stderr (the abort kills the process, so the harness re-spawns
itself per case), and wasm with an `unreachable` trap under wasmtime. A
release-mode case pins that `bounds_checks: false` leaves in-bounds results
unchanged.

The negative case is `uses_ungranted_cap.core.json`: a Core-IR snapshot whose
`leak(fs: Fs, path: str)` body `perform`s `Fs` while declaring the empty
(`pure`) effect row. The real M2 checker reports `E0110` (missing capability),
so `marv build` refuses it — it can never reach codegen. The same diagnostic now
fires *from source* — a `pure fn` that calls a capability method (e.g.
`fs.read(path)`) is rejected before codegen (MARV-6). Run it yourself:

```sh
marv build tests/run/uses_ungranted_cap.core.json   # E0110, exits non-zero
```

## Trying it

```sh
marv run   examples/factorial.mv --entry factorial 6        # 720 (interpreter)
marv build --run examples/factorial.mv --entry factorial 6  # 720 (Cranelift JIT)
marv run   examples/arithmetic.mv                            # 42  (entry defaults to main)
marv run --grant Spawn examples/spawn.mv                     # 42 + two Spawn effects
```

## The WebAssembly backend (M5)

`marv-codegen-wasm` is the third backend, emitting a WebAssembly module with
`wasm-encoder`. It compiles the same subset as Cranelift — including aggregates
and enums over a linear-memory heap (MARV-9) and arrays with `len`/index/store
(MARV-30) — plus `Core::Perform`, and every scalar is an `i64`, so it stays in
lockstep with the oracle. `marv build --target wasm-component <file> -o out.wasm`
writes the module and prints its capability manifest.

### Capabilities are host imports

This is the web sandbox `spec/01` §9 is built on. A `perform` of a capability
lowers to a **call to an imported function** — one import per
`(capability, operation)`, named `(CapName, "op<n>")`. Consequences:

- A **pure** module performs nothing, so it **imports nothing**. There is no slot
  through which any host could hand it authority — it can only compute.
- A module that wants the network or request/response authority imports `Net::opN`
  or `Http::opN`. The host decides whether to satisfy that import. Withhold it
  and the module **cannot be instantiated**, let alone open a socket or read a
  request. The import list is the capability manifest, statically inspectable
  (`WebAssembly.Module.imports` / the `marv build` output).
- A capability parameter carries **no ABI slot** — authority is the import, not a
  value threaded through the call. `demo.fetch(net)` exports as a zero-argument
  wasm function; the `Net` it needs shows up as an *import*, not a parameter.
- String operands to an import are passed as the normal `str` ABI word: a pointer
  to the module's `[len, codepoint…]` linear-memory block.
- String results from an import use the same one-word handle shape. The core-WASM
  backend can model this today for `Http.method/path/body_text`. Listener operations that
  return linear resource capabilities, such as `Net.listen`, still report honest
  `unsupported`; component/WIT
  packaging remains the place where those handles become named host-level string
  types.

### The differential gate and the browser demo

`crates/marv-codegen-wasm/tests/differential.rs` runs the same `tests/run/*.mv`
corpus through **wasmtime** and asserts it matches the interpreter, and checks
that a pure module imports nothing, a `Net`-performing module imports exactly
`Net`, and string-returning `Http` operations validate as host imports.

[`../web/`](../web) is a dependency-free browser demo (serve it with any static
server) proving the sandbox live:

- `factorial.wasm` (pure) — manifest shows *imports: none*; runs with zero authority.
- `fetcher.wasm` (imports `Net`) — with the grant unchecked the page supplies no
  `Net` import and instantiation **fails** (the module cannot reach the network);
  with it checked the page supplies `Net` and `fetch()` runs through it.

```sh
marv build --target wasm-component examples/factorial.mv -o web/factorial.wasm
marv build --target wasm-component web/fetcher.core.json -o web/fetcher.wasm
cd web && python3 -m http.server 8087   # then open http://localhost:8087/
```

## Status and what's next

- **Done:** interpreter over the full Core IR (capability injection, effect
  logging, currying, recursion, `match`); a Cranelift JIT and a WebAssembly
  backend over the integer/boolean subset **plus heap-boxed aggregates and enums
  with field-binding `match`** (MARV-9) **plus fixed-length arrays with
  `len`/index/store** (MARV-30) **plus runtime-length slices `[]T` with
  `len`/index and an allocate-copy-store element store** (MARV-33) **plus the
  Tier-1 debug bounds check on runtime element reads/stores, with
  `marv build --release` to omit it** (MARV-34); `marv run`, `marv build --target
  native-cranelift`, `marv build --target wasm-component`; the three-way
  differential gate (interpreter ↔ Cranelift ↔ wasm) and a browser sandbox demo.
- **Next:** ahead-of-time object/executable emission and an LLVM backend for
  release builds (MARV-10); broader ownership-aware reclamation for heap values
  that escape arena reset scopes; string/aggregate-typed capability operands and
  full component-model / WIT packaging. The interpreter remains the oracle each
  backend is differentially tested against.
