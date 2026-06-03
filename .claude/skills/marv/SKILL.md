---
name: marv
description: >-
  Drive the marv language toolchain — author, check, repair, run, verify, and commit .mv
  programs via the generate→check→repair loop. Use when working with .mv source, .core.json
  Core-IR snapshots, the `marv` CLI, or the marv MCP server / JSON-RPC protocol.
---

# Driving marv

marv is a compiled language whose author is a coding agent and whose auditor is a human. The
compiler is a service (JSON-RPC, salsa-incremental) plus a CLI. Your job is to run the loop
tightly and respect the invariants. Full context: [`AGENTS.md`](../../../AGENTS.md),
[`README.md`](../../../README.md), and `spec/03` (the protocol).

## The loop

1. **check** the file(s) you generated (`marv check <file>`, or the `marv_check` MCP tool).
2. If there are errors, prefer **applying the fix** a diagnostic carries (confidence ≥ 0.8)
   over regenerating; otherwise regenerate the offending definition using the message.
3. **fmt** to canonical form (`marv fmt --write`) — never hand-format; there is exactly one form.
4. **verify** pure / verified-subset definitions (`marv verify`); on `failed`, use the
   counterexample to repair, then re-verify. `unsupported` is fine (falls back to runtime checks).
5. **run / build** with an explicit capability grant (`marv run --grant …`, `marv build`).
6. **commit** to freeze reproducible hashes (`marv commit`); already-reviewed hashes need no re-audit.

## Two ways to call it

- **CLI** — `marv fmt|check|run|build|verify|commit` (see `docs/cli.md`). Good for one-shot work.
- **MCP server** (`marv-mcp`) — tool-call access with a persistent snapshot: `marv_open_snapshot`
  then `marv_check` / `marv_signature` / `marv_error_set` / `marv_effects` / `marv_core` /
  `marv_verify` / `marv_apply_fix` / `marv_format` / `marv_commit`. Wiring: `docs/agents.md`.

## Invariants (do not fight these)

- **No ambient authority** — power enters only through capability parameters (`Io`, `Fs`,
  `Net`, `Clock`, `Rand`, `Alloc`); pass a function only what it needs. A function with no
  `Net` parameter cannot reach the network.
- **One canonical form** — run `fmt`, don't argue about style.
- **Local reasoning** — every signature is fully annotated; annotate.
- **Determinism** — same source ⇒ same hashes/diagnostics.
- **Honesty** — don't claim a check/verify passed that you didn't run; the tools report
  `unsupported` rather than guessing, and so should you.

## What parses today

`mod`/`import`, `struct`/`fn` (incl. `pure fn`), `let`/`var`, `if`/`else`, the binary
operators, calls/recursion, field projection, and `requires`/`ensures` contracts. Enums/
`match`, loops, `?`, generics, capabilities-from-source, and collections are roadmap — for
those today, use a `*.core.json` snapshot (see `tests/run/*.core.json`). Don't generate
surface that won't parse.
