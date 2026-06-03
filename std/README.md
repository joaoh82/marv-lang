# std/ — the marv standard prelude (M7)

The standard library, written in marv. These are the declarations the toolchain
links against once a build references them by content hash (`spec/01` §8).

| File | Declares |
|------|----------|
| [`option.mv`](option.mv) | `Option[T]` — the only way to express absence (`?T` sugar). |
| [`result.mv`](result.mv) | `Result[T, E]` — success/typed-failure (`!T` sugar, `?` propagation). |
| [`capabilities.mv`](capabilities.mv) | The capability types `Io`/`Fs`/`Net`/`Clock`/`Rand`/`Alloc` (plus `Stream`/`Conn`) as declared interfaces — power enters only through these (`spec/01` §5). |

## Status

These use post-M0 surface — `enum`, `interface`, generics, `match`, `?`/`!`
sugar — that the M0 parser does not yet accept (like `examples/hello.mv` and
`examples/report.mv`). They are the **reference prelude declarations**: faithful
to the grammar (`spec/02` §B) and the design (`spec/01` §§3, 5, 6), consumed as
the front end's surface grows toward full coverage. The content-addressed store
(`marv-store`) and the capability-as-host-import model (`marv-codegen-wasm`) are
already in place to link and sandbox them.
