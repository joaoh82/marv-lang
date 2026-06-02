# marv

**marv** (short for *Marvin*) is a general-purpose, compiled programming language and
toolchain designed around an unusual premise: **the author is a coding agent, and the
auditor is a human.** Almost every design decision falls out of taking that premise
seriously.

> Humans optimize languages for limited working memory, impatience with verbosity, and
> taste. Agents have the opposite cost structure: verbosity is nearly free, but
> *implicitness is expensive* because it burns context. So marv is relentlessly explicit,
> optimizes for **local reasoning** (a function's meaning is recoverable from its
> signature + body alone), and is built as a **machine-queryable service** with precise,
> structured, fix-carrying diagnostics — while remaining auditable by a human who reads
> only types, effects, capabilities, and contracts.

Semantic inspirations: **Zig** (no hidden control flow / no hidden allocation, inferred
error sets, explicit allocators), **Rust** (sum types, move semantics, `Result`),
**Hylo/Val** (mutable value semantics + second-class references → memory safety without GC
*and* without lifetime annotations), **Austral** (no global inference, linear types,
capability-based security), **Unison** (content-addressed code), and **Dafny/SPARK**
(first-class contracts).

---

## The three specs

| File | Contents |
|------|----------|
| [`01-design-spec.md`](01-design-spec.md) | Language design: philosophy, types, memory model, effects & capabilities, error sets, contracts, modules/content-addressing, concurrency, worked examples. |
| [`02-grammar-and-core-ir.md`](02-grammar-and-core-ir.md) | Lexical grammar, full EBNF surface grammar, the canonical **Core IR** (ANF + de Bruijn), desugaring rules, typing/effect judgments, and the content-hashing scheme. |
| [`03-compiler-protocol.md`](03-compiler-protocol.md) | The agent-facing JSON-RPC protocol: query catalog, structured diagnostics with machine-actionable fixes, and concrete request/response examples for the generate→check→repair loop. |

Read them in order. `01` is the *why* and *what*, `02` is the *exact form*, `03` is the
*interface an agent drives*.

---

## Host language: Rust (Stage 0), then self-host (Stage 1)

marv's toolchain is written in **Rust**. This is a project-specific choice, not a default:

- **`salsa`** gives you the incremental, demand-driven query engine that the agent protocol
  in `03` *is* — the same architecture rust-analyzer uses.
- One ecosystem covers every backend you need: **Cranelift** (fast dev/native),
  **LLVM via `inkwell`** (optimized release), and the richest **WASM** tooling anywhere
  (`wasm-encoder`, `walrus`, `wasmtime`, `wasm-tools`, component model) for browser + server.
- Verifier + infra are partly off-the-shelf: **`z3`** / **`easy-smt`** for contract
  discharge, **`blake3`** for content-addressing, **`serde`** + JSON-RPC for the protocol.

**Bootstrap trajectory** (mirrors Rust: OCaml→self, and Zig: C++→self):

1. **Stage 0** — full marv compiler + toolchain in Rust.
2. **Stage 1** — once marv compiles itself, rewrite the compiler in marv and self-host.
   Keep the Rust Stage-0 compiler permanently as a *differential-testing oracle*.

Zig stays a *semantic muse* for marv; it is not the toolchain host (pre-1.0 churn, thinner
meta-ecosystem, no salsa equivalent).

### Recommended crate layout

```
marv/
  crates/
    marv-syntax/        # lexer, recursive-descent parser, AST, canonical formatter
    marv-core/          # Core IR, ANF lowering, de Bruijn, blake3 content hashing
    marv-types/         # type + effect + capability checker; error-set inference; contract frontend
    marv-db/            # salsa query database (incremental engine; the protocol's backbone)
    marv-verify/        # SMT contract discharge (z3 / easy-smt) for the verified subset
    marv-codegen-cl/    # Cranelift backend (dev + native)
    marv-codegen-llvm/  # LLVM backend (release native) via inkwell
    marv-codegen-wasm/  # WASM + component-model backend (browser & server)
    marv-interp/        # tree-walking interpreter (semantics oracle, used before codegen lands)
    marv-store/         # content-addressed code store + lockfile resolution
    marv-server/        # JSON-RPC agent-protocol server (wraps marv-db queries)
    marv-cli/           # `marv` command-line front-end
  std/                  # marv standard library, written in marv
  spec/                 # these three documents
  tests/                # golden tests, round-trip property tests, differential tests
```

### Build order (give Claude Code these as milestones)

- **M0 — Front end.** Lexer + parser + AST + canonical formatter. Acceptance gate:
  the round-trip property `parse ∘ format == id` holds on all canonical forms (proptest).
- **M1 — Core IR.** ANF lowering, de Bruijn conversion, blake3 hashing. Gate: alpha-
  equivalent surface programs lower to identical Core hashes (golden tests).
- **M2 — Checker.** Type + effect + capability checking; error-set inference; second-class
  reference & linearity checks; diagnostics emit machine-actionable fixes.
- **M3 — Query server.** Wire `marv-db` (salsa) + `marv-server` (JSON-RPC). Expose
  `check`, `typeAt`, `errorSet`, `effects`, `canonical`, `core`, `hash`.
- **M4 — Run it.** `marv-interp` first (as oracle), then Cranelift native codegen.
- **M5 — Web.** WASM/component backend + a browser demo proving capability-gated sandboxing.
- **M6 — Verify.** Runtime contract checks everywhere; then SMT discharge for the verified
  subset (`marv-verify`).
- **M7 — Reuse & self-host.** Content-addressed store + lockfile; begin Stage-1 self-hosting.

### Non-negotiable invariants (enforce in CI)

1. **One canonical form.** The formatter is the parser's inverse; there is exactly one way
   to write any program. No style options.
2. **No hidden control flow / no hidden allocation.** Every effect is visible at the call
   site; allocation only via an explicit allocator capability.
3. **No ambient authority.** No global I/O, clock, randomness, or network. Power enters only
   through capability parameters.
4. **Local reasoning.** No cross-function type inference; every signature is fully annotated.
5. **Determinism.** Compilation and builds are bit-for-bit reproducible.
