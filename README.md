<h1 align="center">marv</h1>

<p align="center">
  <em>A compiled programming language whose <b>author is a coding agent</b> and whose <b>auditor is a human</b>.</em>
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> ·
  <a href="#the-toolchain">Toolchain</a> ·
  <a href="#using-marv-from-an-agent">For agents</a> ·
  <a href="#documentation">Docs</a> ·
  <a href="spec/">Spec</a>
</p>

---

**marv** (short for *Marvin*) takes one premise seriously: most code will be written by
machines and read by people. Every design rule serves **explicitness**, **local
reasoning**, and **machine-verifiability** — so a human auditor can trust agent-generated
code by reading signatures, and an agent can generate→check→repair in a tight loop against
a real compiler service.

What that means concretely:

- **No hidden control flow or growable allocation.** Every effect is visible at the call site;
  user-visible growable heap allocation happens only through an explicit `Alloc` capability.
- **No ambient authority.** There is no global I/O, clock, randomness, or network. Power
  enters a function only through **capability** parameters, recorded in its effect row — so
  a signature *bounds* everything a function can do. This is also the WASM sandbox model.
- **Errors are values with inferred sets.** The complete failure surface is recoverable
  from the type.
- **Memory safety with no GC and no lifetimes** — mutable value semantics + second-class
  references + `linear` resource types.
- **Contracts are first-class** — `requires`/`ensures`/`invariant`, runtime-checked in debug
  and SMT-discharged for a verified subset.
- **The unit of identity is the content hash of a definition's Core IR.** Renames are free,
  identical code dedups, and builds are reproducible by lockfile.
- **The compiler is a service first, a CLI second** — a salsa-backed incremental query
  engine behind a JSON-RPC protocol, built for the agent loop.

