# tests/

Repository-level test fixtures, shared across crates. Per `spec/README.md` this
is the home for golden tests, round-trip property tests, and (later) differential
tests against the Stage-0 oracle.

## Layout

```
tests/
  fmt/        # formatter golden fixtures: paired *.in.mv (raw) / *.out.mv (canonical)
  run/        # M4 execution corpus: runnable .mv programs (+ one .core.json that
              # must fail to compile) for the interpreter↔Cranelift differential test
```

More subdirectories arrive with their milestones — e.g. `core/` for Core-IR hash
goldens (M1), `check/` for diagnostic goldens (M2), `diff/` for Stage-0 vs
Stage-1 differential cases (M7).

## How these run

Cargo only discovers integration tests inside a package, so the runnable harness
lives in [`../crates/marv-syntax/tests/`](../crates/marv-syntax/tests) and reads
fixtures from this directory by relative path. `cargo test` exercises:

- **`fmt_golden_fixtures`** (`golden.rs`) — for each `tests/fmt/*.in.mv`, asserts
  `format(input) == <name>.out.mv` and that the result is idempotent.
- **`examples_are_canonical`** (`golden.rs`) — asserts every file in
  `../examples/` is already canonical.
- **`parse_format_roundtrip`** / **`format_is_idempotent`** (`roundtrip.rs`) — the
  M0 acceptance gate. A built-in deterministic LCG generates thousands of
  in-subset ASTs and asserts `parse(format(ast)) == ast`, plus idempotence. No
  external crates (proptest can be swapped in later).
- **`interpreter_and_cranelift_agree`** / **`capability_outside_effect_row_fails_to_compile`**
  (`../crates/marv-codegen-cl/tests/differential.rs`) — the M4 acceptance gate.
  Each `run/*.mv` program is executed by *both* the interpreter and the Cranelift
  backend and the two results must match (and equal a golden value); the
  `run/uses_ungranted_cap.core.json` snapshot must be rejected by the checker.
- **`wasm_agrees_with_interpreter`** (`../crates/marv-codegen-wasm/tests/differential.rs`)
  — the M5 acceptance gate. The same `run/*.mv` corpus is compiled to WebAssembly
  and executed under **wasmtime**, matching the interpreter; a pure module imports
  nothing while a capability-using module surfaces that capability as a host
  import. The browser side of the sandbox lives in [`../web/`](../web).

Fixtures come in two flavors: **in-subset** cases (e.g. `fmt/decls.in.mv`) drive
the real parse-and-reprint formatter (indentation, parenthesization, spacing),
while cases outside the M0 parser (e.g. `fmt/whitespace.in.mv`, `fmt/tabs.in.mv`)
exercise the whitespace fallback.

## Adding a formatter fixture

1. Write the raw input as `tests/fmt/<name>.in.mv` (trailing whitespace, tabs,
   blank-line runs — whatever you want normalized).
2. Generate the golden output (stdin/stdout filter mode):
   `cargo run --bin marv -- fmt < tests/fmt/<name>.in.mv > tests/fmt/<name>.out.mv`
3. Eyeball the `.out.mv`, then `cargo test` to lock it in.

To exercise the parse-and-reprint path rather than the whitespace fallback, the
input must be a complete M0-subset module (starts with `mod`, uses only supported
items). Otherwise the fixture just tests the fallback.
