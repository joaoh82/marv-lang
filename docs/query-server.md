# The query server: incremental queries and the agent protocol

> **The compiler is a service first, a CLI second.** It is a demand-driven,
> incremental query engine (`salsa`) wrapped in a JSON-RPC server. An agent edits
> a snapshot, asks structured questions, and receives structured answers —
> including diagnostics that carry ready-to-apply fixes.
> — `spec/03-compiler-protocol.md` §1

This is the interface an agent drives the compiler through: open an in-memory
workspace, ask `check`/`signature`/`core`/…, apply the fixes the diagnostics
carry, and loop. The normative description is
[`../spec/03-compiler-protocol.md`](../spec/03-compiler-protocol.md).

## Where it lives

- `marv_db` — the **incremental query engine** (`salsa`). [`SourceFile`] is the
  one input (path, kind, text); [`analyze`] is the one memoized query, running
  the full `parse → lower → typecheck → effects/errors` pipeline over a file and
  returning a flat [`FileAnalysis`]. salsa re-executes it only when that file's
  text changes — so editing one file never recomputes the others.
- `marv_db::corespec` — **Core-IR snapshot ingestion** (`spec/03` §3.1). A file
  may be marv source *or* a Core module in JSON; see "Driving the capability
  rules" below for why.
- `marv_server` — the **JSON-RPC 2.0 server**: snapshot management, the method
  catalog, and the wire shapes of `spec/03` §4. Transport is line-delimited JSON
  over any reader/writer (`serve`), with stdio as the default in the
  `marv-server` binary.

## Current status: M3 (query server)

### Incremental queries

Each compiler phase is reachable through one memoized salsa query keyed by file
content. The defining property — *an edit recomputes only its dependents* — is
asserted directly: `marv_db::ANALYZE_RUNS` counts real query executions, and
`crates/marv-db/tests/incremental.rs` proves that re-asking with no edit is free,
and editing file A re-runs A's analysis but not B's.

The grain today is the **file**: any edit re-analyzes the whole file. Per-
*definition* incrementality (a salsa tracked struct per def) is a later
refinement; the file grain already delivers the milestone's property and keeps
the phase crates (which define `Diagnostic`/`Core`/`LoweredModule`) free of a
salsa dependency.

### Snapshots

A snapshot is a set of `SourceFile` inputs. `openSnapshot` creates one and
returns an id (`s1`, `s2`, …). `applyEdits`, `applyFix`, and `format` return a
*new* snapshot id, reusing the input handles of unchanged files so salsa's
memoization carries across snapshots. `closeSnapshot` discards one.

### Method catalog

| Method | Returns |
|--------|---------|
| `marv/openSnapshot` | `{ snapshotId }` from a set of `{path, text}` (source) or `{path, core}` (Core IR) files. |
| `marv/applyEdits` | A new `{ snapshotId }` after whole-file replacements and/or byte-range `edits`. |
| `marv/closeSnapshot` | `{ closed }`. |
| `marv/check` | `{ diagnostics }` for the snapshot, optionally scoped to a `def` or `file`. |
| `marv/typeAt` | `{ def, type, effects, span }` for the definition enclosing a byte offset (`span` is the def's real header span over source, `null` over Core). |
| `marv/signature` | `{ name, params, ret, effects, errorSet, pure, requires, ensures, hash }`. |
| `marv/effects` | `{ effects }` — the **inferred** capability row. |
| `marv/errorSet` | `{ errorSet }` — the **inferred** error set. |
| `marv/callers` / `marv/callees` | `{ callers }` / `{ callees }` — call edges, by qualified name. |
| `marv/canonical` | `{ text }` — the canonical form of a `def` or `file`. |
| `marv/core` | `{ hash, core, deps, alphaCanonical }` — the Core IR and content identity (`spec/03` §4.4). |
| `marv/hash` | `{ hash }` — the content hash alone. |
| `marv/applyFix` | A new `{ snapshotId, diagnostics }` after applying a diagnostic's repair. |
| `marv/format` | A new `{ snapshotId, files }` with every file normalized to canonical form. |

Verification and build/run (`verify`/`build`/`run`/`commit`) belong to later
milestones and report JSON-RPC *method not found* (`-32601`).

### The wire form is serde over the Core IR

`marv/core` emits the Core IR as serde's default externally-tagged JSON — exactly
the `spec/03` §4.4 shape: a struct variant `Lam` is `{"Lam": { … }}`, a newtype
variant `Var(0)` is `{"Var": 0}`, a unit variant `I32` is `"I32"`, and a content
hash is the string `"b3:<hex>"`. The same derive deserializes an ingested Core
module, so the round trip is symmetric.

### Acceptance gate (met)

`crates/marv-server/tests/protocol.rs` drives the server with the worked-example
requests of `spec/03` §4 and asserts the responses: the **missing-`Fs` fix flow**
(`check` → fix-carrying diagnostic → `applyFix` → clean), **`signature`**, and
**`core` + `hash`** (including the M1 alpha-equivalence identity surfaced over the
protocol). The stdio NDJSON transport and JSON-RPC error envelope are covered too.

## Scope honesty

Three boundaries are worth stating plainly, continuing the M1/M2 notes:

1. **Spans are definition-granular (MARV-12).** Over `.mv` source, every
   diagnostic, `typeAt`, and `verify` carries a **real** span — the byte range and
   `{line, col}` of the enclosing definition's header — threaded from the lexer
   through the parser's `ItemSpan` side table (the AST and Core hashing are
   untouched). A `MissingCapability` fix resolves its edit to the parameter-list
   insertion point, so `applyFix` lands at a real offset. What is *not* yet
   exact is sub-definition granularity: `spec/02` §F rule 4 keeps spans out of the
   span-free Core IR the checker runs over, so a diagnostic points at its def's
   header rather than the offending sub-expression (that finer grain needs a
   Core→source map). Core-ingested files have no source text, so their spans stay
   `null`. The `code`, `message`, and each fix's `newText` are always present.

2. **Driving the capability rules needs Core ingestion.** The M0 front end emits
   no `perform`/`raise`/enum/`linear` forms, so a capability or error-set misuse
   cannot be *written* in `.mv` source yet (see `checker.md`). Because the
   protocol is agent-facing and agents hold Core directly, a snapshot file may be
   ingested as a Core module (`spec/03` §3.1). This is what lets the spec's
   missing-`Fs` example run end to end through the *real* checker. The same
   `applyFix` over a Core file makes the declaration honest — it sets the
   definition's declared effect row to what its body actually exercises
   (`marv_types::effect_row`), the structured equivalent of inserting `fs: Fs`
   into a signature.

3. **The prose example's `E0307` is `E0110`.** `spec/03` §6 fixes the real error
   numbering by check family (capabilities are `E011x`); the `E0307` in the §4.1
   prose is an older illustrative number. The tests assert the real, stable code.
