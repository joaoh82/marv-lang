# AGENTS.md — driving marv as an LLM/agent

marv is built to be authored by coding agents and audited by humans. This file tells an agent
how to *use* the toolchain effectively. It is read by Claude Code, Codex, Cursor, and other
agent harnesses that honor `AGENTS.md`.

If you are a human: this is also the shortest path to "how do I drive this thing." See
[`README.md`](README.md) and [`docs/`](docs/) for depth.

## The mental model

- The compiler is a **service**, not just a CLI: a salsa-backed incremental query engine
  behind a JSON-RPC protocol (`spec/03`). Hold a **snapshot** (a set of files), and every
  query (`check`, `signature`, `errorSet`, `effects`, `core`, …) is cheap to re-run after an
  edit. Two ways to call it:
  - **CLI** (`marv …`) — one-shot, file-based. Best for scripts and quick checks.
  - **MCP server** (`marv-mcp`) — tool-call access to the protocol with a persistent snapshot,
    for agents in an MCP-capable harness. See [`docs/agents.md`](docs/agents.md) to wire it up.
- **Diagnostics carry fixes.** Most type/effect/error errors come back with a machine-
  applicable `Fix` (edits + a confidence). Prefer applying a high-confidence fix over
  regenerating.
- **Identity is the content hash**, not the name. Renames are free; identical code dedups;
  `commit` freezes reproducible hashes and lets you skip re-auditing code whose hash was
  already reviewed.

## The loop you should run (generate → check → repair)

```
1. open the file(s) you generated
2. loop:
     diags = check(file)
     if no errors: break
     pick the highest-severity diagnostic
     if it has a fix with confidence ≥ 0.8: apply the fix
     else: regenerate the offending definition using the message + related notes
3. fmt (canonical form — never argue about style; there is exactly one)
4. for each pure / verified-subset def: verify(def)
       on "failed": use the counterexample to repair, then re-verify
5. build (--target native-cranelift | wasm-component) ; run with an explicit capability grant
6. commit (freeze hashes into the store + lockfile)
```

### CLI cheat-sheet

```sh
marv fmt --check <file>                        # is it canonical? (exit non-zero if not)
marv fmt --write <file>                        # rewrite to canonical form
marv check <file>                              # diagnostics (codes are stable: E0001…)
marv run <file> --entry NAME [args…]           # interpret (the reference semantics)
marv run <file> --grant Fs,Net --entry NAME    # inject ONLY these capabilities
marv build --run <file> --entry NAME [args…]   # Cranelift JIT, then execute
marv build --target wasm-component <file> -o out.wasm
marv verify <file> [--def NAME]                # SMT: proved / failed+counterexample / unsupported
marv commit <file> [--store .marv]             # freeze into the content-addressed store
```

Both `.mv` source and `*.core.json` Core-IR snapshots are accepted (the latter is currently
the only way to express capability `perform` and enums, until that surface lands).

## Capabilities — the rule you must respect

There is **no ambient authority**. A function can only do what its parameters let it: power
enters through **capability** parameters (`Io`, `Fs`, `Net`, `Clock`, `Rand`, `Alloc`) and its
effect row records them. When you write or run code:

- Pass a function only the capabilities it needs. A function with no `Net` parameter provably
  cannot reach the network; with no `Alloc`, it provably does not allocate.
- `marv run --grant …` injects exactly the listed capabilities — nothing else exists at
  runtime. On WebAssembly, capabilities are host imports the embedder chooses to supply.
- You cannot construct a capability; you receive one and may narrow it (`io.fs()`).

## Invariants to honor (do not fight these)

1. **One canonical form.** Don't hand-format; run `fmt`. The formatter is the parser's inverse.
2. **No hidden control flow / allocation.** Every effect is visible at the call site.
3. **No ambient authority.** See above.
4. **Local reasoning.** Every signature is fully annotated; there is no cross-function
   inference. Annotate.
5. **Determinism.** Same source ⇒ same hashes, diagnostics, ordering.

Honesty matters: the verifier reports `unsupported` rather than a false `proved`; backends
report `unsupported` rather than emitting wrong code; docs mark *implemented vs. designed*.
Mirror that — don't claim a check passed that you didn't run.

## What's real today (so you don't generate what won't parse)

The parser currently accepts: `mod`/`import`, `struct`/`fn` (incl. `pure fn`), `let`/`var`,
`if`/`else`, the binary operators, calls/recursion, field projection, and
`requires`/`ensures` contracts. Enums/`match`, loops, `?` error handling, generics,
capabilities-from-source, and collection literals are on the surface roadmap — to use those
features today, construct a `*.core.json` snapshot (see [`docs/store.md`](docs/store.md) and
the `tests/run/*.core.json` fixtures) or check the roadmap in the project tracker.

See [`docs/agents.md`](docs/agents.md) for the MCP server and harness wiring, and `spec/03`
for the full protocol method catalog and worked examples.
