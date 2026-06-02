# The canonical formatter

> **Invariant #1 — one canonical form.** The formatter is the parser's inverse;
> there is exactly one way to write any program. No style options.
> — `spec/README.md`, non-negotiable invariants

This is foundational, not cosmetic. A single canonical form means diffs are
minimal, review never spends attention on style, and the round-trip property
`parse ∘ format == id` (the M0 acceptance gate) is well-defined.

## Where it lives

- `marv_syntax::format(&str) -> String` — the library entry point (crate
  `marv-syntax`, milestone M0).
- `marv fmt` — the CLI wrapper. See [`cli.md`](cli.md).

## Current status: the M0 whitespace subset

The lexer/parser/AST are still being built, so a true parse-and-reprint formatter
does not exist yet. What ships today is the conservative subset that needs no
parser — a **whitespace canonicalizer**:

| Rule | Effect |
|------|--------|
| Line endings | `\r\n` and `\r` → `\n` |
| Tabs | expand to 4 spaces |
| Trailing whitespace | stripped from every line |
| Blank-line runs | collapsed to a single blank line |
| Leading blank lines | dropped |
| File ending | exactly one trailing newline |

Properties it already guarantees:

- **Deterministic** — same input, same output, every time.
- **Idempotent** — `format(format(x)) == format(x)`. Enforced by tests.

What it deliberately does **not** do yet (these require the parser): reflow
internal spacing (`fn  main` stays as written), normalize indentation depth,
insert canonical parenthesization, or normalize `;` separators.

## Tests

- Unit tests: `crates/marv-syntax/src/lib.rs` (`#[cfg(test)]`).
- Golden + property tests: `crates/marv-syntax/tests/golden.rs`, driven by
  `tests/fmt/*.in.mv` / `*.out.mv` fixtures and the canonical `examples/*.mv`.

## Roadmap

1. **M0 completion** — lexer + recursive-descent parser + AST, then replace the
   whitespace pass with parse-and-reprint. Prove `parse ∘ format == id` with
   proptest.
2. Expose the formatter as the `marv/canonical` and `marv/format` protocol
   methods (`spec/03`, milestone M3).

When the formatter learns a new normalization, add or update a fixture in
`tests/fmt/`, refresh any affected `examples/`, and update this doc — in the same
change.
