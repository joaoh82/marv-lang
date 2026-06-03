# web/ — the marv WebAssembly sandbox demo (M5)

A dependency-free browser demo proving **capability-gated sandboxing**: a module
compiled from marv to WebAssembly can do nothing but compute unless the host page
hands it the specific capabilities (host imports) it needs. See
[`../docs/run-and-codegen.md`](../docs/run-and-codegen.md) and `spec/01` §9.

## Contents

| File | What it is |
|------|------------|
| `index.html` | The demo page (inline JS, no build step). |
| `factorial.wasm` | `examples/factorial.mv` compiled to wasm — **pure**, imports nothing. |
| `arithmetic.wasm` | `examples/arithmetic.mv` compiled to wasm — pure, nullary `main`. |
| `fetcher.core.json` | A Core-IR snapshot: `fetch(net: Net)` that `perform`s `Net`. |
| `fetcher.wasm` | `fetcher.core.json` compiled to wasm — imports `Net::op0`. |

## Run it

A static server is enough (the page only `fetch()`es the `.wasm` files):

```sh
cd web
python3 -m http.server 8087
# open http://localhost:8087/
```

Then:

- **factorial** — its manifest reads *imports: none*; click "run (sandboxed)" to
  compute `factorial(n)` with zero host authority.
- **fetcher** — its manifest reads `Net::op0`. With "grant Net" **unchecked**,
  "run fetch()" fails at instantiation (the page provides no `Net` import, so the
  module can't start — it cannot reach the network). With it **checked**, the page
  supplies a `Net` import and `fetch()` runs through it.

## Regenerating the `.wasm` files

```sh
cargo run --bin marv -- build --target wasm-component examples/factorial.mv  -o web/factorial.wasm
cargo run --bin marv -- build --target wasm-component examples/arithmetic.mv -o web/arithmetic.wasm
cargo run --bin marv -- build --target wasm-component web/fetcher.core.json  -o web/fetcher.wasm
```

The committed `.wasm` files are small (≈60–110 bytes) and are kept in the repo so
the demo works without a toolchain build.
