# examples/

Illustrative marv (`.mv`) programs that track the language as the specs in
[`../spec/`](../spec) describe it. They are reference samples for humans and a
fixture source for the test suite — not yet compilable (codegen is milestone M4).

| File | Shows |
|------|-------|
| [`hello.mv`](hello.mv) | Capabilities as parameters — power enters only through `Io`. |
| [`clamp.mv`](clamp.mv) | A `pure` function with `requires`/`ensures` contracts (Tier-2 verifiable subset). |
| [`report.mv`](report.mv) | `struct`/`error` decls, second-class `&` references, a loop `invariant`, effect rows, and inferred error sets. |
| [`geometry.mv`](geometry.mv) | The **M0 parsed subset** end to end: `struct`/`linear struct`, `pure fn`, `&`/`&mut` params, `if`/`else`, fully-parenthesized binary operators. Round-trips through the real parser. |

`hello`, `clamp`, and `report` use features still beyond the M0 parser, so `marv
fmt` normalizes them with its whitespace fallback for now. `geometry.mv` is
deliberately inside the parsed subset, so `marv fmt` reprints it from the AST and
the `examples_are_canonical` test exercises the parser itself.

## Invariant: examples stay canonical

Every `.mv` file here **must already be in canonical form**. The integration
test `examples_are_canonical` in
[`../crates/marv-syntax/tests/golden.rs`](../crates/marv-syntax/tests/golden.rs)
runs `marv fmt` over each file and fails if it would change anything. Before
committing a new or edited example:

```sh
marv fmt --write examples/*.mv   # or: cargo run --bin marv -- fmt --write examples/*.mv
```

As the formatter's parsed subset grows toward full coverage, this test keeps the
examples honest automatically.
