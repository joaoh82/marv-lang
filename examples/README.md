# examples/

Illustrative marv (`.mv`) programs that track the language as the specs in
[`../spec/`](../spec) describe it. They are reference samples for humans and a
fixture source for the test suite. As of M4 the integer/boolean subset is
**runnable** — see `factorial.mv` and `arithmetic.mv` below and
[`../docs/run-and-codegen.md`](../docs/run-and-codegen.md).

| File | Shows |
|------|-------|
| [`hello.mv`](hello.mv) | Capabilities as parameters — power enters only through `Io`. |
| [`clamp.mv`](clamp.mv) | **Verifiable (M6):** a `pure` function with `requires`/`ensures` contracts. `marv verify examples/clamp.mv` proves it (Tier 2); `marv run` enforces it at runtime (Tier 1). |
| [`report.mv`](report.mv) | `struct`/`error` decls, second-class `&` references, a loop `invariant`, effect rows, and inferred error sets. |
| [`geometry.mv`](geometry.mv) | The **M0 parsed subset** end to end: `struct`/`linear struct`, `pure fn`, `&`/`&mut` params, `if`/`else`, fully-parenthesized binary operators. Round-trips through the real parser. |
| [`factorial.mv`](factorial.mv) | **Runnable (M4):** recursion + an `if`. `marv run --entry factorial 6` and `marv build --run …` both yield `720`. |
| [`arithmetic.mv`](arithmetic.mv) | **Runnable (M4):** a nullary `main` that calls two other functions — curried cross-function calls lowered to direct native calls. |

`hello` and `report` use features still beyond the M0 parser, so `marv fmt`
normalizes them with its whitespace fallback for now. `geometry.mv`, `clamp.mv`,
`factorial.mv`, and `arithmetic.mv` are inside the parsed subset, so `marv fmt`
reprints them from the AST and the `examples_are_canonical` test exercises the
parser itself (`clamp.mv` joined this set in M6, when `requires`/`ensures`
clauses became parseable — note the formatter does not yet preserve `///` doc
comments, so contract examples carry none). `factorial.mv` and `arithmetic.mv` additionally lie inside the
*executable* subset, so all three backends run them — the interpreter
(`marv run`), the Cranelift JIT (`marv build --run`), and WebAssembly
(`marv build --target wasm-component`, then via wasmtime or the browser demo in
[`../web/`](../web)).

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
