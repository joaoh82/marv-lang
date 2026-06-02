# The `marv` CLI

`marv` is the command-line front end for the toolchain (crate `marv-cli`). It is
the CLI half of the compiler; the agent-facing half is the JSON-RPC service
(`marv-server`, milestone M3) described in
[`../spec/03-compiler-protocol.md`](../spec/03-compiler-protocol.md).

```
marv <command> [args]
```

## Commands

| Command | Status | Description |
|---------|--------|-------------|
| `fmt`    | **working** (M0, parse-and-reprint + whitespace fallback) | Canonicalize marv source. |
| `check`  | milestone M2 | Type / effect / capability checking. |
| `build`  | milestone M4 | Compile a target. |
| `verify` | milestone M6 | Discharge contracts via SMT. |

Commands that are not yet implemented parse their arguments and exit non-zero
with `not yet implemented (milestone Mx)`, so scripts get an honest signal.

## `marv fmt`

```
marv fmt [--write|--check] [files...]
```

- **No file arguments** — reads stdin and writes canonical form to stdout (a
  filter you can pipe through).
- **File arguments, no flag** — prints each file's canonical form to stdout.
- **`--write`** — rewrites each file **in place**.
- **`--check`** — writes nothing; exits non-zero if any input is not already in
  canonical form. Useful in CI and pre-commit hooks. (Mutually exclusive with
  `--write`.)

Examples:

```sh
# Filter mode
printf 'mod m\nfn f(){\na+b\n}\n' | marv fmt

# Preview canonical form without touching the files
marv fmt examples/*.mv

# Format files in place
marv fmt --write examples/*.mv

# CI gate: fail if anything is unformatted
marv fmt --check examples/*.mv tests/fmt/*.out.mv
```

### What `fmt` does today

The formatter is the *inverse of the parser* — exactly one textual form per
program (non-negotiable invariant #1). As of M0 it **parses and reprints** the
implemented language subset (module headers, imports, `struct`/`fn` decls, the
type language, `let`/`var`/`return`, block tails, binary operators, `if`/`else`):
indentation, spacing, full parenthesization, and integer/string normalization are
all applied. Input outside that subset — or otherwise unparseable — falls back to
the parser-free **whitespace canonicalizer** (line endings, tabs → 4 spaces,
trailing-whitespace stripping, blank-line collapsing, single trailing newline).
Both paths are deterministic and idempotent. See [`formatter.md`](formatter.md).

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success (and, for `fmt --check`, all inputs already canonical). |
| `1` | Usage error, I/O error, unimplemented command, or `--check` found non-canonical input. |
