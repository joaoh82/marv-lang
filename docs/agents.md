# Using marv from LLMs & agents

marv is designed to be authored by coding agents (`spec/03` is an agent-facing protocol).
There are four complementary integration points; use as many as your harness supports.

1. **[`AGENTS.md`](../AGENTS.md)** (root) — cross-tool instructions read by Claude Code,
   Codex, Cursor, and others. The loop, the commands, the capability model, the invariants.
   Always in effect when the agent opens the repo.
2. **MCP server** (`marv-mcp`) — the JSON-RPC protocol as MCP tools, for tool-call access with
   a persistent snapshot. Setup below.
3. **Claude Code skill** ([`.claude/skills/marv/`](../.claude/skills/marv/SKILL.md)) — packages
   the loop for Claude Code; auto-available when this repo is open.
4. **CLI** — `marv fmt|check|run|build|verify|commit` ([`cli.md`](cli.md)), for one-shot/scripted use.

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

Codex also reads `AGENTS.md` natively, so the loop instructions apply with no extra setup.

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

See [`AGENTS.md`](../AGENTS.md) for the invariants and [`../spec/03-compiler-protocol.md`](../spec/03-compiler-protocol.md)
for the full method catalog and worked examples.
