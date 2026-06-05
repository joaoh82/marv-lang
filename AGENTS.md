# AGENTS.md

Instructions for coding agents (Codex, Cursor, and other harnesses that read `AGENTS.md`)
working in this repository.

## Working rules live in CLAUDE.md

The canonical contributor rules for this repo — what the project is, the build milestones and
their acceptance gates, the non-negotiable invariants, and where project knowledge lives —
are in **[`CLAUDE.md`](CLAUDE.md)**. Read it first and treat it as authoritative; this file
does not duplicate it (so the two can't drift). Everything in `CLAUDE.md` applies to you too.

## Driving the marv toolchain

If your task is to *write, check, run, or verify `.mv` programs* (as opposed to editing the
compiler itself), the generate→check→repair loop, the CLI cheat-sheet, the capability model,
and the MCP-server setup are documented in **[`docs/agents.md`](docs/agents.md)**.

## Codex-specific guidelines

<!--
Add your own Codex-specific instructions below. This section is yours to own — anything that
should apply when Codex (not Claude) works in this repo. The sections above just keep the
shared rules in one place (CLAUDE.md) instead of copied here.
-->
