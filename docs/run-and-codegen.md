# Running marv: interpreter + Cranelift backend (M4)

Milestone M4 makes marv programs *execute*, two ways, both over the same
canonical Core IR (`spec/02` §C):

- **`marv-interp`** — a tree-walking interpreter, the **semantics oracle**. It
  is the reference meaning of a program and is kept permanently as the thing the
  native backends are tested against.
- **`marv-codegen-cl`** — a **Cranelift** backend that JIT-compiles Core to
  native code (`spec/01` §9 — "same Core IR feeds both").

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
  multi-variable loops work without aggregate layout.

Every scalar lives in a 64-bit register in *both* backends, so their wrapping
arithmetic matches — the property that makes the differential test meaningful.
Constructs with no surface form yet (aggregate runtime layout, `perform`,
first-class closures, floats) are interpreted where the interpreter can, and the
Cranelift backend returns an honest `unsupported` rather than emitting wrong
code. New constructs land in *both* backends together so agreement is preserved.

## Capabilities are injected, never ambient

`marv run --grant CAP,…` is the sandbox (`spec/03` §4.5). The entry point's
capability parameters are filled *only* from the host's grant set; the
interpreter records every `perform` as an effect, and refuses to materialize an
ungranted capability. This is the runtime mirror of the static guarantee: the
checker already rejects a function that performs a capability outside its
declared effect row *before* it can run, so the runtime grant check is
defense-in-depth, not the primary line of defense.

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

The negative case is `uses_ungranted_cap.core.json`: a Core-IR snapshot whose
`leak(fs: Fs, path: str)` body `perform`s `Fs` while declaring the empty
(`pure`) effect row. The real M2 checker reports `E0110` (missing capability),
so `marv build` refuses it — it can never reach codegen. Run it yourself:

```sh
marv build tests/run/uses_ungranted_cap.core.json   # E0110, exits non-zero
```

## Trying it

```sh
marv run   examples/factorial.mv --entry factorial 6        # 720 (interpreter)
marv build --run examples/factorial.mv --entry factorial 6  # 720 (Cranelift JIT)
marv run   examples/arithmetic.mv                            # 42  (entry defaults to main)
```

## The WebAssembly backend (M5)

`marv-codegen-wasm` is the third backend, emitting a WebAssembly module with
`wasm-encoder`. It compiles the same integer/boolean subset as Cranelift, plus
`Core::Perform`, and every scalar is an `i64` — so it stays in lockstep with the
oracle. `marv build --target wasm-component <file> -o out.wasm` writes the module
and prints its capability manifest.

### Capabilities are host imports

This is the web sandbox `spec/01` §9 is built on. A `perform` of a capability
lowers to a **call to an imported function** — one import per
`(capability, operation)`, named `(CapName, "op<n>")`. Consequences:

- A **pure** module performs nothing, so it **imports nothing**. There is no slot
  through which any host could hand it authority — it can only compute.
- A module that wants the network imports `Net::op0`. The host decides whether to
  satisfy that import. Withhold it and the module **cannot be instantiated**, let
  alone open a socket. The import list is the capability manifest, statically
  inspectable (`WebAssembly.Module.imports` / the `marv build` output).
- A capability parameter carries **no ABI slot** — authority is the import, not a
  value threaded through the call. `demo.fetch(net)` exports as a zero-argument
  wasm function; the `Net` it needs shows up as an *import*, not a parameter.

### The differential gate and the browser demo

`crates/marv-codegen-wasm/tests/differential.rs` runs the same `tests/run/*.mv`
corpus through **wasmtime** and asserts it matches the interpreter, and checks
that a pure module imports nothing while a `Net`-performing module imports exactly
`Net`.

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
  backend over the integer/boolean subset; `marv run`, `marv build --target
  native-cranelift`, `marv build --target wasm-component`; the three-way
  differential gate (interpreter ↔ Cranelift ↔ wasm) and a browser sandbox demo.
- **Next:** aggregates and enum `match` in the native/wasm backends;
  ahead-of-time object/executable emission and an LLVM backend for release builds;
  string/aggregate-typed capability operands (needs linear memory) and full
  component-model / WIT packaging. The interpreter remains the oracle each backend
  is differentially tested against.
