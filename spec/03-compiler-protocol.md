# marv — Agent-Facing Compiler Protocol

Status: draft 0.1 · Companion to `01-design-spec.md` and `02-grammar-and-core-ir.md`

The marv compiler is built **as a service for an agent first**, and a CLI second. It is a
demand-driven, incremental query engine (`salsa`) wrapped in a JSON-RPC server
(`marv-server`). An agent edits a snapshot, asks structured questions, and receives
structured answers — including diagnostics that carry ready-to-apply fixes. This document
is the interface contract.

---

## 1. Architecture

```
            ┌──────────────────────── marv-server (JSON-RPC) ────────────────────────┐
agent  <->  │  request/response + notifications                                       │
            │      │                                                                   │
            │      v                                                                   │
            │   marv-db (salsa)  ── incremental queries, cached & invalidated by edit ─┤
            │      │  parse → lower(Core) → typecheck → effects/errors → verify        │
            │      v                                                                   │
            │   content-addressed store (marv-store)                                   │
            └───────────────────────────────────────────────────────────────────────┘
```

- **Snapshots, not files-on-disk.** The agent owns an in-memory workspace; it sends edits as
  text or AST patches. Every query is answered against the current snapshot. Disk is optional.
- **Incremental.** Because each phase is a salsa query keyed by content, an edit to one
  function recomputes only what depends on it. Queries are cheap to call in a tight loop.
- **Deterministic.** Same snapshot ⇒ same answers, same hashes, same diagnostics ordering.

Transport: JSON-RPC 2.0 over stdio (default) or a local socket. All positions are
**UTF-8 byte offsets** *and* `{line, col}` (0-based) to suit both text-diffing agents and
humans. Definitions are addressable by `name` (path) or by content `hash`.

---

## 2. Core types (shared shapes)

```jsonc
// A source range.
"Span": { "file": "report.mv", "startByte": 142, "endByte": 168,
          "start": {"line": 9, "col": 4}, "end": {"line": 9, "col": 30} }

// A text edit (for fixes and patches).
"Edit": { "span": Span, "newText": "fs: Fs, " }

// A machine-actionable fix attached to a diagnostic.
"Fix": { "title": "add missing capability parameter `fs: Fs`",
         "edits": [ Edit, ... ], "confidence": 0.93 }

// A diagnostic.
"Diagnostic": {
  "code": "E0307",                       // stable, documented error code
  "severity": "error" | "warning" | "info",
  "span": Span,
  "message": "function uses `Fs` but its signature has no `Fs` capability",
  "related": [ { "span": Span, "message": "`Fs` first required here" } ],
  "fixes": [ Fix, ... ]                   // ordered best-first; may be empty
}
```

**Every diagnostic is expected to carry at least one fix where one is mechanically
derivable** (missing capability, missing error in set, non-exhaustive match, unconsumed
linear value, escaping reference). This is the heart of the agent loop: the compiler does not
merely reject — it proposes the repair.

---

## 3. Method catalog

### 3.1 Workspace

| Method | Purpose |
|---|---|
| `marv/openSnapshot` | Create a workspace from a set of `{path, text}` files. Returns a `snapshotId`. |
| `marv/applyEdits` | Apply `Edit[]` (or whole-file replacements) to a snapshot; returns new `snapshotId`. |
| `marv/closeSnapshot` | Discard a snapshot. |

### 3.2 Checking & understanding (read-only queries)

| Method | Returns |
|---|---|
| `marv/check` | All `Diagnostic[]` for the snapshot (or a file/def subset). |
| `marv/typeAt` | The `Type` and effect row at a byte offset. |
| `marv/signature` | The full signature of a def: params, return, effect row, error set, contracts. |
| `marv/errorSet` | The inferred error set of a function (the payloads of `!T`). |
| `marv/effects` | The capabilities a function requires (its effect row). |
| `marv/callers` / `marv/callees` | Incoming / outgoing call edges for a def. |
| `marv/resolveImpl` | Which interface `impl` a generic call site selected. |
| `marv/canonical` | The canonical-formatted text of a snippet/def (the formatter as a query). |
| `marv/core` | The Core IR (`02` §C) for a def, plus its content `hash`. |
| `marv/hash` | The content hash of a def (without the full Core). |
| `marv/unsafeSites` | All `unsafe` blocks/functions and their `SAFETY:` notes (audit). |

