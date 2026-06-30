# std/ — the marv standard prelude (M7)

The standard library, written in marv. These are the declarations the toolchain
links against once a build references them by content hash (`spec/01` §8).

| File | Declares |
|------|----------|
| [`option.mv`](option.mv) | `Option[T]` — the only way to express absence (`?T` sugar). |
| [`result.mv`](result.mv) | `Result[T, E]` — success/typed-failure (`!T` sugar, `?` propagation). |
| [`capabilities.mv`](capabilities.mv) | The capability types `Io`/`Fs`/`Net`/`Http`/`Clock`/`Rand`/`Alloc` (plus `Stream`/`Conn`) as declared interfaces — power enters only through these (`spec/01` §5). |
| [`http.mv`](http.mv) | `Request`/`Response` structs plus helpers over the host-provided `Http` request capability. |
| [`spawn.mv`](spawn.mv) | First structured-concurrency slice: `TaskI64` linear handles plus `spawn_i64`/`join_i64` over explicit `Spawn`. |
| [`collections.mv`](collections.mv) | `List[T]`, `Map[K, V]`, and `Set[T]` — growable collections allocated through explicit `Alloc`; string-compatible map/set ops plus scalar `i64` hash-backed ops run on interpreter, Cranelift, and WASM. |
| [`str.mv`](str.mv) | `from_chars(alloc, chars)` — explicit-`Alloc` string building from `List[char]`; lowered to a Core string primitive. |
| [`bytes.mv`](bytes.mv) | Byte-slice helpers plus source-level UTF-8 encode/decode between `[]u8`, `List[u8]`, and `str`. |
| [`json.mv`](json.mv) | JSON scalar and source-backed flat-object parsing/serialization with typed `JsonError`. |

## Status

`option.mv` and `result.mv` are now **real parsed source**, not reference-only:
they use `enum`, generics, and `match`, all of which the front end accepts. They
parse, are canonical (`marv fmt` reprints them unchanged), and lower to Core.
The CLI resolves `std` imports transitively, so `marv check std/result.mv`
resolves the imported `Option` constructors from `std/option.mv` in the
single-file command path. Doc comments are preserved by the canonical formatter
and remain outside content identity.

`capabilities.mv` is also live parsed source: a non-generic `interface` is a
capability declaration, and method calls on capability values lower to
`perform`/narrowing. `Alloc` is declared there alongside `Io`/`Fs`/`Net`/`Http`
as the auditable entry point for user-visible growable allocation and
host-provided request handling.

`collections.mv` is live parsed source too. The public `List[T]` operations are normal std
functions at the surface, while the compiler lowers their call sites to list Core ops with a
runtime `[len, cap, e0, …]` layout. `push`, `pop`, and `set` return the updated list value,
so surface code normally rebinds the `var` that holds the list. Backends update the backing
block directly when no growth is needed.

`Map[K, V]` and `Set[T]` are present as std types. String-key operations remain
source-compatible with the first slice: `map_new`, `map_with_capacity`, `map_insert`,
`map_get_or`, `map_contains`, `map_remove`, `map_len`, plus the parallel `set_*`
operations. MARV-61 adds stored hashes in the entry layout, a first `Hash[T]`
interface (`hash_key` + `key_eq`), and scalar-key `map_i64_*` / `set_i64_*`
operations for `Map[i64, V]` and `Set[i64]`. Allocation remains visible through
`Alloc`, and `tests/run/map_set.mv` pins interpreter/Cranelift/WASM parity.

`str.mv` is live parsed source as well. Its `from_chars` body is a placeholder in source form:
the lowerer rewrites calls imported from `std.str` to a Core primitive that copies a
`List[char]` into the runtime string block. Taking `Alloc` keeps user-visible string building
explicit in signatures.

`bytes.mv` is ordinary marv source layered on top of slices, lists, chars, strings, and
typed errors. It provides byte length/index/equality helpers, `List[u8]` append, UTF-8
decode from `[]u8` to `str`, and UTF-8 encode from `str` to `List[u8]`; allocation remains
explicit through `Alloc`.

`json.mv` is ordinary marv source layered on strings, scalar enums, typed errors, and explicit
`Alloc` string building. This first slice parses JSON scalars and validates flat objects whose
field values are scalars, exposes field lookup over the validated source text, and serializes
scalars plus validated objects. Recursive arrays/materialized object maps stay with the later
recursive-ADT and hash-map work.

`http.mv` is the first server-runtime std layer. A host grants one `Http`
capability per request, or a `Net`-authorized `Listener` can accept one with
`accept_http`; low-level operations read the method/path/body text and send a
response, while user code can work with normal `Request` and `Response` structs
through helper functions. Multi-request serve loops, raw byte streaming, and OS
socket scheduling stay host/runtime follow-ups; `File`, `Listener`, and `Conn`
close-once lifecycle safety is represented in `capabilities.mv` with
`linear interface` resource capabilities.

`spawn.mv` is the first structured-concurrency std slice. `Spawn` is capability-gated in
`capabilities.mv`; `spawn_i64` performs `Spawn.start` and returns a `linear TaskI64`, and
`join_i64` consumes that handle. The checker rejects detached/unjoined task handles, while
the interpreter models each start as a recorded `Spawn` effect. Channels, true parallel host
scheduling, and generic task result handles remain staged follow-ups.
