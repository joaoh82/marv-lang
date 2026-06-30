# The `marv` CLI

`marv` is the command-line front end for the toolchain (crate `marv-cli`). It is
the CLI half of the compiler; the agent-facing half is the JSON-RPC service
(`marv-server`, milestone M3) described in
[`../spec/03-compiler-protocol.md`](../spec/03-compiler-protocol.md).

```
marv <command> [args]
```

## Commands

| Command | Status | Description |
|---------|--------|-------------|
| `fmt`    | **working** (M0+, parse-and-reprint for the implemented surface) | Canonicalize marv source. |
| `check`  | **working** (M2) | Type / effect / capability / error-set / reference / linearity checking. |
| `build`  | **working** (M4 `native-cranelift`, M5 `wasm-component`, MARV-14 pinned store, MARV-68 native AOT) | Compile a target: Cranelift JIT/AOT or a WebAssembly module. |
| `run`    | **working** (M4, MARV-14 pinned store) | Interpret an entry point with an explicit capability grant set. |
| `resolve-impl` | **working** (MARV-5) | Report each generic instantiation and which coherent `impl` its bounded type arguments select. |
| `verify` | **working** (M6, Tier 2) | Discharge `requires`/`ensures` contracts via SMT. |
| `commit` | **working** (M7/MARV-14) | Freeze definitions into the content-addressed store; report the lockfile delta. |
| `store audit/gc` | **working** (MARV-14) | Inspect blob provenance/reachability or collect unreachable blobs. |

`check`, `build`, `run`, `verify`, and `commit` accept either a `.mv` **source** file (parsed and
lowered through the front end) or a `*.core.json` **Core-IR snapshot**
(`marv_db::CoreModuleSpec`, `spec/03` §3.1). Capability use is now expressible in
`.mv` source: a method call on a capability value (`io.stdout().write(...)`,
`io.fs()`) lowers to a `Core::Perform` and its effect row is inferred and checked
(MARV-6). `.core.json` remains useful for hand-authoring Core directly.

**Source import discovery.** When a source file imports another module, the CLI
lowers the imported source modules alongside the entry file, transitively. `std.*`
imports are found through `MARV_STD` or the nearest `std/` ancestor. Non-`std`
imports are found under the nearest ancestor containing `marv.toml`; if there is
no manifest yet, the entry file's directory is the source root. Files are indexed
by their declared `mod` path, so file names are not semantic. Missing or
ambiguous non-`std` module declarations are load errors, not silent soft-skips.
Missing `std` modules remain opaque so not-yet-surfaced builtins such as
`std.math` can be imported without forcing a source file.

```text
workspace/
  app/
    marv.toml
    src/main.mv   # mod app.main; import util.math (double)
  util/
    marv.toml
    src/math.mv   # mod util.math; pure fn double(...)
```

`marv check app/src/main.mv`, `marv run app/src/main.mv`, `marv build --run
app/src/main.mv`, and `marv commit --store app/.marv app/src/main.mv` all operate
on the discovered package graph. Imported definitions are frozen under their own
qualified names (`util.math.double`), while the entry file keeps normal
bare-entry behavior (`main`, or `--entry app.main.main`). `build --store` /
`run --store` then use the content store for pinned hash linking. See
[`packages.md`](packages.md) for the manifest format and bootstrap path.

## `marv fmt`

```
marv fmt [--write|--check] [files...]
```

- **No file arguments** — reads stdin and writes canonical form to stdout (a
  filter you can pipe through).
- **File arguments, no flag** — prints each file's canonical form to stdout.
- **`--write`** — rewrites each file **in place**.
- **`--check`** — writes nothing; exits non-zero if any input is not already in
  canonical form. Useful in CI and pre-commit hooks. (Mutually exclusive with
  `--write`.)

Examples:

```sh
# Filter mode
printf 'mod m\nfn f(){\na+b\n}\n' | marv fmt

# Preview canonical form without touching the files
marv fmt examples/*.mv

# Format files in place
marv fmt --write examples/*.mv

# CI gate: fail if anything is unformatted
marv fmt --check examples/*.mv tests/fmt/*.out.mv
```

### What `fmt` does today

