# Using marv from LLMs & agents

marv is built to be **authored by coding agents and audited by humans** (`spec/03` is an
agent-facing protocol). This is the reference for how to *drive the toolchain* effectively as
an agent. If you are a human, it's also the shortest path to "how do I drive this thing"; see
[`README.md`](../README.md) and the rest of [`docs/`](.) for depth.

> Looking for the *working rules for contributing to this repo* (branch hygiene, invariants
> you must not break, where knowledge lives)? Those live in [`CLAUDE.md`](../CLAUDE.md) — the
> canonical contributor-instructions file — with [`AGENTS.md`](../AGENTS.md) pointing to it for
> Codex/Cursor and other harnesses. This document is about *using the language*, not editing
> the compiler.

## The mental model

- The compiler is a **service**, not just a CLI: a salsa-backed incremental query engine
  behind a JSON-RPC protocol (`spec/03`). Hold a **snapshot** (a set of files), and every
  query (`check`, `signature`, `errorSet`, `effects`, `core`, …) is cheap to re-run after an
  edit.
- **Diagnostics carry fixes.** Most type/effect/error errors come back with a machine-
  applicable `Fix` (edits + a confidence). Prefer applying a high-confidence fix over
  regenerating.
- **Identity is the content hash**, not the name. Renames are free; identical code dedups;
  `commit` freezes reproducible hashes and lets you skip re-auditing code whose hash was
  already reviewed.

## Integration points

Use as many as your harness supports:

1. **Repo instruction files** — [`AGENTS.md`](../AGENTS.md) (read by Codex, Cursor, and other
   harnesses that honor it) and [`CLAUDE.md`](../CLAUDE.md) (Claude Code). These orient an
   agent the moment it opens the repo and point at this document.
2. **MCP server** (`marv-mcp`) — the JSON-RPC protocol as MCP tools, for tool-call access with
   a persistent snapshot. Setup below.
3. **Claude Code skill** ([`.claude/skills/marv/`](../.claude/skills/marv/SKILL.md)) — packages
   the loop for Claude Code; auto-available when this repo is open.
4. **CLI** — `marv fmt|check|run|build|verify|commit` ([`cli.md`](cli.md)), for one-shot/scripted use.

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

Both `.mv` source and `*.core.json` Core-IR snapshots are accepted. Enums and capability
`perform`/narrowing are now expressible directly in `.mv` source (MARV-1, MARV-6); a source
file that `import std.*` is resolved against the `std/` directory automatically. The
`*.core.json` path remains for hand-authoring Core IR directly.

## The MCP server

`marv-mcp` speaks MCP over stdio and forwards each tool to a `marv/*` protocol method, holding
one in-process compiler/server for the session (so a snapshot opened by `marv_open_snapshot`
persists across subsequent queries).

Build it:

```sh
cargo build --release -p marv-mcp     # → target/release/marv-mcp
```

Tools exposed: `marv_open_snapshot`, `marv_check`, `marv_signature`, `marv_error_set`,
`marv_effects`, `marv_callers`, `marv_callees`, `marv_canonical`, `marv_core`, `marv_hash`,
`marv_type_at`, `marv_apply_fix`, `marv_format`, `marv_verify`, `marv_commit`. Each tool's
`arguments` are the method's params (e.g. `marv_open_snapshot` takes `{ files: [{path, text}] }`
and returns a `snapshotId`; subsequent tools take `{ snapshotId, def }`).

### Claude Code

```sh
claude mcp add marv -- /absolute/path/to/target/release/marv-mcp
```

or add to a project `.mcp.json`:

```json
{
  "mcpServers": {
    "marv": { "command": "/absolute/path/to/target/release/marv-mcp", "args": [] }
  }
}
```

### Codex

Register the same binary as an MCP server in `~/.codex/config.toml`:

```toml
[mcp_servers.marv]
command = "/absolute/path/to/target/release/marv-mcp"
args = []
```

Codex also reads `AGENTS.md` natively, so the repo instructions apply with no extra setup.

### Any MCP client

The server is a standard MCP stdio server (`initialize` → `tools/list` → `tools/call`). Point
your client's MCP config at the `marv-mcp` binary.

## A minimal session (what the agent should do)

```
marv_open_snapshot { files: [{ path: "demo.mv", text: "<generated source>" }] }   → { snapshotId }
marv_check         { snapshotId }                          → diagnostics (+ fixes)
# apply a high-confidence fix, or regenerate the offending def; re-check until clean
marv_format        { snapshotId }                          → canonical snapshot
marv_verify        { snapshotId, def: "demo.f" }           → proved | failed(+counterexample) | unsupported
marv_commit        { snapshotId }                          → lockfile delta (new vs already-reviewed)
```

Then build/run via the CLI (`marv build`, `marv run --grant …`) for native/WASM artifacts and
capability-scoped execution.

## Capabilities — the rule you must respect

There is **no ambient authority**. A function can only do what its parameters let it: power
enters through **capability** parameters (`Io`, `Fs`, `Net`, `Clock`, `Rand`, `Alloc`) and its
effect row records them. When you write or run code:

- Pass a function only the capabilities it needs. A function with no `Net` parameter provably
  cannot reach the network; with no `Alloc`, it cannot perform user-visible growable allocation
  (compiler-managed fixed-shape boxing is a runtime representation detail).
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

The parser currently accepts: `mod`/`import`, `struct`/`fn` (incl. `pure fn`),
`enum`/`match` (payload-binding patterns + `_`), `error`/`!T`/`?` error handling, struct
literals + index reads + assignment (`lvalue = e`, `var`), `while`/`for` loops with
`invariant`, `let`/`var`, `if`/`else`, the binary operators, the prefix unary operators
(`-e`, `not e`, `&e`/`&mut e`), calls/recursion, field
projection, generic parameter lists + type arguments, `interface`/`impl` with
monomorphization, **capabilities & `perform` from source** (capability method calls →
`Perform`, `io.fs()` narrowing, inferred-and-checked effect rows), and `requires`/`ensures`
contracts. Collection literals, `linear` capabilities, richer package metadata, and package-aware
read queries are the remaining surface roadmap. Local source imports are discoverable by the CLI,
and source-only JSON-RPC snapshots can be checked as a module set; for anything not yet expressible,
construct a `*.core.json` snapshot (see [`store.md`](store.md) and the `tests/run/*.core.json` fixtures) or check
[`roadmap.md`](roadmap.md).

See [`../spec/03-compiler-protocol.md`](../spec/03-compiler-protocol.md) for the full protocol
method catalog and worked examples.
