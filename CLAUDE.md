# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repository is

**marv** (short for *Marvin*) is a compiled programming language and toolchain, designed
around one premise: **the author is a coding agent, and the auditor is a human.** Every design
rule serves explicitness, local reasoning, and machine-verifiability.

The compiler is **implemented end to end in Rust** — Stage-0 milestones **M0–M7 are
complete**: front end, Core IR, checker, salsa query server + JSON-RPC, interpreter plus
Cranelift/WASM codegen, runtime contracts + SMT verification, and the content-addressed store.
Current work is **growing the language surface** (and beginning self-hosting) via `MARV-#`
tasks — see [`docs/roadmap.md`](docs/roadmap.md) for live status and ordering, and the
[`crates/`](crates/) tree for the code.

The three design documents under `spec/` remain the **source of truth for design** — read them
before changing semantics. But for *what is actually built today*, trust `docs/roadmap.md` and
the code, not any future-tense prose in the specs (or in this file).

| File | What it defines |
|------|------|
| [`spec/01-design-spec.md`](spec/01-design-spec.md) | The *why/what*: philosophy, types, memory model, effects & capabilities, error sets, contracts, content-addressing, concurrency. |
| [`spec/02-grammar-and-core-ir.md`](spec/02-grammar-and-core-ir.md) | The *exact form*: lexical + EBNF surface grammar, the canonical **Core IR** (ANF + de Bruijn), desugaring, typing/effect judgments, content-hashing scheme. |
| [`spec/03-compiler-protocol.md`](spec/03-compiler-protocol.md) | The *interface*: the agent-facing JSON-RPC protocol — query catalog, fix-carrying diagnostics, the generate→check→repair loop. |

Read them in order (`01` → `02` → `03`). `spec/README.md` summarizes all three.

## Host language and toolchain

Stage 0 of the compiler is written in **Rust**. This is a deliberate project choice (not a
default) because the agent protocol *is* a demand-driven incremental query engine, and the
Rust ecosystem supplies every piece: `salsa` (incremental queries, the protocol's backbone),
`cranelift` + `inkwell`/LLVM + the WASM component-model tooling (backends), `z3`/`easy-smt`
(contract discharge), `blake3` (content-addressing), `serde` + JSON-RPC (the protocol).

Bootstrap trajectory: **Stage 0** = full compiler in Rust; **Stage 1** = once marv compiles
itself, rewrite the compiler in marv and self-host, keeping the Rust Stage-0 compiler
permanently as a *differential-testing oracle*. marv source files use the `.mv` extension.

## Workspace layout

The crate split follows `spec/README.md` — each crate is one compiler phase, which keeps salsa
query boundaries clean:

```
crates/
  marv-syntax/      # lexer, recursive-descent parser, AST, canonical formatter
  marv-core/        # Core IR, ANF lowering, de Bruijn conversion, blake3 content hashing
  marv-types/       # type + effect + capability checker; error-set inference; contracts frontend
  marv-db/          # salsa query database (incremental engine) — the protocol's backbone
  marv-verify/      # SMT contract discharge (z3 / easy-smt) for the verified subset
  marv-codegen-cl/  # Cranelift backend (dev + native)
  marv-codegen-llvm/# LLVM backend (release native) via inkwell
  marv-codegen-wasm/# WASM + component-model backend (browser & server)
  marv-interp/      # tree-walking interpreter (semantics oracle / differential reference)
  marv-store/       # content-addressed code store + lockfile resolution
  marv-server/      # JSON-RPC agent-protocol server (wraps marv-db queries)
  marv-mcp/         # MCP stdio server forwarding tools to marv/* protocol methods
  marv-cli/         # `marv` command-line front-end
std/                # marv standard library, written in marv
selfhost/           # Stage-1 self-hosting: compiler passes ported to marv (in progress)
examples/           # illustrative .mv programs, kept in canonical form
tests/              # golden tests, round-trip property tests, differential tests
web/                # browser demo: capability-gated WASM sandbox + prebuilt artifacts
docs/               # human-facing toolchain documentation (usage + impl status)
```

## Keeping examples, tests, and docs current

`examples/`, `tests/`, and `docs/` are first-class and must not be allowed to
drift from the implementation. Treat updating them as **part of** any change that
affects observable behavior, not a follow-up:

- **`examples/`** — illustrative `.mv` programs that track the language as the
  specs describe it. Every file must be in canonical form; the
  `examples_are_canonical` test (`crates/marv-syntax/tests/golden.rs`) enforces
  this. When syntax/semantics change, update the affected examples (and run
  `marv fmt examples/*.mv`). Add an example when a milestone introduces a feature
  worth showing.
- **`tests/`** — repository-level golden/round-trip/differential fixtures. When a
  phase changes output (formatter normalization, Core-IR hashing, diagnostics),
  add or refresh the matching fixture in the same change. The runnable harness
  lives in the relevant crate's `tests/` and reads these fixtures by path.
- **`docs/`** — toolchain usage and per-milestone implementation status. When a
  milestone changes observable behavior (a new subcommand, a new protocol method,
  the formatter learning to reflow), update the matching doc in the same change.
  Keep status claims honest about what is and isn't implemented yet.

A change that alters behavior without touching the relevant example/test/doc is
incomplete.

## Build milestones (M0–M7 — complete)

These Stage-0 milestones are all **done**; the list is kept as the record of what each
delivered and the acceptance gate it had to clear. Ongoing work (surface expansion,
backend/verification breadth, self-hosting) is tracked as `MARV-#` tasks in
[`docs/roadmap.md`](docs/roadmap.md) — go there for what to build next.

- **M0 — Front end.** Lexer + parser + AST + canonical formatter. Gate: the round-trip
  property `parse ∘ format == id` holds on all canonical forms (proptest).
- **M1 — Core IR.** ANF lowering, de Bruijn conversion, blake3 hashing. Gate: alpha-equivalent
  surface programs lower to *identical* Core hashes (golden tests).
- **M2 — Checker.** Type + effect + capability checking, error-set inference, second-class
  reference & linearity checks; diagnostics emit machine-actionable fixes.
- **M3 — Query server.** Wire `marv-db` (salsa) + `marv-server` (JSON-RPC). Expose `check`,
  `typeAt`, `errorSet`, `effects`, `canonical`, `core`, `hash`.
- **M4 — Run it.** `marv-interp` first (as oracle), then Cranelift native codegen.
- **M5 — Web.** WASM/component backend + a browser demo proving capability-gated sandboxing.
- **M6 — Verify.** Runtime contract checks everywhere; then SMT discharge for the verified
  subset (`marv-verify`).
- **M7 — Reuse & self-host.** Content-addressed store + lockfile; begin Stage-1 self-hosting.

## Non-negotiable invariants (enforce in CI)

These are the spirit of the language. Any implementation choice that violates one is wrong,
even if it passes tests:

1. **One canonical form.** The formatter is the parser's inverse; there is exactly one way to
   write any program. No style options.
2. **No hidden control flow / no hidden allocation.** Every effect is visible at the call site;
   heap allocation happens only via an explicit `Alloc` capability.
3. **No ambient authority.** No global I/O, clock, randomness, or network. Power enters a
   function only through capability parameters; capabilities are unforgeable (received or
   narrowed, never constructed).
4. **Local reasoning.** No cross-function type inference; every signature is fully annotated.
   Inference exists only *inside* a function body.
5. **Determinism.** Compilation and builds are bit-for-bit reproducible; same snapshot ⇒ same
   hashes, diagnostics, and ordering.

## Architecture concepts that span multiple specs

- **The Core IR is identity.** Surface syntax is thin sugar; the unit of identity is the
  blake3-256 content hash of a definition's Core IR (ANF + de Bruijn, names erased, children
  resolved to hashes → a Merkle DAG of code). Alpha-equivalent programs hash identically. This
  is what gives free renames, automatic dedup, reproducible builds, and "has this hash been
  audited before?" as a first-class query. See `spec/02` §C and §F.