The formatter is the *inverse of the parser* — exactly one textual form per
program (non-negotiable invariant #1). It parses and reprints the implemented
language surface, including the current examples and `std/` files, preserving
`///` doc comments as item metadata outside content identity. Input that is
otherwise unparseable falls back to the parser-free **whitespace canonicalizer**
(line endings, tabs → 4 spaces, trailing-whitespace stripping, blank-line
collapsing, single trailing newline). Both paths are deterministic and
idempotent. See [`formatter.md`](formatter.md).

## `marv check`

```
marv check <file>
```

Runs the M2 checker over every definition and prints each diagnostic as
`severity[CODE] qualified.name: message`, followed by any related notes and the
mechanically-derivable fix titles the checker attached (`spec/03` §2). Exits
non-zero if any diagnostic is error severity. See [`checker.md`](checker.md) for
the rule and error-code catalog.

```sh
marv check examples/factorial.mv
marv check tests/run/uses_ungranted_cap.core.json   # reports E0110, exits 1
```

## `marv run`

```
marv run [--grant CAP,CAP] [--entry NAME] [--store DIR] <file> [args...]
```

Interprets an entry point with the tree-walking interpreter (`marv-interp`) —
the reference semantics oracle. It first runs `check` and **refuses to run** if
there are errors.

- **`--grant`** — the comma-separated capabilities the host hands to the program.
  The entry's capability parameters are filled *only* from this set; an
  ungranted capability makes the entry un-runnable (`spec/03` §4.5, the sandbox).
- **`--entry`** — which function to call. Defaults to `main`, or the sole
  function if there is exactly one.
- **`--store DIR`** — use the lockfile-pinned dependency closure from `DIR`
  before interpreting, so calls resolve by dag hash rather than source names.
- **`[args...]`** — fill the entry's non-capability value parameters, in order
  (parsed at each parameter's type).

`marv run` is the **debug runner**: Tier-1 checks (contracts and the runtime
bounds check, MARV-34) always run. `--release` is a `build` flag; passing it to
`run` prints a note and is otherwise ignored.

The entry's result is printed to stdout; any capability effects it performed are
logged to stderr as `effect: <cap> op#<n> [<args>]`.

```sh
marv run examples/factorial.mv --entry factorial 6     # prints 720
marv run examples/arithmetic.mv                         # entry defaults to main → 42
marv run --grant Io examples/hello.mv                   # logs: effect: Io op#5 / Stream op#0 ["hello, marv\n"]
marv run --grant Http examples/http_echo.mv             # prints marv-http-echo and logs Http request/response effects
marv run examples/hello.mv                              # refused: capability `Io` not granted
```

## `marv build`

```
marv build [--target T] [--run] [--release] [--store DIR] [--emit object|exe] [--out PATH] [--entry NAME] <file> [args...]
```

Compiles with the selected backend. Like `run`, it first runs `check` and
**refuses to compile** code with errors — this is where a program that uses a
capability absent from its effect row fails to build (`spec/03` §5).

Both targets compile **only the definitions reachable from the entry point**
(MARV-8) — the entry's transitive closure over the same dependency edges the
content store links (`marv-store::resolve`). A sibling definition that uses a
construct the backend cannot lower yet no longer blocks the build, as long as
the entry never references it:

```sh
marv build examples/geometry.mv --entry max --run 3 7   # 7 — `translate` is
                                                        # unreachable, so its
                                                        # unsupported method
                                                        # call is irrelevant
```

The entry resolves as for `run`: `--entry NAME` (bare or qualified), else
`main` in the entry file, else the sole function. When none of those resolves
(no `main` among several functions), the **whole module set** is compiled — and
`commit`/audit flows always operate on every discovered definition; pruning is a
`build` behavior only.

- **`--target`** — `native-cranelift` (default) or `wasm-component`. LLVM is a
  later milestone. Unknown targets are rejected.
- **`--run`** *(native only)* — after compiling, JIT-executes the entry point and
  prints its integer result. It cannot be combined with native AOT output flags.
  Without `--run`, `--out`, or `--emit`, `build` reports success and the arity.
- **`--emit object|exe`** *(native only)* — `object` writes the deterministic
  relocatable object for the entry's reachable closure; `exe` links that object
  with the small native runtime wrapper. For native builds, `--out` without
  `--emit` means `--emit exe`.
- **`--release`** — omit the Tier-1 debug checks from the compiled artifact.
  Today that is the runtime **bounds check** (MARV-34): debug builds (the
  default) abort on an array/slice subscript outside `0..len` — Cranelift with a
  structured report on stderr, wasm with an `unreachable` trap — while release
  builds emit the unchecked pre-MARV-34 code. The interpreter (`marv run`) is
  the debug runner and always checks. See
  [run-and-codegen.md](run-and-codegen.md).
- **`--out PATH`** — where to write the artifact. Defaults are `<file>` for a
  native executable, `<file>.o` for a native object, and `<file>.wasm` for wasm.
- **`--store DIR`** — resolve known imports through `DIR/lockfile.json`, fetch
  their transitive closure from `DIR/blobs/b3/`, and compile Core whose calls
  are keyed by pinned dag hashes. Missing blobs are hard errors. For packages,
  use the same store directory passed to `marv commit`; the source package is
  still parsed/checked before edges are rewritten to pinned hashes.
- **`--entry`** / **`[args...]`** — as for `run` (integer arguments).

### `--target native-cranelift`

Cranelift (`marv-codegen-cl`) has two modes:

- **JIT**: `--run` executes the entry in-process and prints its integer result.
- **AOT**: `--emit object` writes a relocatable object, and `--out` /
  `--emit exe` links a host executable that can run without `marv`.

The object exports content-hashed marv symbols and imports the small runtime ABI
(`marv_rt_alloc`, `marv_rt_heap_mark`, `marv_rt_heap_reset`, and, for debug
bounds checks, `marv_rt_bounds_fail`). The CLI executable path links those hooks
with a generated C wrapper. Today that wrapper supports entries with up to four
integer value arguments and prints the integer result; richer host integration
for capabilities stays with the interpreter/WASM host story for now.

```sh
marv build examples/factorial.mv                              # compiles, reports success
marv build --run examples/factorial.mv --entry factorial 6    # prints 720
marv build examples/factorial.mv --entry factorial --out factorial
./factorial 6                                                  # prints 720
marv build --emit object examples/factorial.mv --entry factorial -o factorial.o
```

If the reachable closure needs a construct the backend cannot lower, such as a
capability `perform`, native AOT fails with the same clear backend error as the
JIT path and does not leave a partial artifact.

### `--target wasm-component`

Emits a WebAssembly module (`marv-codegen-wasm`) and reports its **capability
manifest** — the host imports it requires. A pure module imports nothing; a
module that `perform`s a capability imports one function per operation
(`spec/01` §9). The host (a wasmtime embedding or a browser page) grants a
capability by supplying that import, and withholds it by not.

```sh
marv build --target wasm-component examples/factorial.mv -o factorial.wasm
#   → wrote factorial.wasm … capabilities required: none (pure — imports nothing)
marv build --target wasm-component web/fetcher.core.json -o fetcher.wasm
#   → capabilities required (host imports): Net::op0
```

(Today the artifact is a core wasm module — the component model's substrate —
with capabilities as host imports; full component/WIT packaging is a later step.)

All three backends — interpreter, Cranelift, and WASM — are differentially tested
for agreement on a corpus under [`../tests/run/`](../tests/run); the WASM sandbox
also ships a browser demo. See [`run-and-codegen.md`](run-and-codegen.md).

## `marv resolve-impl`

```
marv resolve-impl <file>
```

The `marv/resolveImpl` report (`spec/01` §3.4): for every generic instantiation
the program requests, print which coherent `impl` each of its bounded type
arguments resolves to, and the fully-qualified definition each interface method
dispatches to. Also surfaces any unsatisfied-bound (`E0160`) or coherence
(`E0161`) violations (exiting non-zero if present).

```
marv resolve-impl examples/generics.mv
# generics.max@i32 (instantiates `max`)
#     T: Ord = i32  ->  impl Ord[i32]
#         cmp -> generics.cmp$Ord$i32
```

This makes monomorphization auditable: a human (or agent) can confirm exactly
which implementation a generic call selected, with no global inference or orphan
ambiguity.

## `marv verify`

```
marv verify [--def NAME] <file>
```

Discharges each function's `requires`/`ensures` contracts — and every `while`
loop's `invariant`s (MARV-22) — with the SMT backend (Tier 2, `marv-verify`) and
prints one of `proved` / `failed` (with a concrete counterexample) /
`unsupported` per function (`spec/03` §3.3, §4.3). A function whose only
contract is a loop `invariant` is reported too. `--def` restricts to one
definition. Exits non-zero only when a contract is provably **violated** (a
`failed`); `unsupported` is success (the honest fallback to Tier-1 runtime
checks). Requires a `z3` binary on `PATH`; without one, every function reports
`unsupported` and falls back to runtime checking.

```sh
marv verify examples/clamp.mv
#   proved   math.clamp  (Tier 2: holds for all inputs)
```

See [`verification.md`](verification.md) for the two tiers, the verified subset,
and how a counterexample is produced.

## `marv commit`

```
marv commit [--store DIR] <file>
```

Checks the file, then freezes its discovered source module/package graph into
the content-addressed store (default `.marv/`), rebinds names in the lockfile,
and prints the delta —
each definition marked **new** (frozen & reviewed) or **already in store /
already reviewed**, plus any names **rebound** to a new hash. Identity is the
content (dag) hash, so re-committing the same source is idempotent and renames
change no hashes:

```sh
marv commit examples/clamp.mv          # + math.clamp  b3:d94f…  (new — frozen & reviewed)
marv commit examples/clamp.mv          # = math.clamp  b3:d94f…  (already reviewed)
```

For a package, prefer an explicit package-local store:

```sh
marv commit --store app/.marv app/src/main.mv
marv build --store app/.marv --run app/src/main.mv
```

See [`store.md`](store.md) for the dag-hash / Merkle-DAG scheme, free renames,
dedup, the blob layout, pinned builds, GC/audit commands, and how this
underpins Stage-1 self-hosting.

## `marv store audit`

```
marv store audit [--store DIR]
```

Prints every stored blob with its hash, last-seen name, reviewed flag, lockfile
reachability, dependency count, and unsafe-site count. `unsafe fn` and
`unsafe extern fn` metadata is source-level audit data outside Core identity;
committed unsafe definitions therefore show a nonzero unsafe-site count even
when their Core body dedups with an already-reviewed safe definition.

## `marv store gc`

```
marv store gc [--store DIR]
```

Removes blobs not reachable from any current lockfile binding and rewrites the
content-addressed blob directory.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success (and, for `fmt --check`, all inputs already canonical). |
| `1` | Usage error, I/O error, unimplemented command, `--check` found non-canonical input, a `check`/`build`/`run` found checker errors, or a backend/runtime failure. |
