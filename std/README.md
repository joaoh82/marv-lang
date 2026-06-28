# std/ — the marv standard prelude (M7)

The standard library, written in marv. These are the declarations the toolchain
links against once a build references them by content hash (`spec/01` §8).

| File | Declares |
|------|----------|
| [`option.mv`](option.mv) | `Option[T]` — the only way to express absence (`?T` sugar). |
| [`result.mv`](result.mv) | `Result[T, E]` — success/typed-failure (`!T` sugar, `?` propagation). |
| [`capabilities.mv`](capabilities.mv) | The capability types `Io`/`Fs`/`Net`/`Clock`/`Rand`/`Alloc` (plus `Stream`/`Conn`) as declared interfaces — power enters only through these (`spec/01` §5). |
| [`collections.mv`](collections.mv) | `List[T]` — growable lists allocated through explicit `Alloc`; core ops run on interpreter, Cranelift, and WASM. |
| [`str.mv`](str.mv) | `from_chars(alloc, chars)` — explicit-`Alloc` string building from `List[char]`; lowered to a Core string primitive. |
| [`bytes.mv`](bytes.mv) | Byte-slice helpers plus source-level UTF-8 encode/decode between `[]u8`, `List[u8]`, and `str`. |

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
`perform`/narrowing. `Alloc` is declared there alongside `Io`/`Fs`/`Net` as the
auditable entry point for user-visible growable allocation.

`collections.mv` is live parsed source too. The public `List[T]` operations are normal std
functions at the surface, while the compiler lowers their call sites to list Core ops with a
runtime `[len, cap, e0, …]` layout. `push`, `pop`, and `set` return the updated list value,
so surface code normally rebinds the `var` that holds the list. Backends update the backing
block directly when no growth is needed.

`str.mv` is live parsed source as well. Its `from_chars` body is a placeholder in source form:
the lowerer rewrites calls imported from `std.str` to a Core primitive that copies a
`List[char]` into the runtime string block. Taking `Alloc` keeps user-visible string building
explicit in signatures.

`bytes.mv` is ordinary marv source layered on top of slices, lists, chars, strings, and
typed errors. It provides byte length/index/equality helpers, `List[u8]` append, UTF-8
decode from `[]u8` to `str`, and UTF-8 encode from `str` to `List[u8]`; allocation remains
explicit through `Alloc`.
