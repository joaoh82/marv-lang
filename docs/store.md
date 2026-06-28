# Content-addressed store, lockfile, and reuse (M7)

marv's unit of identity is the **content hash of a definition's Core IR**, not
its name or path (`spec/01` §8, `spec/02` §F). `marv-store` turns that into the
reuse properties the design promises, and `marv commit` (`marv/commit`,
`spec/03` §3.4) is how an agent freezes work into it.

## The dag hash (the Merkle DAG)

M1 hashing makes a definition's hash depend on the *names* of the things it
calls (cross-references were `symbol_hash(name)`, a deliberate M1 shortcut). The
store finishes the job: it computes a **dag hash** for each definition with its
references resolved to *their* dag hashes (transitively), so a hash commits to
the whole dependency graph and depends on **no names at all**:

- A reference to another definition becomes that definition's dag hash.
- A recursive or mutually-recursive reference (a cycle) becomes a **positional
  placeholder** within its strongly-connected component; the component is hashed
  as a unit and each member's hash derives from the component hash plus its
  position (the Unison approach).

Two consequences fall straight out:

- **Free renames.** Renaming anything — a function, a callee, even a recursive
  function — changes no hash, because names never enter a dag hash.
- **Transitive dedup.** Structurally identical definitions (with identical
  dependencies) collapse to one hash regardless of how anything is named.

## The store and lockfile

- The **store** maps `dag hash → definition` (the name-free Merkle node, plus
  its dependency edges, declaration metadata, and a `reviewed` flag). Identical
  definitions dedup to one entry. Two libraries pinning *different* hashes of
  the "same"-named function are just two keys — they coexist with no conflict
  (no dependency hell).
- The **lockfile** maps `name → dag hash` — names are *labels over hashes*. It
  pins a build to an exact set of hashes (reproducibility); rebinding a name (a
  rename, or a version bump) never disturbs the stored definitions.

On disk, the lockfile is deterministic JSON at `.marv/lockfile.json`, while
definitions are individual content-addressed blobs under `.marv/blobs/b3/`.
The loader still understands the original `.marv/store.json` monolith as a
compatibility/import path, but new writes use one blob per dag hash.

## `marv commit`

```
marv commit [--store DIR] <file>
```

Checks the file (refusing to freeze code that does not check), then freezes each
definition into the store and rebinds its name in the lockfile. It reports the
delta — what is **new** versus **already in the store / already reviewed** — so
an agent (or a human auditor) can ask the provenance question "has this exact
hash been reviewed before?" (`spec/01` §8). Committing is **idempotent**:

```sh
$ marv commit --store .marv examples/clamp.mv
  + math.clamp  b3:d94fc7d0b1b0…  (new — frozen & reviewed)
$ marv commit --store .marv examples/clamp.mv
  = math.clamp  b3:d94fc7d0b1b0…  (already in store — already reviewed)
$ marv fmt --write renamed.mv   # clamp → bound everywhere, then:
$ marv commit --store .marv renamed.mv
  = math.bound  b3:d94fc7d0b1b0…  (already in store — already reviewed)   # same hash!
```

The same logic is exposed as `marv/commit` (`spec/03` §3.4), returning
`{ committed: [{name, hash, status, reviewed}], added, alreadyReviewed, rebound,
storeSize }`.

## Pinned builds

```
marv build --store .marv --run app.mv --entry main
marv run --store .marv app.mv
```

With `--store`, the CLI resolves the freshly-lowered module against the
lockfile's `name → dag hash` bindings, rewrites known imports and local edges to
dag hashes, fetches the full transitive dependency closure through each stored
blob's `deps`, and hands the interpreter/backend a hash-keyed program. Missing
dependency blobs are hard errors: a stored build never falls back to whatever
source happens to be on disk.

The source loader still resolves `std/` modules from source so imported std
declarations are available to the checker and lowerer. Pinned builds use the
store for hash linking; broader project/package/module source discovery beyond
the special-cased `std` loader is tracked separately as MARV-49.

## Audit and GC

```
marv store audit [--store .marv]
marv store gc [--store .marv]
```

`audit` prints each blob's reviewed flag, lockfile reachability, dependency
count, and unsafe-site count. The unsafe-site list is currently empty because
`unsafe fn` remains spec-only surface, but the view is wired so the metadata can
land there when the parser/checker grow it. `gc` removes blobs unreachable from
every current lockfile binding and rewrites the blob directory.

## Self-hosting

Content-addressed reuse is also the substrate for **Stage-1 self-hosting**: the
compiler, rewritten in marv, linked and pinned by hash, with the Rust Stage-0
compiler kept as a differential oracle. The first step lives in
[`../selfhost/`](../selfhost) — the interpreter's primitive kernel ported to
marv and proven equivalent to Stage 0 on the M4 corpus. The standard prelude
those programs link against is in [`../std/`](../std).
