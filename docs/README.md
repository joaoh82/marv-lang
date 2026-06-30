# marv documentation

Human-facing documentation for the marv toolchain. The **specs** in
[`../spec/`](../spec) are the normative source of truth for the *language* and
*protocol*; the docs here cover *using the toolchain* and the state of the
implementation, and are expected to grow with each milestone.

## Index

| Doc | Covers |
|-----|--------|
| [`roadmap.md`](roadmap.md) | The forward roadmap: phases, ordering, and the dependency graph mapping to the `MARV-#` tracker tasks. |
| [`language-reference.md`](language-reference.md) | The language: types, memory model, effects/capabilities, error sets, contracts, modules — with "implemented vs. designed" markers. |
| [`stdlib.md`](stdlib.md) | The standard prelude (`std/`): `Option`, `Result`, collections, bytes/UTF-8, HTTP request helpers, and capability interfaces. |
| [`platform-support.md`](platform-support.md) | Backends (interpreter, Cranelift, LLVM, WASM), host/target matrix, and tooling prerequisites. |
| [`packages.md`](packages.md) | `marv.toml` package manifests, local path dependencies, package-aware server snapshots, and lockfile workflow. |
| [`agents.md`](agents.md) | Using marv from LLMs/agents: the generate→check→repair loop, the MCP server, the Claude Code skill, and client wiring. |
| [`cli.md`](cli.md) | The `marv` command-line interface and its subcommands. |
| [`formatter.md`](formatter.md) | The canonical formatter: what "canonical form" means and the current M0 status. |
| [`core-ir.md`](core-ir.md) | The Core IR and content hashing: ANF + de Bruijn lowering and `blake3` identity (M1). |
| [`checker.md`](checker.md) | The checker: type / effect / capability / error-set / reference / linearity checking and the error-code catalog (M2). |
| [`query-server.md`](query-server.md) | The incremental query engine (`salsa`) and the JSON-RPC agent protocol: snapshots, the method catalog, and the generate→check→repair loop (M3). |
| [`run-and-codegen.md`](run-and-codegen.md) | Executing marv: the tree-walking interpreter (semantics oracle), the Cranelift backend, capability-gated `run`, and the differential gate (M4). |
| [`verification.md`](verification.md) | Contracts and layered verification: Tier-1 runtime checks and Tier-2 SMT discharge (proofs and counterexamples), the verified subset, and `marv verify` / `marv/verify` (M6). |
| [`store.md`](store.md) | Content-addressed store, lockfile, and reuse: the dag-hash Merkle DAG (free renames, transitive dedup), `marv commit` / `marv/commit`, and the Stage-1 self-hosting step (M7). |

## Relationship to the rest of the repo

- [`../spec/`](../spec) — the *why/what* and the *exact form* (design, grammar, Core IR, protocol). Normative.
- [`../examples/`](../examples) — illustrative `.mv` programs, kept in canonical form.
- [`../tests/`](../tests) — repository-level golden/round-trip fixtures.
- `docs/` (here) — how to use the toolchain; implementation status per milestone.

## Keeping docs current

When a milestone changes observable behavior — a new subcommand, a new protocol
method, the formatter learning to reflow code — update the matching doc in the
same change. See CLAUDE.md, "Keeping examples, tests, and docs current".