- **The compiler is a service first, a CLI second.** It is built as a salsa-backed incremental
  query engine wrapped in a JSON-RPC server. The agent owns an in-memory **snapshot** (disk is
  optional) and drives the generate→check→repair loop (`spec/03` §5). Every phase
  (parse → lower → typecheck → effects/errors → verify) is a demand-driven query.
- **Diagnostics carry fixes.** Every diagnostic should ship at least one machine-actionable
  `Fix` (edits + confidence) where one is mechanically derivable — missing capability, missing
  error in set, non-exhaustive match, unconsumed linear value, escaping reference. The compiler
  proposes the repair, it does not merely reject. Error codes (`E0001`…) are stable.
- **Layered verification (be honest about tiers).** Tier 0 (types/effects/capabilities/error
  sets/linearity) is *always* statically guaranteed. Tier 1 (runtime contracts) checks every
  `requires`/`ensures`/`invariant` in debug builds. Tier 2 (SMT proof) covers only a defined
  decidable-ish subset and returns a **counterexample** on failure, or honestly reports
  `unsupported` and falls back to Tier 1. See `spec/01` §7.
- **Memory safety with no GC and no lifetimes.** Mutable value semantics (no shared mutable
  aliasing of owned values) + **second-class references** (`&T`/`&mut T` may be passed down but
  never stored in a field, returned, or captured). This single restriction makes all aliasing
  reasoning local. `linear` types must be consumed exactly once (forgetting to `close` a `File`
  is a compile error). See `spec/01` §4.

## Knowledge Base

### Project-specific — `~/Documents/josh-obsidian-synced/Projects/marv/`

- **Code:** `/Users/joaoh82/projects/marv`
- **Context (read first):** `~/Documents/josh-obsidian-synced/Projects/marv/context.md`
- **Notes (running journal):** `~/Documents/josh-obsidian-synced/Projects/marv/notes.md`
- **Project wiki:** `~/Documents/josh-obsidian-synced/Projects/marv/wiki/`

**How to use each:**

- `context.md` — stable background (product goals, stakeholders, domain). Read before starting non-trivial work. Update only when underlying facts change.
- `notes.md` — append-only dated journal. Add entries under `## YYYY-MM-DD` headings for decisions, blockers, TODOs, and incidents — anything worth preserving but not stable enough for `context.md`.
- `wiki/` — reference sub-docs (e.g. `Architecture.md`, `Local Dev Setup.md`, `Tech Services.md`). Create new files as topics emerge.

**When to save:**

- New stable fact about the product/domain → update `context.md`.
- A decision, incident, or working note → append a dated entry to `notes.md`.
- Reusable reference material (setup steps, credential locations, architecture) → new/updated file in `wiki/`.

### Cross-project knowledge — `~/Documents/josh-obsidian-synced/vault/`

- **General wiki:** `~/Documents/josh-obsidian-synced/vault/wiki/` — start at `_master-index.md`, then drill into the relevant topic's `_index.md`.
- **Raw dumps:** `~/Documents/josh-obsidian-synced/vault/raw/` — drop unprocessed research here as `YYYY-MM-DD-{slug}.md`.

Read the general wiki when the question isn't specific to this project. Drop raw research or imported notes into `vault/raw/` so it's captured even before it's distilled.
