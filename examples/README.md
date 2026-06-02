# examples/

Illustrative marv (`.mv`) programs that track the language as the specs in
[`../spec/`](../spec) describe it. They are reference samples for humans and a
fixture source for the test suite — not yet compilable (the lexer/parser are
milestone M0, in progress).

| File | Shows |
|------|-------|
| [`hello.mv`](hello.mv) | Capabilities as parameters — power enters only through `Io`. |
| [`clamp.mv`](clamp.mv) | A `pure` function with `requires`/`ensures` contracts (Tier-2 verifiable subset). |
| [`report.mv`](report.mv) | `struct`/`error` decls, second-class `&` references, a loop `invariant`, effect rows, and inferred error sets. |

## Invariant: examples stay canonical

Every `.mv` file here **must already be in canonical form**. The integration
test `examples_are_canonical` in
[`../crates/marv-syntax/tests/golden.rs`](../crates/marv-syntax/tests/golden.rs)
runs `marv fmt` over each file and fails if it would change anything. Before
committing a new or edited example:

```sh
marv fmt examples/*.mv      # or: cargo run --bin marv -- fmt examples/*.mv
```

As the formatter grows from the M0 whitespace pass into the full
parse-and-reprint formatter, this test keeps the examples honest automatically.