### 3.3 Verification

| Method | Returns |
|---|---|
| `marv/verify` | Discharge a def's contracts via SMT (Tier 2). Returns `proved`, or `failed` with a **counterexample** assignment, or `unsupported` (outside the verified subset → falls back to runtime checks). |

### 3.4 Mutation (agent acts on results)

| Method | Purpose |
|---|---|
| `marv/applyFix` | Apply a `Fix` returned by a diagnostic; returns the new snapshot + re-check. |
| `marv/format` | Normalize the whole snapshot to canonical form. |
| `marv/commit` | Freeze defs into the content-addressed store; returns the lockfile delta (hashes). |

### 3.5 Build

| Method | Purpose |
|---|---|
| `marv/build` | Compile a target. `{ target: "native-cranelift" \| "native-llvm" \| "wasm-component", optimize }`. Returns artifact path/bytes + diagnostics. |
| `marv/run` | Build (or interpret via `marv-interp`) and run an entry point with an explicit set of host-provided capabilities; returns exit status + captured effects. |

---

## 4. Worked examples

### 4.1 `check` surfaces a missing capability *with the fix*

Request — the agent wrote a function that reads a file but forgot the `Fs` parameter:

```jsonc
{ "jsonrpc": "2.0", "id": 1, "method": "marv/check",
  "params": { "snapshotId": "s1", "scope": { "def": "report.load" } } }
```

Response:

```jsonc
{ "jsonrpc": "2.0", "id": 1, "result": {
  "diagnostics": [{
    "code": "E0307",
    "severity": "error",
    "span": { "file": "report.mv", "startByte": 88, "endByte": 96,
              "start": {"line": 5, "col": 12}, "end": {"line": 5, "col": 20} },
    "message": "`fs.read(path)` requires capability `Fs`, but `load` declares no `Fs`",
    "related": [{ "span": { "file":"report.mv","startByte":60,"endByte":83,
                            "start":{"line":4,"col":0},"end":{"line":4,"col":23} },
                  "message": "signature of `load` is here" }],
    "fixes": [{
      "title": "add capability parameter `fs: Fs`",
      "confidence": 0.94,
      "edits": [{ "span": { "file":"report.mv","startByte":72,"endByte":72,
                            "start":{"line":4,"col":12},"end":{"line":4,"col":12} },
                  "newText": "fs: Fs, " }]
    }]
  }]
}}
```

The agent applies it directly:

```jsonc
{ "jsonrpc": "2.0", "id": 2, "method": "marv/applyFix",
  "params": { "snapshotId": "s1", "diagnosticCode": "E0307",
              "span": { "startByte": 88, "endByte": 96 } } }
```

Response: `{ "result": { "snapshotId": "s2", "diagnostics": [] } }` — clean.

### 4.2 `signature` — everything an auditor/agent needs without the body

```jsonc
// -> request: marv/signature { snapshotId:"s2", def:"report.load" }
{ "result": {
  "name": "report.load",
  "params": [ {"name":"fs","type":"Fs"}, {"name":"path","type":"str"} ],
  "ret": "Config",
  "effects": ["Fs"],                         // capabilities it may exercise
  "errorSet": ["FileError", "ParseError"],   // inferred complete failure surface
  "pure": false,
  "requires": [],
  "ensures": [],
  "hash": "b3:9f2c1a…"                        // content identity
}}
```

### 4.3 `verify` — proof with a counterexample to iterate against

The agent wrote `clamp` but with a wrong postcondition (`result <= hi` only).

