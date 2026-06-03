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
| `verify` | milestone M6 | Discharge contracts via SMT. |

Commands that are not yet implemented parse their arguments and exit non-zero
with `not yet implemented (milestone Mx)`, so scripts get an honest signal.

`check`, `build`, and `run` accept either a `.mv` **source** file (parsed and
lowered through the front end) or a `*.core.json` **Core-IR snapshot**
(`marv_db::CoreModuleSpec`) — currently the only way to express a body that
`perform`s a capability, since the surface has no `perform` form yet
(`spec/03` §3.1).

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

The entry's result is printed to stdout; any capability effects it performed are
logged to stderr as `effect: <cap> op#<n> [<args>]`.

```sh
marv run examples/factorial.mv --entry factorial 6     # prints 720
marv run examples/arithmetic.mv                         # entry defaults to main → 42
```

## `marv build`

```
marv build [--target T] [--run] [--out PATH] [--entry NAME] <file> [args...]
```

Compiles with the selected backend. Like `run`, it first runs `check` and
**refuses to compile** code with errors — this is where a program that uses a
capability absent from its effect row fails to build (`spec/03` §5).

- **`--target`** — `native-cranelift` (default) or `wasm-component`. LLVM is a
  later milestone. Unknown targets are rejected.
- **`--run`** *(native only)* — after compiling, JIT-executes the entry point and
  prints its integer result. Without it, `build` reports success and the arity.
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

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success (and, for `fmt --check`, all inputs already canonical). |
| `1` | Usage error, I/O error, unimplemented command, `--check` found non-canonical input, a `check`/`build`/`run` found checker errors, or a backend/runtime failure. |
