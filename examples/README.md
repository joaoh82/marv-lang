# examples/

Illustrative marv (`.mv`) programs that track the language as the specs in
[`../spec/`](../spec) describe it. They are reference samples for humans and a
fixture source for the test suite. As of M4 the integer/boolean subset is
**runnable** — see `factorial.mv` and `arithmetic.mv` below and
[`../docs/run-and-codegen.md`](../docs/run-and-codegen.md).

| File | Shows |
|------|-------|
| [`hello.mv`](hello.mv) | **Runnable (MARV-6):** capabilities & `perform` from source — `io.stdout()` narrows `Io` to a `Stream`, `out.write(...)` performs. `marv run --grant Io examples/hello.mv` logs the `Io`/`Stream` effects; without `--grant Io` it is refused. |
| [`read_file.mv`](read_file.mv) | **Runnable (MARV-6):** capability **narrowing** — `io.fs()` attenuates `Io` to `Fs`, then `fs.read(path)` performs. `marv run --grant Io examples/read_file.mv /etc/hosts` records the `Io`→`Fs` narrowing and the read; the signature alone proves it touches only the filesystem. |
| [`clamp.mv`](clamp.mv) | **Verifiable (M6):** a `pure` function with `requires`/`ensures` contracts. `marv verify examples/clamp.mv` proves it (Tier 2); `marv run` enforces it at runtime (Tier 1). |
| [`report.mv`](report.mv) | **Checks (MARV-6):** `struct`/`error` decls, second-class `&` references, a loop `invariant`, an inferred error set, and a real capability `perform` — `load_and_total(fs: Fs, …)` does `fs.read(path)?`. |
| [`geometry.mv`](geometry.mv) | The **M0 parsed subset** end to end: `struct`/`linear struct`, `pure fn`, `&`/`&mut` params, `if`/`else`, fully-parenthesized binary operators. Round-trips through the real parser. |
| [`factorial.mv`](factorial.mv) | **Runnable (M4):** recursion + an `if`. `marv run --entry factorial 6` and `marv build --run …` both yield `720`. |
| [`arithmetic.mv`](arithmetic.mv) | **Runnable (M4):** a nullary `main` that calls two other functions — curried cross-function calls lowered to direct native calls. |
| [`color.mv`](color.mv) | **Runnable:** an `enum` + an exhaustive `match`. `main` constructs `Color.Green` and `rank` matches it; `marv run --entry main examples/color.mv` yields `2`. Drop an arm and `marv check` fires E0130 (non-exhaustive). |
| [`mutation.mv`](mutation.mv) | **Runnable:** construction + mutation (MARV-4) — a `struct` literal `Point { x: …, y: … }`, a `var` accumulator reassigned with `total = …`, and an in-place field update `q.x = …`. `marv run --entry main examples/mutation.mv` yields `45`; mutating the copy `q` leaves the original untouched (mutable value semantics, `spec/01` §4). |
| [`quantifiers.mv`](quantifiers.mv) | **Provable (MARV-11):** the expanded verified subset — a bounded `forall` over an array parameter (`requires (forall i in 0..len(a): (a[i] >= lo))`), an `exists` conclusion discharged from a sortedness premise over a slice, truncate-toward-zero `/`/`%` proved via the quotient/remainder identity *in the contract itself*, and `old(n)` in `ensures`. `marv verify examples/quantifiers.mv` proves all four; `marv run --entry bump examples/quantifiers.mv -- 41` yields `42`, and a violated quantified `requires` aborts at runtime (Tier 1). |
| [`loops.mv`](loops.mv) | **Runnable (MARV-2) + provable (MARV-22):** `while` loops carrying `var`s across iterations, with `invariant`s checked at runtime (Tier 1) *and* discharged by SMT (Tier 2) — `marv verify examples/loops.mv` proves `sum_to` and `pow`. `marv run --entry sum_to examples/loops.mv 5` yields `15`; it runs identically on the interpreter, Cranelift JIT, and WASM (differential corpus). |
| [`casts.mv`](casts.mv) | **Runnable (MARV-7):** `char` literals (`'\n'`), `as` casts (`(n as u8)`, widening + narrowing), the fixed-array type `[N]T`, and `len(str)`. Integer casts truncate/wrap to width identically on the interpreter, Cranelift, and WASM (`tests/run/casts.mv`); a constant that overflows its narrowing target (`256 as u8`) fails `marv check` with `E0104`. |
| [`arrays.mv`](arrays.mv) | **Runnable (MARV-30):** array literals `[e0, …]`, indexed read `a[i]`, `len(a)`, the index store `a[i] = e` (a functional element update under mutable value semantics), and a `len`-bounded `while`/`for`. `marv run examples/arrays.mv` yields `42`; it runs identically on the interpreter, Cranelift JIT, and WASM (differential corpus [`tests/run/arrays.mv`](../tests/run/arrays.mv)). |
| [`slices.mv`](slices.mv) | **Runnable (MARV-33 + MARV-20):** runtime-length slices `[]T` — an array literal bound to `[]i64` (the slice constructor), a `len`-bounded `while` over a `&[]i64` reference, a `for x in xs` over the same slice (the desugared index loop executing for real), and the element store `ys[0] = v` (`Core::IndexSet`, a functional update over a runtime length). `marv run examples/slices.mv` yields `30`; the same shapes are pinned three-way (interpreter / Cranelift / WASM) in the differential corpus [`tests/run/slices.mv`](../tests/run/slices.mv). |
| [`optionals.mv`](optionals.mv) | **Runnable (MARV-18):** constructing and matching an enum **imported from another module**, checked as a single file — `import std.option (Option)`, then `Option.Some(n)` / `Option.None` and an exhaustive `match`, resolved to the imported enum's real constructors (correct tags, `std.option.Option` nominal) by the CLI's `std` resolution. `marv check examples/optionals.mv` is clean; `marv run --entry main examples/optionals.mv` yields `42`, and the same program runs on the Cranelift JIT (`marv build --run --entry main`). |
| [`generics.mv`](generics.mv) | **Runnable (MARV-5):** generics + an `interface`/`impl` with a bound. `max[T: Ord](a, b)` calls the interface method `cmp`; `main` calls `max(3, 7)`, which **monomorphizes** to `max@i32` and **dispatches** `cmp` to the coherent `impl Ord[i32]`. `marv run --entry main examples/generics.mv` yields `7`; `marv resolve-impl examples/generics.mv` reports the selected impl; instantiating at a type with no impl (e.g. `max(true, false)`) fails `marv check` with `E0160`. Since the `Ordering` enum got a runtime layout (MARV-9), the monomorphized program also runs on the Cranelift JIT and WASM — an `i64` variant lives in the differential corpus as [`tests/run/generics.mv`](../tests/run/generics.mv) (MARV-26). |
| [`app_tokenizer.mv`](app_tokenizer.mv) | **Application example (MARV-40 / MARV-45):** scans a string, splits on separators, pushes token slices into a growable `List[str]` through explicit `Alloc`, and returns a deterministic token summary. Pinned in the three-way differential corpus as [`tests/run/app_tokenizer.mv`](../tests/run/app_tokenizer.mv). |
| [`app_router.mv`](app_router.mv) | **Application example (MARV-40 / MARV-46):** a tiny route classifier that builds a list of route prefixes, checks path prefixes with string indexing, and returns stable route codes. Pinned in the three-way differential corpus as [`tests/run/app_router.mv`](../tests/run/app_router.mv). |
| [`app_invoice_summary.mv`](app_invoice_summary.mv) | **Application example (MARV-40 / MARV-47):** parses a delimited invoice-like record, pushes signed amounts into `List[i64]`, and folds the list into a summary score. Pinned in the three-way differential corpus as [`tests/run/app_invoice_summary.mv`](../tests/run/app_invoice_summary.mv). |
| [`bytes_utf8.mv`](bytes_utf8.mv) | **Std example (MARV-54):** decodes a `[]u8` payload with `std.bytes.decode_utf8`, appends text, and encodes it back to `List[u8]` with explicit `Alloc`. The companion corpus checks decode on the interpreter and pins backend-safe encode/equality paths three-way in [`tests/run/bytes_utf8.mv`](../tests/run/bytes_utf8.mv). |

