# Platform support

marv has one Core IR and several backends. The **interpreter** is the reference semantics
oracle; the native (Cranelift/LLVM) and WebAssembly backends are differentially tested against it
on a shared corpus ([`tests/run/`](../tests/run)), so "the backends agree" is a checkable
property (`spec/01` §9).

## Backends

| Backend | Crate | Form | Status |
|---------|-------|------|--------|
| Tree-walking interpreter | `marv-interp` | in-process; the oracle | **working** — full Core IR (arithmetic, `if`/`match`, recursion, currying, aggregates, `perform`/effects, contracts/Tier-1) plus a deterministic `Http` request/response test host |
| Cranelift (native) | `marv-codegen-cl` | **JIT** (in-process), AOT `.o`, linked executable | **working** for the integer/boolean subset + heap-boxed aggregates/enums (MARV-9) + `List[T]` growable storage (MARV-42) + string manipulation (MARV-43) + arena reclamation for scalar-carried loop temporaries; AOT object/executable builds are working for backend-supported reachable closures (MARV-68) |
| WebAssembly | `marv-codegen-wasm` | component `.wasm` + `.wit`, or core `.wasm` substrate | **working** for the integer/boolean subset + growable linear-memory aggregates/enums (MARV-9) + `List[T]` growable storage (MARV-42) + string manipulation (MARV-43) + arena reclamation for scalar-carried loop temporaries + capabilities-as-host-imports; `wasm-component` now wraps the core module in a validating component with deterministic WIT (MARV-70) |
| LLVM (release) | `marv-codegen-llvm` | textual LLVM IR compiled/linked by `clang` | **working release slice** for scalar arithmetic/casts, calls/recursion, `if`/`match`, `while`, early `return`, boxed structs/enums, arrays, runtime-length slice updates, `List[T]`, strings, iterator loops, bytes/UTF-8, JSON serializer-safe paths, and current map/set/app corpus paths; `raise`, capability `perform`, unsafe/resource host integration remain honest `unsupported` |

The interpreter executes the whole Core IR. The Cranelift and WASM backends today compile the
integer/boolean subset the front end can lower (arithmetic, comparisons, `and`/`or`, `if`/`else`,
`let`, curried cross-function calls and recursion), **plus aggregates and enums (MARV-9)**: a
`struct`/tuple product or `enum` variant is a pointer to a `[tag, fields…]` block — Cranelift on a
host arena, WASM in a growable linear-memory arena — so `Ctor`/`Proj`/n-way `Match` (binding fields)
run identically to the interpreter's tagged `Value`. Strings use a related `[len, codepoint…]`
block for concat, slicing, indexing, iteration, and `List[char] -> str` building. Scalar-carried loops mark/reset those arenas,
which reclaims compiler-managed boxes created and consumed within one iteration. Both compute
scalars at 64-bit width so they match the oracle exactly. Constructs they don't lower yet
(first-class closures, floats) return an honest `unsupported` rather than emitting wrong code — and
land in *both* backends together so agreement is preserved. A definition the entry never reaches
doesn't block a build: `marv build` compiles only the entry's transitive closure (MARV-8), so a
module can mix supported and not-yet-supported functions. Native AOT uses the same reachable
closure and lowering rules, then writes either a deterministic object (`--emit object`) or links
that object with a small runtime wrapper (`--out app`). The current executable wrapper is for
pure/value entries with up to four integer arguments; reachable capability `perform` code still
fails clearly before artifact emission. The LLVM release slice also uses reachability pruning,
emits deterministic textual IR for the supported closure, and asks `clang -O2` to produce the
optimized executable. The WASM backend additionally lowers `perform` to a host-import call.

## Capabilities across hosts

- **Interpreter** (`marv run --grant CAP,…`): the host's grant set is injected at the entry
  point; each `perform` is recorded as an effect; an ungranted capability is refused at the
  boundary (defense-in-depth behind the static effect-row guarantee). For `Http`, the
  interpreter includes a deterministic test host (`POST /echo`, body `marv-http-echo`).
  `Net.listen`/`Listener.accept_http` can drive one listener-accepted HTTP exchange against
  that same deterministic host; real OS socket scheduling remains host runtime work.
- **WebAssembly**: each capability operation is a module/component **import**. A pure
  artifact imports nothing (no slot through which authority could be handed to it); a module
  that wants the network or HTTP request access imports `Net`/`Http` operations and **cannot
  be instantiated** unless the host supplies them. `marv build --target wasm-component`
  wraps the core module in a validating component and writes WIT that exposes those imports
  as typed functions. `marv build --target wasm-core` emits the core module substrate
  directly for wasmtime/browser core-module embeddings. Listener operations that return
  linear resource capabilities, such as `Net.listen`, still report honest `unsupported`.
  Scalar, boolean, and string-handle values cross the current component/core ABI as
  one-word `s64` slots; richer component-model records/resources are staged follow-ups.
  The import list is the capability manifest, statically inspectable through WIT,
  component imports, `WebAssembly.Module.imports` for `wasm-core`, and the `marv build`
  output. See [`web/`](../web).

## Hosts & targets

| | Build/test host | Notes |
|---|---|---|
| macOS (arm64, x86_64) | ✅ supported | primary dev platform |
| Linux (x86_64) | ✅ supported | CI |
| WASM (`wasm32`, component) | ✅ produced by `marv build --target wasm-component` | validates as a component and ships WIT |
| WASM (`wasm32`, core module) | ✅ produced by `marv build --target wasm-core` | runs under wasmtime and in the browser |
| Windows | untested | should build (pure-Rust workspace); not in CI yet |

## Tooling prerequisites

- **Rust** pinned to `1.94.0` (`rust-toolchain.toml`).
- **z3** on `PATH` for the Tier-2 SMT verifier (`marv verify`). Optional: without it,
  verification reports `unsupported` and falls back to Tier-1 runtime checks, and the
  solver-dependent tests skip rather than fail.
- **wasmtime** is a dev-dependency (the WASM differential tests run modules under it); not
  needed at runtime for `marv` itself.
- **cc** on `PATH` is needed only when `marv build --out app` links a native executable.
  `marv build --emit object` can emit the relocatable object without a C linker.
- **clang** on `PATH` is needed for `marv build --target native-llvm --run` and
  `--target native-llvm --out app`; the crate does not require `llvm-config`.
