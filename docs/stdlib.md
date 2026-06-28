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
> `examples/optionals.mv` does the same from user code). The persistent content store and
> pinned hash linking are done (MARV-14); the remaining module work is broader project/package
> source discovery beyond `std` (MARV-49). Still pending: `linear` capabilities, so a `Conn`
> must be `close`d exactly once (MARV-27). The capability *model* is also exercised over the
> Core IR and on WebAssembly (host imports).

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

### `std/collections.mv` — `List[T]`
A growable list. Construction and `push` take an explicit `Alloc` capability; `push`, `pop`,
and `set` return the updated list value, so callers rebind a `var`.
`len(list)`, `list[i]`, `get`, `set`, and `for x in list` run on the interpreter, Cranelift,
and WASM backends. The runtime layout is `[len, cap, e0, …]`; `len` is a header load and index
loads skip the two-word header. Backends update the backing block in place when capacity allows
and allocate-copy only on growth.

```marv
struct List[T] { … }
fn new[T](alloc: Alloc) -> List[T]
fn with_capacity[T](alloc: Alloc, capacity: usize) -> List[T]
fn push[T](alloc: Alloc, list: List[T], value: T) -> List[T]
fn pop[T](list: List[T]) -> List[T]
fn get[T](list: List[T], index: usize) -> T
fn set[T](list: List[T], index: usize, value: T) -> List[T]
pure fn len[T](list: List[T]) -> usize
```

### `std/str.mv` — string building
String manipulation is built into the language surface for literals, `+`, `len(s)`, `s[i]`,
`s[a..b]`, and `for c in s`. Growable construction stays explicit: build a `List[char]`
with an `Alloc` capability, then call `from_chars`.

```marv
fn from_chars(alloc: Alloc, chars: List[char]) -> str
```

The lowerer rewrites `from_chars` call sites to a Core primitive; the source body is only the
std declaration shape.

### `std/bytes.mv` — bytes and UTF-8
Byte buffers use the existing runtime-length slice/list surfaces: borrowed data is `[]u8`,
growable output is `List[u8]`, and construction takes an explicit `Alloc`. `decode_utf8`
turns a byte slice into a `str` with typed `Utf8Error.Invalid` failures for malformed input;
`encode_utf8` turns a `str` into a growable byte list. The helpers are plain marv source, so
they exercise the same `u8`, `char`, `str`, `List[T]`, `?`, and loop paths as user code.

```marv
error Utf8Error { Invalid }
pure fn byte_len(bytes: []u8) -> usize
pure fn at(bytes: []u8, index: usize) -> u8
fn append(alloc: Alloc, bytes: List[u8], byte: u8) -> List[u8]
pure fn equal(left: []u8, right: []u8) -> bool
fn decode_utf8(alloc: Alloc, bytes: []u8) -> !str
fn encode_utf8(alloc: Alloc, text: str) -> List[u8]
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
or string/byte builder must receive `Alloc`, and the checker rejects an allocation
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