> **Status:** the Stage-0 compiler (in Rust) is implemented end to end through milestones
> **M0–M7** — front end, Core IR + content hashing, the type/effect/capability checker, the
> incremental query server, two execution backends (a tree-walking interpreter and a
> Cranelift JIT), a WebAssembly backend with a capability-gated browser sandbox, layered
> verification (runtime contracts + SMT), and a content-addressed store with `commit`.
> The language *surface* is still a growing subset (see [Status & roadmap](#status--roadmap)).

---

## Quickstart

**Prerequisites:** a Rust toolchain (pinned to 1.94.0 via `rust-toolchain.toml`). For the
Tier-2 SMT verifier, a `z3` binary on `PATH` (`brew install z3` / `apt-get install z3`);
without it, `verify` honestly falls back to runtime checks.

```sh
git clone https://github.com/joaoh82/marv-lang
cd marv-lang
make build            # cargo build --release  → ./target/release/marv
make test             # cargo test --workspace (z3-backed verify tests run if z3 is present)
```

Then drive the toolchain (examples ship in [`examples/`](examples/)):

```sh
marv fmt examples/factorial.mv                      # canonical form (the parser's inverse)
marv check examples/factorial.mv                    # type / effect / capability / error / linearity
marv run examples/factorial.mv --entry factorial 6  # 720  (tree-walking interpreter — the oracle)
marv build --run examples/factorial.mv --entry factorial 6   # 720  (Cranelift JIT)
marv build --target wasm-component examples/factorial.mv -o factorial.wasm
marv verify examples/clamp.mv                       # proved  (Tier-2 SMT) — or a counterexample
marv commit examples/clamp.mv                       # freeze into the content-addressed store
```

(Or run any of these via `cargo run -p marv-cli -- <args>` without installing the binary.)

### The capability sandbox, in a browser

`web/` is a dependency-free demo proving capability-gated sandboxing: a pure module imports
nothing, while a module that wants the network imports `Net` and **cannot be instantiated**
unless the page grants it.

```sh
make build && marv build --target wasm-component examples/factorial.mv -o web/factorial.wasm
cd web && python3 -m http.server 8087   # open http://localhost:8087/
```

---

## The toolchain

`marv` is the CLI front end; the agent-facing half is the JSON-RPC service (`marv-server`).

| Command | What it does |
|---|---|
| `marv fmt [--write\|--check] [files…]` | Canonicalize source. The formatter is the parser's inverse — exactly one form per program. |
| `marv check <file>` | Type / effect / capability / error-set / reference / linearity checks; fix-carrying diagnostics. |
| `marv run [--grant CAP,…] [--entry NAME] <file> [args…]` | Interpret an entry point (the semantics oracle). Capabilities enter only via `--grant`. |
| `marv build [--target native-cranelift\|wasm-component] [--run] [--release] [--store DIR] [--out PATH] [--entry NAME] <file>` | Compile via Cranelift (JIT, `--run` to execute) or to a WebAssembly module. Only definitions reachable from the entry are compiled (MARV-8). With `--store`, imports/deps are fetched from pinned dag hashes. Debug builds (default) carry the Tier-1 bounds check; `--release` omits it. |
| `marv verify [--def NAME] <file>` | Discharge `requires`/`ensures` contracts via SMT: `proved` / `failed` (with a counterexample) / `unsupported` (→ runtime fallback). |
| `marv commit [--store DIR] <file>` | Freeze definitions into the content-addressed store; report the lockfile delta (new vs. already-reviewed). |
| `marv store audit/gc [--store DIR]` | Inspect provenance/reachability or remove blobs unreachable from the lockfile. |

Both `.mv` source and `*.core.json` Core-IR snapshots are accepted. See
[`docs/cli.md`](docs/cli.md) for full details and exit codes.

### The generate → check → repair loop

The toolchain is built for this loop (`spec/03` §5): an agent owns an in-memory snapshot,
`check`s it, applies the highest-confidence **fix** a diagnostic carries (or regenerates the
offending definition), formats, `verify`s the verified-subset definitions, builds/runs with a
chosen capability grant, and `commit`s — freezing reproducible hashes and skipping re-audit of
code whose hash was already reviewed.

---

## Using marv from an agent

marv is designed to be driven by LLMs/agents. Start with **[`docs/agents.md`](docs/agents.md)**
— how to drive the toolchain: the generate→check→repair loop, the CLI commands, the capability
model, and the invariants. The repo's agent-instruction files ([`AGENTS.md`](AGENTS.md) for
Codex/Cursor, [`CLAUDE.md`](CLAUDE.md) for Claude Code) carry the contributor rules and point
there. For tool-call access, the **MCP server**
([`crates/marv-mcp`](crates/marv-mcp)) exposes the JSON-RPC protocol methods as MCP tools;
see [`docs/agents.md`](docs/agents.md) for wiring it into Claude Code, Codex, and other MCP
clients, plus the bundled Claude Code skill.

---

## Repository layout

```
crates/
  marv-syntax/       lexer, recursive-descent parser, AST, canonical formatter
  marv-core/         Core IR (ANF + de Bruijn), lowering, blake3 content hashing
  marv-types/        type / effect / capability / error-set / reference / linearity checker
  marv-db/           salsa incremental query database (the protocol's backbone)
  marv-verify/       SMT contract discharge (z3 via easy-smt) for the verified subset
  marv-codegen-cl/   Cranelift backend (native, JIT today)
  marv-codegen-llvm/ LLVM backend (stub — release builds, future)
  marv-codegen-wasm/ WebAssembly backend (capabilities as host imports)
  marv-interp/       tree-walking interpreter (the semantics oracle)
  marv-store/        content-addressed store + lockfile (Merkle DAG, free renames, dedup)
  marv-server/       JSON-RPC agent-protocol server (wraps marv-db queries)
  marv-mcp/          MCP server exposing the protocol to agents
  marv-cli/          the `marv` command-line front end
std/                 the standard prelude, written in marv
selfhost/            Stage-1 self-hosting (compiler passes ported to marv)
examples/            illustrative .mv programs (kept in canonical form)
web/                 the WebAssembly capability-sandbox browser demo
spec/                normative design specs (read these first)
docs/                human-facing toolchain documentation
tests/               repository-level golden / round-trip / differential fixtures
```

## Documentation

- **Specs (normative):** [`spec/01`](spec/01-design-spec.md) (design), [`spec/02`](spec/02-grammar-and-core-ir.md) (grammar + Core IR), [`spec/03`](spec/03-compiler-protocol.md) (agent protocol). Read in order.
- **Toolchain docs:** [`docs/`](docs/) — [CLI](docs/cli.md), [language reference](docs/language-reference.md), [standard library](docs/stdlib.md), [platform support](docs/platform-support.md), [checker](docs/checker.md), [core IR](docs/core-ir.md), [query server](docs/query-server.md), [run & codegen](docs/run-and-codegen.md), [verification](docs/verification.md), [store](docs/store.md), [agents](docs/agents.md).

## Status & roadmap

Stage-0 milestones **M0–M7 are complete**. The language *surface* the parser accepts is a
deliberate growing subset (today: `fn`/`struct`, `enum`/`match`, `error`/`!T`/`?` error
handling, `interface`/`impl` + generics, **capabilities & `perform` from source**
(`io.fs()` narrowing, `out.write(...)` → `Perform`, inferred effect rows checked against the
capability parameters), struct literals + indexing + assignment, `char` literals + `as` casts +
`len`, string concat/slice/index/iteration/building, `std.collections.List[T]` with
explicit-`Alloc` growable operations, `while`/`for` loops,
`let`/`var`, `if`/`else`, arithmetic/boolean ops, the prefix unary
operators (`-e`, `not e`, `&e`/`&mut e`), calls/recursion,
`pure` + `requires`/`ensures` contracts). The next application-language wave is tracked by
MARV-48: project/package/module discovery beyond the special-cased `std` loader, richer
collections and literals, bytes/UTF-8, JSON, HTTP/server capabilities, structured concurrency,
`unsafe`/FFI auditability, and deeper verification. The content store already supports
lockfile-pinned builds by hash; the remaining module work is the developer-facing source/package
discovery layer above that store. The full backlog (surface growth → backend breadth →
verification breadth → application runtime → AOT/LLVM → self-hosting) — with phases, ordering,
and the dependency graph — is in
[`docs/roadmap.md`](docs/roadmap.md), mapped to the `MARV-#` tasks in the project tracker.

## Contributing

- `make fmt-check && make clippy && make test` must pass (CI enforces this; the toolchain is
  pinned in `rust-toolchain.toml`).
- `examples/`, `tests/`, and `docs/` are first-class — update them in the same change that
  alters observable behavior. See [`CLAUDE.md`](CLAUDE.md) for the engineering invariants.
- Maintainers: see [`MAINTAINERS.md`](MAINTAINERS.md).

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your
option.