Every example now parses, formats, and checks through the **real** front end — the
`examples_are_canonical` test reprints each from the AST (the formatter's whitespace
fallback is no longer needed for any of them). `hello`, `read_file`, and `report` joined
when capabilities & `perform` from source landed (MARV-6); `clamp.mv` joined in M6 with
`requires`/`ensures` (`quantifiers.mv` followed under MARV-11 with bounded quantifiers,
contract arithmetic, and `old(e)`); `color.mv` when `enum`/`match` landed; `generics.mv` when
`interface`/`impl` + generic bounds landed (MARV-5); `arrays.mv` when array codegen landed
(MARV-30); `slices.mv` when runtime-length slices landed (MARV-33 + MARV-20);
`optionals.mv` when single-file lowering of imported enums landed (MARV-18);
`bytes_utf8.mv` when the source-level `std.bytes` UTF-8 helpers landed (MARV-54).
`factorial.mv`, `arithmetic.mv`, `color.mv`, `mutation.mv`, `loops.mv`,
`generics.mv`, `arrays.mv`, `slices.mv`, and `optionals.mv` additionally lie inside the
*executable* subset, so the interpreter runs them (`marv run`); the integer ones
(`factorial`, `arithmetic`, `loops`, `arrays`, `slices`) also run on the Cranelift JIT (`marv build --run`)
and WebAssembly (`marv build --target wasm-component`, then via wasmtime or the browser
demo in [`../web/`](../web)). `hello`/`read_file` run on the interpreter under
`marv run --grant Io` (capability ops are interpreter-modeled; Cranelift rejects
`perform`). `generics.mv` constructs an `enum` (`Ordering`); now that aggregate codegen
has landed (MARV-9), the monomorphized generic runs identically on the interpreter,
Cranelift, and WASM — exercised in the differential corpus by
[`tests/run/generics.mv`](../tests/run/generics.mv) (MARV-26). `arrays.mv` exercises array
literals, `len`/index, and the index store the same three-way (MARV-30).

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
