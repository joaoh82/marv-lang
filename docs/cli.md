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
| `fmt`    | **working** (M0, parse-and-reprint + whitespace fallback) | Canonicalize marv source. |
| `check`  | **working** (M2) | Type / effect / capability / error-set / reference / linearity checking. |
| `build`  | **working** (M4 `native-cranelift`, M5 `wasm-component`) | Compile a target: Cranelift JIT or a WebAssembly module. |
| `run`    | **working** (M4) | Interpret an entry point with an explicit capability grant set. |
| `resolve-impl` | **working** (MARV-5) | Report each generic instantiation and which coherent `impl` its bounded type arguments select. |
| `verify` | **working** (M6, Tier 2) | Discharge `requires`/`ensures` contracts via SMT. |
| `commit` | **working** (M7) | Freeze definitions into the content-addressed store; report the lockfile delta. |

`check`, `build`, `run`, `verify`, and `commit` accept either a `.mv` **source** file (parsed and
lowered through the front end) or a `*.core.json` **Core-IR snapshot**
(`marv_db::CoreModuleSpec`, `spec/03` §3.1). Capability use is now expressible in
`.mv` source: a method call on a capability value (`io.stdout().write(...)`,
`io.fs()`) lowers to a `Core::Perform` and its effect row is inferred and checked
(MARV-6). `.core.json` remains useful for hand-authoring Core directly.

**`import std.*` resolution.** When a source file imports a `std` module
(e.g. `import std.io (Io)`), the CLI locates the `std/` source directory — the
`MARV_STD` environment variable if set, else the nearest ancestor of the file that
contains one — parses the imported modules (transitively), and lowers them
alongside your file so the imported declarations are in scope: capability
interfaces, and (MARV-18) **enums** — a single file that constructs or matches an
imported enum (`Option.Some(x)`, `match res { Result.Ok(x) => … }`) lowers it to
real constructors with the imported enum's nominal and tags, so
`marv check std/result.mv` works standalone. If an imported enum's source cannot
be resolved (the named `std` module has no file, or the import is not `std.*`),
referencing its constructors is a clear lower error naming the import and its
module. General cross-module linking via the content store is MARV-14; this is
the minimal resolution the capability and enum surfaces need.

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
program (non-negotiable invariant #1). As of M0 it **parses and reprints** the
implemented language subset (module headers, imports, `struct`/`fn` decls, the
type language, `let`/`var`/`return`, block tails, binary operators, `if`/`else`):
indentation, spacing, full parenthesization, and integer/string normalization are
all applied. Input outside that subset — or otherwise unparseable — falls back to
the parser-free **whitespace canonicalizer** (line endings, tabs → 4 spaces,
trailing-whitespace stripping, blank-line collapsing, single trailing newline).
Both paths are deterministic and idempotent. See [`formatter.md`](formatter.md).

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
marv run [--grant CAP,CAP] [--entry NAME] <file> [args...]
```

Interprets an entry point with the tree-walking interpreter (`marv-interp`) —
the reference semantics oracle. It first runs `check` and **refuses to run** if
there are errors.

- **`--grant`** — the comma-separated capabilities the host hands to the program.
  The entry's capability parameters are filled *only* from this set; an
  ungranted capability makes the entry un-runnable (`spec/03` §4.5, the sandbox).
- **`--entry`** — which function to call. Defaults to `main`, or the sole
  function if there is exactly one.
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
marv run examples/hello.mv                              # refused: capability `Io` not granted
```

## `marv build`

```
marv build [--target T] [--run] [--release] [--out PATH] [--entry NAME] <file> [args...]
```

Compiles with the selected backend. Like `run`, it first runs `check` and
**refuses to compile** code with errors — this is where a program that uses a
capability absent from its effect row fails to build (`spec/03` §5).

- **`--target`** — `native-cranelift` (default) or `wasm-component`. LLVM is a
  later milestone. Unknown targets are rejected.
- **`--run`** *(native only)* — after compiling, JIT-executes the entry point and
  prints its integer result. Without it, `build` reports success and the arity.
- **`--release`** — omit the Tier-1 debug checks from the compiled artifact.
  Today that is the runtime **bounds check** (MARV-34): debug builds (the
  default) abort on an array/slice subscript outside `0..len` — Cranelift with a
  structured report on stderr, wasm with an `unreachable` trap — while release
  builds emit the unchecked pre-MARV-34 code. The interpreter (`marv run`) is
  the debug runner and always checks. See
  [run-and-codegen.md](run-and-codegen.md).
- **`--out PATH`** *(wasm only)* — where to write the `.wasm` module (default
  `<file>.wasm`).
- **`--entry`** / **`[args...]`** — as for `run` (integer arguments).

### `--target native-cranelift`

Cranelift JIT (`marv-codegen-cl`).

```sh
marv build examples/factorial.mv                              # compiles, reports success
marv build --run examples/factorial.mv --entry factorial 6    # prints 720
```

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

Checks the file, then freezes its definitions into the content-addressed store
(default `.marv/`), rebinds their names in the lockfile, and prints the delta —
each definition marked **new** (frozen & reviewed) or **already in store /
already reviewed**, plus any names **rebound** to a new hash. Identity is the
content (dag) hash, so re-committing the same source is idempotent and renames
change no hashes:

```sh
marv commit examples/clamp.mv          # + math.clamp  b3:d94f…  (new — frozen & reviewed)
marv commit examples/clamp.mv          # = math.clamp  b3:d94f…  (already reviewed)
```

See [`store.md`](store.md) for the dag-hash / Merkle-DAG scheme, free renames,
dedup, the lockfile, and how this underpins Stage-1 self-hosting.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success (and, for `fmt --check`, all inputs already canonical). |
| `1` | Usage error, I/O error, unimplemented command, `--check` found non-canonical input, a `check`/`build`/`run` found checker errors, or a backend/runtime failure. |
