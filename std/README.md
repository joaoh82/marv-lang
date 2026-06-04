# std/ — the marv standard prelude (M7)

The standard library, written in marv. These are the declarations the toolchain
links against once a build references them by content hash (`spec/01` §8).

| File | Declares |
|------|----------|
| [`option.mv`](option.mv) | `Option[T]` — the only way to express absence (`?T` sugar). |
| [`result.mv`](result.mv) | `Result[T, E]` — success/typed-failure (`!T` sugar, `?` propagation). |
| [`capabilities.mv`](capabilities.mv) | The capability types `Io`/`Fs`/`Net`/`Clock`/`Rand`/`Alloc` (plus `Stream`/`Conn`) as declared interfaces — power enters only through these (`spec/01` §5). |

## Status

`option.mv` and `result.mv` are now **real parsed source**, not reference-only:
they use `enum`, generics, and `match`, all of which the front end accepts. They
parse, are canonical (`marv fmt` reprints them unchanged), and lower to Core —
lower the two together (they share the `Option` constructor namespace; `result`
imports it) via `marv_core::lower_modules`, since single-file lowering resolves
only the enums declared in that file. Doc comments were dropped to keep them
canonical (the formatter does not yet preserve `///` — see the roadmap), so each
declaration's intent is summarized in the table above.

`capabilities.mv` still uses `interface`, which has no surface form yet, so it
remains a **reference declaration** consumed as the front end grows. The
content-addressed store (`marv-store`) and the capability-as-host-import model
(`marv-codegen-wasm`) are already in place to link and sandbox the prelude.
