# marv standard library

The prelude lives in [`std/`](../std), written in marv. It declares the core data types and
the capability interfaces every program links against (`spec/01` §§3, 5, 6).

> **Status.** The capability interfaces are now **live from source** (MARV-6): `std/`
> parses, lowers, and checks, and a program that `import std.io (Io)` and calls
> `io.stdout().write(...)` or narrows `io.fs()` checks its inferred effect row and runs under
> `marv run --grant` (the CLI resolves `import std.*` to these files — see
> [`cli.md`](cli.md)). Enums, generics, and `?`/`!` sugar are likewise real, and the
> resolution covers imported **enum constructors and `match`es** (MARV-18): each `std/` file —
> and any program importing one — checks standalone (`marv check std/result.mv` resolves the
> `Option.Some(x)` / `Option.None` it builds to the imported enum's constructors;
> `examples/optionals.mv` does the same from user code). Still pending:
> cross-*module* error-set propagation and the persistent content store that links defs by
> hash (MARV-14), and `linear` capabilities (so a `Conn` must be `close`d). The capability
> *model* is also exercised over the Core IR and on WebAssembly (host imports).

## Data types

### `std/option.mv` — `Option[T]`
The only way to express absence (`?T` is sugar for `Option[T]`). Matched exhaustively; there
is no `null`.

```marv
enum Option[T] { None, Some(T) }
pure fn is_some[T](opt: &Option[T]) -> bool
pure fn unwrap_or[T](opt: Option[T], fallback: T) -> T
```

### `std/result.mv` — `Result[T, E]`
Success or typed failure. `!T` with an inferred error set `E` is sugar for `Result[T, E]`;
`?` propagates the `Err` branch (`spec/01` §6, `spec/02` §D).

```marv
enum Result[T, E] { Ok(T), Err(E) }
pure fn is_ok[T, E](res: &Result[T, E]) -> bool
pure fn ok[T, E](res: Result[T, E]) -> Option[T]
```

### `std/ord.mv` — `interface Ord[T]`
Total ordering (`spec/01` §3.4). The `Ordering` enum is the result of `cmp`; a coherent `impl`
supplies `cmp` per concrete type (`Ord[i32]`, `Ord[i64]`), and the generic `max`/`min` bound a
type parameter with `T: Ord`. A call like `max(3, 7)` **monomorphizes** to `max@i32` and
**dispatches** `cmp` to `impl Ord[i32]`; `marv resolve-impl` reports the selection, and
instantiating at a type with no `impl` fails `check` with `E0160`.

```marv
enum Ordering { Lt, Eq, Gt }
interface Ord[T] { fn cmp(a: T, b: T) -> Ordering }
impl Ord[i32] { fn cmp(a: i32, b: i32) -> Ordering { … } }
fn max[T: Ord](a: T, b: T) -> T
fn min[T: Ord](a: T, b: T) -> T
```

## Capabilities

`std/capabilities.mv` declares the standard capability types as interfaces — the operations a
holder may perform. Power enters a program only through these; they are unforgeable (received
or narrowed, never constructed). A concrete host (native runtime, or a WASM host-import set)
supplies the implementations the process/page chooses to grant.

| Capability | Role | Representative operations |
|------------|------|---------------------------|
| `Io` | Root capability; everything narrows from it | `fs() -> Fs`, `net() -> Net`, `clock() -> Clock`, `rand() -> Rand`, `alloc() -> Alloc`, `stdout() -> Stream` |
| `Stream` | A text/byte output stream | `write(text: str) -> !` |
| `Fs` | Filesystem | `read(path: str) -> ![]u8`, `write(path, bytes) -> !` |
| `Net` | Network | `get(url) -> ![]u8`, `connect(host, port) -> !Conn` |
| `Conn` | Open connection | `send`, `recv`, `close` |
| `Clock` | Monotonic time | `now() -> i64` |
| `Rand` | Randomness | `next_u64() -> u64` |
| `Alloc` | Allocator | `alloc(bytes: usize) -> ![]u8` |

`Alloc` is the auditable entry point for user-visible growable allocation: a list
or string builder must receive `Alloc`, and the checker rejects an allocation
`perform` outside the function's capability row. Compiler-managed boxes for
fixed-shape structs/enums/arrays are still an implementation detail and do not
add an `Alloc` parameter to user signatures; the runtimes allocate those boxes
from the same reclaiming heap infrastructure.

### Why this matters

A human auditor verifies "this transform cannot exfiltrate data" by reading one line of a
signature — no `Net` parameter. And it *is* the sandbox: hand a function only the
capabilities you want it to have. On WebAssembly each capability becomes a host import the
embedding decides to satisfy; a pure module imports nothing and a `Net`-using module cannot
even instantiate without a `Net` import (see the [`web/`](../web) demo and
[platform-support.md](platform-support.md)).

Narrowing (attenuation): `let fs = io.fs()` turns an `Io` into an `Fs`, so downstream code is
bounded to the filesystem. Capabilities flow down only; they are never returned or stored.
