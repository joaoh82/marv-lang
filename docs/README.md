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

## Relationship to the rest of the repo

- [`../spec/`](../spec) — the *why/what* and the *exact form* (design, grammar, Core IR, protocol). Normative.
- [`../examples/`](../examples) — illustrative `.mv` programs, kept in canonical form.
- [`../tests/`](../tests) — repository-level golden/round-trip fixtures.
- `docs/` (here) — how to use the toolchain; implementation status per milestone.

## Keeping docs current

When a milestone changes observable behavior — a new subcommand, a new protocol
method, the formatter learning to reflow code — update the matching doc in the
same change. See CLAUDE.md, "Keeping examples, tests, and docs current".
