# Platform support

marv has one Core IR and several backends. The **interpreter** is the reference semantics
oracle; the native (Cranelift) and WebAssembly backends are differentially tested against it
on a shared corpus ([`tests/run/`](../tests/run)), so "the backends agree" is a checkable
property (`spec/01` §9).

## Backends

| Backend | Crate | Form | Status |
|---------|-------|------|--------|
| Tree-walking interpreter | `marv-interp` | in-process; the oracle | **working** — full Core IR (arithmetic, `if`/`match`, recursion, currying, aggregates, `perform`/effects, contracts/Tier-1) |
| Cranelift (native) | `marv-codegen-cl` | **JIT** (in-process) | **working** for the integer/boolean subset; AOT object/executable emission is roadmap |
| WebAssembly | `marv-codegen-wasm` | core `.wasm` module | **working** for the integer/boolean subset + capabilities-as-host-imports; component/WIT packaging + linear-memory aggregates are roadmap |
| LLVM (release) | `marv-codegen-llvm` | — | **stub** (roadmap — optimized release builds via `inkwell`) |

The interpreter executes the whole Core IR. The Cranelift and WASM backends today compile the
**integer/boolean subset** the front end can lower (arithmetic, comparisons, `and`/`or`,
`if`/`else`, `let`, curried cross-function calls and recursion); both compute scalars at
64-bit width so they match the oracle exactly. Constructs they don't lower yet (aggregates
with runtime layout, first-class closures, floats) return an honest `unsupported` rather than
emitting wrong code — and land in *both* backends together so agreement is preserved. The
WASM backend additionally lowers `perform` to a host-import call.

## Capabilities across hosts

- **Interpreter** (`marv run --grant CAP,…`): the host's grant set is injected at the entry
  point; each `perform` is recorded as an effect; an ungranted capability is refused at the
  boundary (defense-in-depth behind the static effect-row guarantee).
- **WebAssembly**: each capability operation is a module **import**. A pure module imports
  nothing (no slot through which authority could be handed to it); a module that wants the
  network imports `Net` and **cannot be instantiated** unless the host supplies it. The
  import list is the capability manifest, statically inspectable
  (`WebAssembly.Module.imports` / the `marv build` output). See [`web/`](../web).

## Hosts & targets

| | Build/test host | Notes |
|---|---|---|
| macOS (arm64, x86_64) | ✅ supported | primary dev platform |
| Linux (x86_64) | ✅ supported | CI |
| WASM (`wasm32`, core module) | ✅ produced by `marv build --target wasm-component` | runs under wasmtime and in the browser |
| Windows | untested | should build (pure-Rust workspace); not in CI yet |

## Tooling prerequisites

- **Rust** pinned to `1.94.0` (`rust-toolchain.toml`).
- **z3** on `PATH` for the Tier-2 SMT verifier (`marv verify`). Optional: without it,
  verification reports `unsupported` and falls back to Tier-1 runtime checks, and the
  solver-dependent tests skip rather than fail.
- **wasmtime** is a dev-dependency (the WASM differential tests run modules under it); not
  needed at runtime for `marv` itself.
