# examples/

Illustrative marv (`.mv`) programs that track the language as the specs in
[`../spec/`](../spec) describe it. They are reference samples for humans and a
fixture source for the test suite. As of M4 the integer/boolean subset is
**runnable** — see `factorial.mv` and `arithmetic.mv` below and
[`../docs/run-and-codegen.md`](../docs/run-and-codegen.md).

| File | Shows |
|------|-------|
| [`hello.mv`](hello.mv) | Capabilities as parameters — power enters only through `Io`. |
| [`clamp.mv`](clamp.mv) | A `pure` function with `requires`/`ensures` contracts (Tier-2 verifiable subset). |
| [`report.mv`](report.mv) | `struct`/`error` decls, second-class `&` references, a loop `invariant`, effect rows, and inferred error sets. |
| [`geometry.mv`](geometry.mv) | The **M0 parsed subset** end to end: `struct`/`linear struct`, `pure fn`, `&`/`&mut` params, `if`/`else`, fully-parenthesized binary operators. Round-trips through the real parser. |
| [`factorial.mv`](factorial.mv) | **Runnable (M4):** recursion + an `if`. `marv run --entry factorial 6` and `marv build --run …` both yield `720`. |
| [`arithmetic.mv`](arithmetic.mv) | **Runnable (M4):** a nullary `main` that calls two other functions — curried cross-function calls lowered to direct native calls. |

`hello`, `clamp`, and `report` use features still beyond the M0 parser, so `marv
fmt` normalizes them with its whitespace fallback for now. `geometry.mv`,
`factorial.mv`, and `arithmetic.mv` are inside the parsed subset, so `marv fmt`
reprints them from the AST and the `examples_are_canonical` test exercises the
parser itself. `factorial.mv` and `arithmetic.mv` additionally lie inside the
*executable* subset, so both the interpreter and the Cranelift backend run them
(`marv run` / `marv build --run`).

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
