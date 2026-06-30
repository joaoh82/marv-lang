# Package Manifests

MARV-67 adds a minimal deterministic package workflow on top of the existing
source-module discovery and content-addressed store.

## `marv.toml`

A package root is any directory containing `marv.toml`. The first slice supports
a deliberately small TOML subset:

```toml
[package]
name = "app"
roots = ["src"]

[dependencies.util]
path = "../util"
```

- `[package].name` is required. Source modules in that package must declare a
  matching first `mod` segment, such as `mod app.main`.
- `[package].roots` is optional and defaults to `["."]`. Each root is scanned
  recursively for `.mv` files, skipping `std`, `.marv`, `.git`, and `target`.
- `[dependencies.NAME].path` declares a local path dependency. The dependency's
  own `marv.toml` must declare `[package].name = "NAME"`. Dependencies load
  transitively and deterministically.
- `std.*` imports still resolve through `MARV_STD` or the nearest repository
  `std/` directory; `std` is not listed as a package dependency.

Files are indexed by declared `mod` path, not by file path. Duplicate module
declarations are reported as ambiguous imports; missing local modules are load
errors.

## Bootstrap Path

Create one package and one local dependency:

```text
workspace/
  app/
    marv.toml
    src/main.mv        # mod app.main
  util/
    marv.toml
    src/math.mv        # mod util.math
```

`app/src/main.mv` can import `util.math`:

```marv
mod app.main
import util.math (double)

pure fn main() -> i64 {
    double(21)
}
```

Then use the normal CLI commands against the entry source file:

```sh
marv check app/src/main.mv
marv run app/src/main.mv
marv build --run app/src/main.mv
marv commit --store app/.marv app/src/main.mv
marv build --store app/.marv --run app/src/main.mv
```

The CLI discovers `app/marv.toml`, loads every module under `app`'s roots plus
the transitive local dependency packages, and lowers/checks the whole module set
together. Without a `marv.toml`, the old fallback remains: non-`std` imports are
searched from the entry file's directory.

## Lockfile Behavior

`marv commit` freezes the entire loaded package graph: the entry package's
definitions and local dependency definitions are committed together, each under
its own qualified name (`app.main.main`, `util.math.double`, ...). The lockfile
stores deterministic `name -> dag hash` bindings in the selected store
directory. Re-running the same commit is idempotent.

`marv build --store DIR` and `marv run --store DIR` still parse/check the source
package graph first, then rewrite known edges through `DIR/lockfile.json` and
fetch the transitive closure from `DIR/blobs/b3/`. Missing blobs are hard errors;
pinned builds never fall back to whatever dependency source happens to be on
disk.

## Server And MCP

JSON-RPC and MCP clients can open a package without enumerating files:

```json
{ "method": "marv/openPackage", "params": { "path": "app" } }
```

`path` may be a package root or any source file inside the package. The result
is a normal source snapshot containing the package graph, so `marv/check` can
check imports as a module set. Existing ad hoc `marv/openSnapshot` remains
unchanged for generated or in-memory files.