```jsonc
// -> request: marv/verify { snapshotId:"s3", def:"math.clamp" }
{ "result": {
  "status": "failed",
  "obligation": "ensures result >= lo",
  "counterexample": { "x": -5, "lo": 0, "hi": 10, "result": -5 },
  "message": "postcondition `result >= lo` violated when x < lo and the else-branch is taken",
  "relatedSpan": { "file":"math.mv","startByte":210,"endByte":248,
                   "start":{"line":12,"col":4},"end":{"line":12,"col":42} }
}}
```

The agent now has a concrete failing input (`x=-5, lo=0`) and the offending branch — it
repairs the body, re-runs `marv/verify`, and gets:

```jsonc
{ "result": { "status": "proved", "tier": 2, "solverMs": 38 } }
```

If a function falls outside the verified subset, `verify` returns
`{ "status": "unsupported", "reason": "non-linear arithmetic over f64",
"fallback": "runtime-checked (Tier 1)" }` — honest about the boundary.

### 4.4 `core` + `hash` — content identity and dedup

```jsonc
// -> request: marv/core { snapshotId:"s2", def:"math.clamp" }
{ "result": {
  "hash": "b3:7a55e0…",
  "core": { "Lam": { "param": {"Int":"I32"}, "effects": {"caps":[],"errors":[]},
                     "body": { "Match": { "scrutinee": {"Var":0}, "branches": [ /* … */ ] } } } },
  "deps": [ "b3:11ab…" ],         // Globals referenced (Merkle DAG edges)
  "alphaCanonical": true          // names already erased; identical logic ⇒ identical hash
}}
```

An agent can call `marv/hash` on a freshly generated function and check the store: if the
hash already exists and was previously reviewed, the code needs no re-audit.

### 4.5 `run` — capabilities are injected explicitly (the sandbox)

```jsonc
// -> request:
{ "method": "marv/run", "params": {
    "snapshotId": "s2", "entry": "report.main",
    "grant": ["Fs"],                  // host grants ONLY filesystem; no Net, no Clock
    "args": ["./data.csv"] } }
// -> result:
{ "result": { "exit": 0, "effects": [ {"cap":"Fs","op":"read","arg":"./data.csv"} ],
              "stdout": "total: 41250\n" } }
```

If `report.main` had tried to use `Net`, the build would have failed at `check` time
(missing `Net` in its effect row) — it can never reach runtime, and the host never has to
trust it. This is the same property that makes browser execution safe: grant the WASM
component only the capability imports you choose.

---

## 5. The generate → check → repair loop (recommended agent algorithm)

```
1. openSnapshot(files)
2. loop:
     diags = check(snapshot)
     if diags is empty: break
     pick d = highest-severity, then highest-confidence-fix diagnostic
     if d has a fix with confidence >= 0.8:
         snapshot = applyFix(snapshot, d)        // cheap, incremental
     else:
         regenerate the offending def using d.message + d.related as guidance
3. format(snapshot)                              // canonical form
4. for each `pure`/verified-subset def: verify(def)
       on "failed": use counterexample to repair, re-verify
5. build(target) ; run(entry, grant=[…])         // capability-scoped
6. commit(snapshot)                              // freeze hashes into the store + lockfile
```

The properties of marv that make this loop fast and reliable:

- **Fix-carrying diagnostics** turn most type/effect/error errors into a one-call repair.
- **Incrementality** keeps `check` and `verify` cheap to call after every edit.
- **Counterexamples** turn proof failures into concrete, debuggable inputs.
- **Canonical form** means the agent never argues with itself about style and diffs stay
  minimal and reviewable.
- **Content hashing** lets the agent skip re-auditing code that already exists, and lets the
  human auditor focus on `unsafeSites` + new hashes + capability/effect signatures.

---

## 6. Error code stability

Error codes (`E0001`…) are stable and documented; each maps to a help page describing the
rule, a minimal failing example, and the canonical fix. Agents may key behavior on codes; the
*messages* may improve over time but codes do not change meaning. A machine-readable index
(`marv/errorCodes`) returns the full catalog for tooling and prompt-priming.
