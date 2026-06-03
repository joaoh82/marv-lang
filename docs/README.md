# marv documentation

Human-facing documentation for the marv toolchain. The **specs** in
[`../spec/`](../spec) are the normative source of truth for the *language* and
*protocol*; the docs here cover *using the toolchain* and the state of the
implementation, and are expected to grow with each milestone.

## Index

| Doc | Covers |
|-----|--------|
| [`cli.md`](cli.md) | The `marv` command-line interface and its subcommands. |
| [`formatter.md`](formatter.md) | The canonical formatter: what "canonical form" means and the current M0 status. |
| [`core-ir.md`](core-ir.md) | The Core IR and content hashing: ANF + de Bruijn lowering and `blake3` identity (M1). |
| [`checker.md`](checker.md) | The checker: type / effect / capability / error-set / reference / linearity checking and the error-code catalog (M2). |
| [`query-server.md`](query-server.md) | The incremental query engine (`salsa`) and the JSON-RPC agent protocol: snapshots, the method catalog, and the generate→check→repair loop (M3). |
| [`run-and-codegen.md`](run-and-codegen.md) | Executing marv: the tree-walking interpreter (semantics oracle), the Cranelift backend, capability-gated `run`, and the differential gate (M4). |

## Relationship to the rest of the repo

- [`../spec/`](../spec) — the *why/what* and the *exact form* (design, grammar, Core IR, protocol). Normative.
- [`../examples/`](../examples) — illustrative `.mv` programs, kept in canonical form.
- [`../tests/`](../tests) — repository-level golden/round-trip fixtures.
- `docs/` (here) — how to use the toolchain; implementation status per milestone.

## Keeping docs current

When a milestone changes observable behavior — a new subcommand, a new protocol
method, the formatter learning to reflow code — update the matching doc in the
same change. See CLAUDE.md, "Keeping examples, tests, and docs current".
