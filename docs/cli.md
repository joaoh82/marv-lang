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
| `fmt`    | **working** (M0, whitespace subset) | Canonicalize marv source. |
| `check`  | milestone M2 | Type / effect / capability checking. |
| `build`  | milestone M4 | Compile a target. |
| `verify` | milestone M6 | Discharge contracts via SMT. |

Commands that are not yet implemented parse their arguments and exit non-zero
with `not yet implemented (milestone Mx)`, so scripts get an honest signal.

## `marv fmt`

```
marv fmt [--check] [files...]
```

- **No file arguments** — reads stdin and writes canonical form to stdout (a
  filter you can pipe through).
- **File arguments** — formats each file **in place**.
- **`--check`** — writes nothing; exits non-zero if any input is not already in
  canonical form. Useful in CI and pre-commit hooks.

Examples:

```sh
# Filter mode
printf 'fn f(){\t}\n\n\n' | marv fmt

# Format files in place
marv fmt examples/*.mv

# CI gate: fail if anything is unformatted
marv fmt --check examples/*.mv tests/fmt/*.out.mv
```

### What `fmt` does today

The full formatter is the *inverse of the parser* — exactly one textual form per
program (non-negotiable invariant #1). The parser is milestone M0 and not done
yet, so `fmt` currently runs the parser-free **whitespace canonicalizer**:
normalize line endings, expand tabs to 4 spaces, strip trailing whitespace,
collapse blank-line runs, drop leading blank lines, and guarantee a single
trailing newline. It is deterministic and idempotent. It does **not** yet reflow
code (internal spacing, indentation depth, parenthesization). See
[`formatter.md`](formatter.md).

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success (and, for `fmt --check`, all inputs already canonical). |
| `1` | Usage error, I/O error, unimplemented command, or `--check` found non-canonical input. |
