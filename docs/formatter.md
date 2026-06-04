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

## Current status: M0 parse-and-reprint (with whitespace fallback)

`format` is now **hybrid**. It parses its input and, if the input is an M0-subset
module, reprints it in true canonical form (the parser's inverse). Input that is
*outside* the parsed subset — or otherwise unparseable — falls back to the
original parser-free **whitespace canonicalizer**. As later milestones widen the
parsed grammar, more programs take the full parse-and-reprint path.

### The parsed subset (full canonicalization)

When the input parses, the formatter applies every canonical rule:

| Rule | Effect |
|------|--------|
| Indentation | 4 spaces per block level, recomputed from structure |
| Statements | one per line; blank lines between top-level items |
| Binary operators | every node fully parenthesized as `(a op b)` |
| Spacing | exactly one space around `=`, `:`, `->`, operators; `, ` separators |
| Trailing commas / semicolons | removed |
| Integer literals | `1_000` → `1000` |
| String escapes | normalized (`\n`, `\t`, `\r`, `\"`, `\\`) |
| File ending | exactly one trailing newline |

The covered subset is module headers, imports, `struct`/`fn` declarations
(`pure fn`, `linear struct`), the type language (named, `[]T`, `&`/`&mut`, `()`),
`let`/`var`/`return` statements, block tails, and value expressions with binary
operators and `if`/`else`. See `crates/marv-syntax/src/ast.rs`.

### The whitespace fallback

For input the parser does not (yet) accept, `format` normalizes line endings,
expands tabs to 4 spaces, strips trailing whitespace, collapses blank-line runs,
drops leading blank lines, and guarantees a single trailing newline — but does
not reflow code. It is exposed directly as `marv_syntax::canonicalize_whitespace`.

Both paths are **deterministic** and **idempotent** (`format(format(x)) ==
format(x)`), enforced by tests.

## Tests

- Unit tests: `crates/marv-syntax/src/lib.rs` (`#[cfg(test)]`).
- Round-trip + idempotence property tests: `crates/marv-syntax/tests/roundtrip.rs`
  — a built-in deterministic LCG generates thousands of in-subset ASTs and asserts
  `parse(format(ast)) == ast` (the M0 acceptance gate).
- Golden tests: `crates/marv-syntax/tests/golden.rs`, driven by `tests/fmt/*.in.mv`
  / `*.out.mv` fixtures and the canonical `examples/*.mv`.

## Roadmap

1. **M0 (done)** — lexer + recursive-descent parser + AST + parse-and-reprint
   formatter over a bounded subset; `parse ∘ format == id` proven by a property
   test. The subset has since grown to include `requires`/`ensures` contracts and
   (MARV-1) `enum` declarations, `match` expressions, and generic parameter
   lists/arguments. Continue widening the parsed grammar (`while`/`for`, `?`,
   error unions, struct/collection literals) toward full coverage so the
   whitespace fallback fades out.
2. Expose the formatter as the `marv/canonical` and `marv/format` protocol
   methods (`spec/03`, milestone M3).

When the formatter learns a new normalization, add or update a fixture in
`tests/fmt/`, refresh any affected `examples/`, and update this doc — in the same
change.
