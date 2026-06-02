# tests/

Repository-level test fixtures, shared across crates. Per `spec/README.md` this
is the home for golden tests, round-trip property tests, and (later) differential
tests against the Stage-0 oracle.

## Layout

```
tests/
  fmt/        # formatter golden fixtures: paired *.in.mv (raw) / *.out.mv (canonical)
```

More subdirectories arrive with their milestones — e.g. `core/` for Core-IR hash
goldens (M1), `check/` for diagnostic goldens (M2), `diff/` for Stage-0 vs
Stage-1 differential cases (M7).

## How these run

Cargo only discovers integration tests inside a package, so the runnable harness
lives in [`../crates/marv-syntax/tests/golden.rs`](../crates/marv-syntax/tests/golden.rs)
and reads fixtures from this directory by relative path. `cargo test` exercises:

- **`fmt_golden_fixtures`** — for each `tests/fmt/*.in.mv`, asserts
  `format(input) == <name>.out.mv` and that the result is idempotent.
- **`examples_are_canonical`** — asserts every file in `../examples/` is already
  canonical.

## Adding a formatter fixture

1. Write the raw input as `tests/fmt/<name>.in.mv` (trailing whitespace, tabs,
   blank-line runs — whatever you want normalized).
2. Generate the golden output:
   `cargo run --bin marv -- fmt < tests/fmt/<name>.in.mv > tests/fmt/<name>.out.mv`
3. Eyeball the `.out.mv`, then `cargo test` to lock it in.
