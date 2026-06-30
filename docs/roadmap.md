# marv roadmap & task ordering

Stage-0 milestones **M0–M7 are complete** (the compiler is implemented end to end in Rust;
see the specs and per-milestone docs). What remains is **growing the language surface** so
more of marv — and eventually the compiler itself — becomes real, plus backend/verification
breadth.

This page is the **global ordering and dependency graph** for that work. The detailed,
self-contained task specs live in the project tracker as `MARV-#`; this is the map that says
where each one sits and what must land first. Each task references back here.

> New here? Read [`../README.md`](../README.md), [`../AGENTS.md`](../AGENTS.md), and the
> [`../spec/`](../spec) files first; then pick a task below.

## The dependency graph

| Task | Phase | Blocked by | Unblocks | Priority |
|------|-------|-----------|----------|----------|
| ~~**MARV-1** enums + `match` (payloads)~~ ✅ done | 1 · Surface (spine) | — | 3, 5, 9, `std` | high |
| ~~**MARV-4** construction/mutation (struct literals, indexing, assignment, `var`)~~ ✅ done | 1 · Surface (spine) | — | 2, 9 | high |
| ~~**MARV-2** `while`/`for` loops → `Core::Loop`~~ ✅ done | 1 · Surface (spine) | ~~MARV-4~~ ✅ | 11 | high |
| ~~**MARV-3** error handling (`error`, `!T`, `?`, error-set inference)~~ ✅ done | 1 · Surface (spine) | ~~MARV-1~~ ✅ | 6 | high |
| ~~**MARV-5** generics + interfaces/impl (monomorphization)~~ ✅ done | 1 · Surface | ~~MARV-1~~ ✅ | 6 | medium |
| ~~**MARV-6** capabilities/`perform` from source~~ ✅ done | 1 · Surface | ~~MARV-5~~ ✅, ~~MARV-3~~ ✅ | — | medium |
| ~~**MARV-7** scalars & collections (str/char, slices/arrays, `as`)~~ ✅ done | 1 · Surface | — (pairs w/ 4) | — | medium |
| ~~**MARV-8** reachability-pruned compilation~~ ✅ done | 2 · Backends | — *(independent)* | — | medium |
| ~~**MARV-9** aggregate/enum codegen (interp + Cranelift + WASM)~~ ✅ done | 2 · Backends | ~~MARV-1~~ ✅, ~~MARV-4~~ ✅ | 10 | medium |
| ~~**MARV-30** array literals + `len`/index codegen (+ index store)~~ ✅ done | 2 · Backends | ~~MARV-9~~ ✅, ~~MARV-7~~ ✅ | — | medium |
| ~~**MARV-33** runtime-length slices `[]T` (construct, `len`/index, element store)~~ ✅ done | 2 · Backends | ~~MARV-30~~ ✅ | 20 | medium |
| ~~**MARV-34** Tier-1 debug bounds check on runtime array/slice indexing~~ ✅ done | 2 · Backends | ~~MARV-33~~ ✅ | — | medium |
| **MARV-10** AOT native + LLVM + WASM component/WIT | 2 · Backends | MARV-9 | — | low |
| ~~**MARV-11** verified-subset expansion + loop invariants + `old`/quantifiers~~ ✅ done — loop-invariant slice landed as **MARV-22** (Hoare-style initiation/consecution/use VCs); the rest extends the contract language to expressions (arithmetic, `len`, indexing, `p.x`), adds surface + Tier-1/Tier-2 `forall`/`exists` over `lo..hi` and `old(e)` in `ensures`, encodes truncate-toward-zero `/` `%` soundly over SMT's Euclidean ops, and encodes arrays/slices (SMT arrays + length) and non-recursive structs/enums (unpacked tag + fields, havocked from the `World`); `examples/quantifiers.mv` proves end to end. Honest residue: calls, floats, casts, and recursive ADTs are `unsupported`; fixed-width integer wrapping landed as **MARV-38** and generic non-recursive ADTs landed as **MARV-59** | 3 · Verification | ~~MARV-2~~ ✅, ~~MARV-1~~ ✅ | 22 | medium |
| ~~**MARV-38** Tier-2 fixed-width integer wraparound (close the mathematical-integers soundness gap)~~ ✅ done — every `+ - * / %` and unary `-` is reduced through a two's-complement `wrap64` over SMT `Int`s (a bitvector sort was ruled out: nonlinear `div`/`mul` is intractable — the division identity times out as a 64-bit bitvector but discharges in well under a second as wrapped `Int`s), and every havocked int/length is range-constrained to `[i64::MIN, i64::MAX]`; `ensures result > x` for `x + 1` is now refuted at `x = i64::MAX`. Tier 2 is *correctly stricter*: an accumulator claimed `>= 0` whose running sum can overflow (`examples/loops.mv`'s `sum_to`) no longer proves; the bounded `count_down_sum` does | 3 · Verification | ~~MARV-11~~ ✅ | — | medium |
| ~~**MARV-12** formatter doc-comments + real source spans~~ ✅ done | 5 · Infra/polish | — *(independent)* | — | medium |
| **MARV-13** port more compiler passes to marv (self-hosting) | 4 · Self-hosting | Phase-1 surface *(incremental now)* | — | low |
| ~~**MARV-14** persistent on-disk store + cross-module resolution~~ ✅ done | 4 · Store | — *(std linking wants Phase 1)* | — | low |
| **MARV-48** full application language surface + runtime epic | 6 · Application language | MARV-40 | 49–61 | medium |
| ~~**MARV-49** project/source-module discovery beyond `std`~~ ✅ done | 6 · Application language | MARV-14 | package metadata/query polish, 53, 60 | high |
| ~~**MARV-50** `Map[K, V]` and `Set[T]` in `std`~~ ✅ done — landed a first string-keyed/string-set, list-backed value-semantic slice with explicit `Alloc` and interpreter/Cranelift/WASM parity; the scalar hash-backed follow-up landed in MARV-61 | 6 · Std collections | ~~MARV-42~~ ✅ | 51, 55, 61 | medium |
| ~~**MARV-51** collection literals for `List`/`Map`/`Set`~~ ✅ done — explicit `alloc` forms for `List { alloc, items }`, `Set { alloc, items }`, and `Map { alloc, keys, values }` lower/run across interpreter, Cranelift, and WASM | 6 · Surface ergonomics | ~~MARV-42~~ ✅, ~~MARV-50~~ ✅ | — | medium |
| ~~**MARV-52** real `Iter[T]` protocol~~ ✅ done — adds `std.iter.IndexIter[T]`, `Iter[T]`, generic `iter_len`/`iter_get` wrappers, and a protocol-backed `for` path for `IndexIter[i64]` while preserving direct indexed fast paths | 6 · Surface/stdlib | ~~MARV-42~~ ✅ | ~~MARV-50~~ ✅ | medium |
| ~~**MARV-53** HTTP/server runtime capability + host ABI~~ ✅ done — adds an explicit `Http` request capability, `std.http` `Request`/`Response` helpers, a deterministic interpreter request host (`POST /echo`, body `marv-http-echo`), a `http_echo` example, and core-WASM capability imports that can return string handles. Honest residue: production listener/accept loops, streaming/raw bodies, and exact close-once lifecycle safety remain tied to MARV-27/MARV-10 and later std work. | 6 · Runtime/capabilities | ~~MARV-49~~ ✅, ~~MARV-54~~ ✅, MARV-27 *(for linear resource safety)* | MARV-10, MARV-27, 55 | high |
| ~~**MARV-54** bytes + UTF-8 stdlib utilities~~ ✅ done | 6 · Std/runtime | ~~MARV-42~~ ✅, ~~MARV-43~~ ✅ | 53, 55 | high |
| ~~**MARV-55** JSON + serialization stdlib~~ ✅ done — first scalar/source-backed flat-object slice with typed `JsonError`, explicit-`Alloc` scalar/string serialization, interpreter parse/error smoke coverage, and backend parity for serializer-safe paths; recursive/materialized JSON landed later in MARV-66 | 6 · Std/app data | ~~MARV-54~~ ✅, ~~MARV-50~~ ✅ | 53, 59, 61 | medium |
| ~~**MARV-56** capability-gated structured concurrency (`Spawn`)~~ ✅ done — first slice adds `std.io.Spawn`, `std.spawn.TaskI64` as a linear scoped handle, `spawn_i64`/`join_i64`, interpreter host-effect recording, boundary grant enforcement, and checker rejection for detached/unjoined task handles; channels/generic task results/true parallel scheduling remain follow-ups | 6 · Runtime/capabilities | — | — | medium |
| ~~**MARV-57** `unsafe`/FFI surface + `unsafeSites` audit query~~ ✅ done — first slice adds `unsafe fn` parsing/formatting, required `/// SAFETY:` justifications, `marv/unsafeSites`, MCP exposure, and store-audit unsafe-site metadata outside Core identity; raw pointer/FFI operations remain staged follow-ups | 6 · Audit/escape hatch | ~~MARV-12~~ ✅ | — | medium |
| ~~**MARV-58** early `return` inside loop bodies~~ ✅ done | 6 · Surface/control flow | ~~MARV-21~~ ✅ | — | low |
| ~~**MARV-59** Tier-2 recursive/generic ADTs~~ ✅ done — first sound slice supports generic, non-recursive ADTs by substituting concrete type arguments before havocking struct/enum fields; false claims over generic enum payloads still produce counterexamples, and recursive ADTs remain honest `unsupported`/Tier-1 fallback. `examples/adt_verify.mv` proves the supported shape. | 6 · Verification | ~~MARV-11~~ ✅, ~~MARV-38~~ ✅ | 13 | medium |
| **MARV-60** roadmap/docs cleanup for MARV-48 | 5 · Infra/polish | MARV-48 | 49–59 | low |
| ~~**MARV-61** hash-backed general-key `Map`/`Set` with `Hash`/`Eq` interfaces~~ ✅ done — adds stored hashes to the std map/set entry layout, a first `Hash[T]` interface carrying explicit `hash_key` + `key_eq`, and scalar-key `map_i64_*` / `set_i64_*` operations for `Map[i64, V]` and `Set[i64]` while preserving dynamic string-key behavior and collection literals across interpreter, Cranelift, and WASM | 6 · Std collections | ~~MARV-50~~ ✅, ~~MARV-5~~ ✅ | 55 | medium |
| **MARV-62** production application platform + self-hosting epic | 7 · Production/self-hosting | ~~MARV-48~~ ✅ | 63–74 | high |
| ~~**MARV-63** production HTTP listener/router runtime~~ ✅ done — adds `Listener.accept_http() -> !Http`, a runnable `examples/http_router.mv` with two routes and a JSON response, deterministic interpreter support for `Net.listen` → `Listener.accept_http` → `Http.respond`, and docs that keep raw/streaming bodies, multi-request scheduling, real OS socket serving, and WASM linear-resource import support honest as follow-ups. | 7 · Runtime/capabilities | ~~MARV-64~~ ✅ | — | high |
| ~~**MARV-64** linear resource capabilities for files/listeners/connections~~ ✅ done — adds `linear interface` for capability resource handles, marks `File`, `Listener`, and `Conn` as close-once resources, and pins source diagnostics for forgotten close, double close, and branch-only close paths. Production listener loops remain MARV-63 | 7 · Runtime/capabilities | ~~MARV-56~~ ✅ | 63 | high |
| ~~**MARV-65** raw FFI operations behind explicit unsafe audit boundaries~~ ✅ done — adds `unsafe extern fn` host FFI declarations with required `SAFETY:` justifications, canonical formatting, `marv/unsafeSites` / store-audit visibility, direct-call rejection from safe functions, and honest unsupported runtime/backend behavior until host symbol linking lands. Raw pointers/ABI-rich handles remain staged follow-ups. | 7 · Platform/unsafe | ~~MARV-57~~ ✅, ~~MARV-64~~ ✅ | — | medium |
| ~~**MARV-66** recursive/materialized JSON DOM with typed codecs~~ ✅ done — adds a list-backed recursive `Json` DOM (`Null`/`Bool`/`Number`/`String`/`Array`/`Object`), `JsonField`, nested parse from `str`/`[]u8`, deterministic `str`/byte serialization, typed lookup/extraction/build helpers, interpreter parse/error coverage, and three-way backend parity for serializer-safe construction paths. Honest residue: parser/error paths still rely on `raise`, so WASM coverage stays on reachable serializer entries until raise lowering lands. | 7 · Std/app data | ~~MARV-55~~ ✅, ~~MARV-59~~ ✅, ~~MARV-61~~ ✅ | 63 | medium |
| ~~**MARV-67** package manifest, dependency resolution, package-aware agent queries~~ ✅ done — adds a minimal deterministic `marv.toml` manifest (`[package] name`, `roots`, local `[dependencies.NAME] path`), a shared `marv-package` loader, CLI package-root discovery with transitive local path deps, `marv/openPackage` / MCP `marv_open_package`, a multi-package example, and docs for package bootstrap plus commit/build lockfile behavior. Pinned store deps remain the existing `--store` lockfile flow: package source graphs are loaded first, then known names are rewritten to dag hashes. | 7 · Packages | ~~MARV-49~~ ✅, ~~MARV-14~~ ✅ | 71–74 | medium |
| ~~**MARV-68** Cranelift AOT object/executable builds~~ ✅ done — `marv build --emit object` now writes deterministic Cranelift object files for the entry's reachable closure, while native `--out app` links a standalone value-entry executable with the current runtime hooks. Reachable unsupported constructs, including capability `perform`, still fail clearly before artifact emission. Production capability-hosted native runtimes remain MARV-69+ work. | 7 · Backends | ~~MARV-8~~ ✅, ~~MARV-9~~ ✅ | 69 | medium |
| ~~**MARV-69** LLVM optimized release backend~~ ✅ done — `native-llvm` emits deterministic LLVM IR and runs/links through `clang -O2` for scalar/calls/recursion/conditionals/loops, boxed structs/enums, arrays, runtime slice updates, `List[T]`, string ops, iterator loops, bytes/UTF-8, JSON serializer-safe paths, and current map/set/app corpus paths. Debug bounds checks abort and `--release` omits them. `raise`, capability `perform`, unsafe/resource host integration, and parser/error paths remain honest unsupported follow-ups. | 7 · Backends | ~~MARV-68~~ ✅ | — | medium |
| ~~**MARV-70** WASM component model and WIT packaging~~ ✅ done — `marv build --target wasm-component` now emits a validating WebAssembly component that embeds the existing core module, lowers typed component imports into the core capability imports, lifts reachable core exports, and writes a deterministic `.wit` sidecar. `wasm-core` remains available for wasmtime/browser core-module embeddings. The current component ABI exposes scalar/boolean/string-handle slots as `s64`; richer component-model records/resources and listener/resource imports remain honest follow-ups. | 7 · Backends | ~~MARV-53~~ ✅ | 63 | medium |
| ~~**MARV-71** Stage-1 AST/Core data model in marv~~ ✅ done — adds `selfhost/model.mv` with Stage-1 spans, diagnostics, tokens, type syntax, AST and Core node shapes, arena/count-based repeated edges, representative parsed-module and Core samples, interpreter tests for canonical parse/lower/check/run traversal, and docs mapping the Rust Stage-0 structures to marv model types. Lexer/parser/lowering/checker ports remain MARV-72/MARV-73; no self-hosting claim yet. | 7 · Self-hosting | ~~MARV-67~~ ✅ *(nice-to-have package workflow)* | 72, 73, 74 | high |
| ~~**MARV-72** Stage-1 lexer/parser slices in marv~~ ✅ done — adds `selfhost/parser.mv` with a first executable lexer/parser slice over a tiny `.mv` grammar (`mod`, one `pure fn`/`fn`, zero/one `i64` param, `-> i64`, int/identifier tail), emitting `selfhost.model` tokens/AST nodes, typed `FrontendError` failures for unsupported grammar, Rust Stage-0 differential tests for the supported fixture, and docs mapping supported vs deferred grammar coverage. Also teaches lowering to resolve imported struct literals/types/projections across module sets so the self-host parser can construct the shared model types. | 7 · Self-hosting | ~~MARV-71~~ ✅ | 73, 74 | high |
| ~~**MARV-73** Stage-1 lowering/checker slices in marv~~ ✅ done — adds `selfhost/lower_check.mv`, a first executable lowering/checker slice for the MARV-72 tiny grammar. It consumes the selfhost AST, builds a `CoreModule` with `i64`/unit arrow types, lambda roots, integer/local/global atom bodies, reports a tiny-scope diagnostic for nullary unresolved locals, raises `PassError` for unsupported AST forms, and differentially tests the supported fixture against Rust Stage-0's Core shape (`i64 -> i64`, lambda param `i64`, body `Var(0)`). Broader lowering/checker coverage remains incremental follow-up work before MARV-74 can claim a compiler-driver milestone. | 7 · Self-hosting | ~~MARV-71~~ ✅, ~~MARV-72~~ ✅ | 74 | high |
| **MARV-74** Stage-1 compiler driver and self-compile milestone | 7 · Self-hosting | 71, 72, 73 | — | high |

Done (Phase 0 · Infra/agent): **MARV-15** repo housekeeping · **MARV-16** CI/CD + release ·
**MARV-17** agent enablement (AGENTS.md, MCP server, skill).
Done (Phase 1 · Surface): **MARV-1** enums + `match` (surface parser + lowering →
`Ctor`/`Match`; generic parameter lists + type arguments; `std/option`+`std/result` now parse
and lower; `examples/color.mv` runs) · **MARV-4** construction/mutation (struct literals →
`Ctor`, index `a[i]` → `Prim{Index}`, assignment `lvalue = e` and `var` reassignment under
mutable value semantics — rebinding in ANF, field updates rebuild the aggregate;
`examples/mutation.mv` runs). Index *store* `a[i] = e` landed in MARV-30 (array element store)
· **MARV-2** `while`/`for` loops → `Core::Loop` (loop-carried `var`s threaded as the
node's `state`, body yields the next-state tuple, the loop yields the final tuple; Tier-1
`invariant` checking in the interpreter; SSA loop blocks in Cranelift + WASM via compile-time
register/local tuples; `examples/loops.mv` runs and agrees across all three backends). `for`
desugars to an index loop and **executes end to end** over arrays (MARV-30) and runtime-length
slices (MARV-33); MARV-20 closed it out with differential coverage for `for` over a slice, a
slice of structs, nested `for`s (depth-keyed index names), and sequential `for`s
(`tests/run/slices.mv`, `examples/slices.mv`); a loop body whose
tail is an `if`/`match` now threads the carried `var`s through the branch join (**MARV-21** — each
branch yields the next-state tuple, kept register/local-resident so the loop stays alloc-free);
early `return` from inside loop bodies now exits the enclosing function across interpreter,
Cranelift, and WASM (**MARV-58**); Tier-2 SMT discharge of loop invariants landed as
**MARV-22** (the loop slice of MARV-11)
· **MARV-42** growable `std.collections.List[T]` (depends on MARV-41 `Alloc`): `List`
is now a concrete std type instead of an opaque soft-skipped import, with
`new`/`with_capacity`, value-semantics `push`/`pop`/`set`, `get`/index, `len`,
and `for x in list` execution across interpreter + Cranelift + WASM. Runtime
layout is `[len, cap, e0, …]`; growable allocation is explicit through `Alloc`.
· **MARV-43** string manipulation: string literals now lower and run on every backend;
`str + str`, `s[i] -> char`, `s[a..b] -> str`, `for c in s`, and
`std.str.from_chars(alloc, chars: List[char])` share a `[len, codepoint…]` runtime block in
Cranelift and WASM. The builder path keeps growable construction explicit through `Alloc`,
and `tests/run/strings.mv` differentially checks interpreter == Cranelift == WASM.
· **MARV-54** bytes and UTF-8 stdlib: `std.bytes` adds byte-slice length/index/equality,
`List[u8]` append, typed `Utf8Error.Invalid`, and source-level UTF-8 encode/decode between
`[]u8`, `List[u8]`, and `str`; `tests/run/bytes_utf8.mv` pins backend-safe encode/equality
paths across the interpreter, Cranelift, and WASM, while decode's typed-error path is
interpreter/check covered until `raise`/`Result` codegen lands.
· **MARV-3** error handling (`error E { … }` decls, `!T`/bare-`!` error unions → `Result[T,
error-union]`, `E.Variant` → `Core::Raise`, postfix `?`; **full cross-call error-set inference**
via a fixpoint over the call graph in `marv-db`, surfaced through `marv/errorSet`; exhaustive
`match` over a caught error value; `examples/errors.mv` checks clean). `?` is a success-value
pass-through (errors propagate by unwinding, so error programs run on the interpreter);
capability-op error sets are MARV-6, the pinned store/linking layer is MARV-14, and
project/package source discovery beyond `std` is MARV-49. `Result`-value codegen is MARV-9.
· **MARV-7** scalars & collections (`char` literals `'a'`/`'\n'`; `as` casts → a new
`Core::Cast { value, to }` node carrying the target type — scalar↔scalar legality checked
(`E0104`), constant-narrowing rejected statically, integer width truncation/wrapping run
*identically across interpreter + Cranelift + WASM* and differential-tested in
`tests/run/casts.mv`; fixed-array type `[N]T` parses; `len(x)` → `Prim{Len}` as a builtin;
`examples/casts.mv` runs and checks clean). The value domain is still 64-bit, so sub-width
semantics surface only at the cast boundary — per-width **arithmetic** wrapping remains MARV-9;
array *literals*, index *stores*, and backend `len`/index over arrays landed in MARV-30.
· **MARV-23** prefix unary operators (`spec/02` §B `unary`): `-e` → `Prim{Neg}`, `not e` →
`Prim{Not}`, and `&e`/`&mut e` → a new `Core::Ref { mutable, of }` node the checker types as
`&T` (so escaping-reference diagnostics fire on `&e`). Unary binds tighter than every binary
operator; `not` is now a reserved word (like `and`/`or`). `-e`/`not e` run identically across
interpreter + Cranelift + WASM (`tests/run/unary.mv`, differential-tested); a second-class
reference carries no runtime cell, so `&e` evaluates to its referent's value. `examples/report.mv`
now parses, lowers, and checks for real — its `total(&sales)` reference-passing exercises the
new operator. · **MARV-12** (done) teaches the lexer/AST/formatter to **preserve `///` doc
comments** (kept on the item below them, normalized, excluded from the content hash) and threads
**real, definition-granular source spans** lexer→parser→`marv-db` so diagnostics, `typeAt`, and
`verify` carry byte+`{line,col}` spans and a `MissingCapability` fix resolves to a real insertion
offset. Per-sub-expression spans stay out of scope (the Core IR is span-free by identity design).
· **MARV-5** generics + interfaces/impl (`spec/01` §§3.3–3.4): generic params now carry interface
**bounds** (`fn sort[T: Ord]`), and `struct S[T]`/`enum E[T]` join the existing generic lists;
new `interface Name[T] { fn … }` and `impl Iface[Type] { … }` items parse, format (round-trip
including bounds/methods), and lower. **Monomorphization** is type-directed at each generic call
site: argument types are inferred, unified with the callee's parameter types to solve the
substitution, and a specialized def (`max@i32`) is generated by re-lowering the generic with its
type params bound to concrete types — interface-method calls in the specialized body **dispatch**
to the **coherent** impl (one per interface-per-type), and the instance is emitted into the
generic's *defining* module so cross-module instantiation resolves. The checker validates **bound
satisfaction** (`E0160` when no `impl Iface[Type]` exists) and **coherence** (`E0161` for a
duplicate impl); `marv resolveImpl`-style reporting (which impl/method a call selected) is exposed
via `marv_types::resolve_impls` and the `marv resolve-impl` CLI subcommand. `std/ord.mv` makes
`interface Ord[T]` real (with `impl Ord[i32]`/`Ord[i64]` and generic `max`/`min`);
`examples/generics.mv` runs on the interpreter (`max(3, 7)` → `7`). Generic *values* still run on
the interpreter only until aggregate/enum codegen lands (MARV-9); per-call-site type-argument
inference is best-effort over surface types (literals default `i32`).
· **MARV-6** capabilities/`perform` from source (`spec/01` §5): a **capability is a non-generic
`interface`** (`Io`, `Fs`, `Stream`, …; generic interfaces like `Ord[T]` stay bounded
polymorphism). A method call on a value of such a type lowers to `Core::Perform` — `io.fs()`
**narrows** to an `Fs` value, `fs.read(path)`/`out.write(text)` **perform** an operation — with the
`OpId` = the method's position and the operands = the non-receiver arguments. A non-`pure`
function's **declared effect row is the set of its capability parameters**; the body's row is
**inferred** from its `Perform` sites and checked against it, where a held capability **authorizes
its narrowing closure** (holding `Io` authorizes the `Fs`/`Net`/`Http`/… it can narrow to), so a `pure fn`
that performs — or any function reaching a capability it never received — is `MissingCapability`
(E0110) *from source*. The interpreter injects granted caps at the entry boundary and a narrowing
op returns the narrowed capability value; the CLI resolves `import std.*` to the `std/` sources
(transitively) so the capability interfaces are in scope (`MARV_STD` overrides discovery; MARV-49
extends the same source-module loading to local non-`std` imports). `std/capabilities.mv` parses/checks; `examples/hello.mv`
(`io.stdout().write(...)`), `examples/read_file.mv` (`io.fs()` → `fs.read`),
`examples/http_echo.mv` (`Http` request/response), and `examples/http_router.mv`
(`Net.listen` → `Listener.accept_http` → `Http.respond`) check, infer their rows, and run under explicit
grants. Cranelift n/a (rejects `Perform`); WASM lowers a `perform` to a host import. Linear
resource capabilities for `File`, `Listener`, and `Conn` landed in MARV-64.
· **MARV-18** single-file lowering of imported enum constructors / matches: the CLI's
`import std.*` resolution (MARV-6) now serves enums too — `marv check std/result.mv` standalone
lowers the `Option.Some(x)`/`Option.None` it builds to real `Ctor`s with the `std.option.Option`
nominal and declaration-order tags, and checks clean. The checker learned the two compatibility
rules this needs: a `Ctor` result (which carries no type arguments) satisfies a declared generic
reference of the same nominal, and an unresolved type parameter (`T` in a generic enum's field, at
a concrete construction/match site) compares as a wildcard — monomorphized instances still check
at concrete types. An imported enum whose source can't be resolved is now an explicit import/load
error or `UnresolvedImportedEnum` lower error instead of a misleading projection error or a silently
wrong method-call desugar. `examples/optionals.mv` shows the user-code shape and runs. The
salsa/protocol path keeps per-file read queries, while `marv/check` can check a source-only snapshot
as a module set (MARV-49).
· **MARV-37** unknown variant of a *known* enum errors at lowering: `Option.Sum(x)` (and the
unapplied `Option.Sum`, and the `match` pattern form) against a known local or resolved-imported
enum previously fell through to the method-call desugar / projection path and **passed `marv
check` silently** (the unknown global typed as `Unknown`). Now any `Enum.Variant` reference whose
enum is known but whose variant is undeclared is the explicit `UnknownEnumVariant` lower error —
naming the enum, listing its declared variants, and suggesting the nearest one (`did you mean
`Option.Some`?`) — and a declared payload variant referenced without arguments (a bare
`Option.Some`) is `UnappliedConstructor`. Local bindings still shadow these readings, so
capability narrowing (`io.fs()`) and the ordinary method-call desugar are untouched. This closes
the hole MARV-18 left open (it covered only *unresolvable* imported enums).

Done (Phase 2 · Backends): **MARV-9** aggregate/enum codegen across interp + Cranelift + WASM
(`spec/02` §C). Both native backends gained a **real runtime representation** for aggregates: every
value is one machine word, and a `struct`/tuple product or `enum` variant is a **pointer** to a
`[tag, field_0, …]` block — the *same* layout the interpreter's `Value::Agg` carries. **Cranelift**
heap-boxes via a host `marv_rt_alloc` symbol, lowers `Proj` to a load and an enum `Match` to a
`br_table` on the tag with per-arm field binding; **WASM** does the same over a linear memory (a new
memory + bump-pointer global, both module-internal so a *pure* module still imports nothing).
Boxing is **lazy** — a `Ctor` stays a compile-time register/local bundle and is spilled only when it
crosses a function boundary, is returned, or is matched at runtime — so loops (whose carried state
never escapes) still allocate nothing and the tested loop lowering is unchanged. The scalar-`bool`
`Match` (the `if`/`else` desugaring) is told from a boxed-`enum` one by the scrutinee's *type*, via a
shared `marv_types::layout` oracle (`is_boxed`/`variant_fields`/`type_of`) the backends consult — the
one fact the type-erased Core does not carry at the node. The three-way differential corpus
(`tests/run/structs.mv`, `color.mv`, `shapes.mv`) asserts interp == Cranelift == wasm on programs
that construct, project, cross boundaries with, and `match` (binding fields, `binds > 0`) aggregates
and enums. MARV-41 adds arena reclamation for compiler-managed boxes whose lifetime is bounded by a
scalar-carried loop iteration; broader ownership-aware reclamation remains future work. AOT/LLVM
emission is a MARV-10 follow-up.
· **MARV-30** array literals + `len`/index codegen (+ index store), closing the collection side
MARV-9 left open. An array literal `[e0, …]` lowers to a new structural `Core::Array { elem, items }`
node (the spec's `Core` has no nominal hash for a `[N]T`, so arrays carry their element type
directly) and boxes to a `[len, e0, …]` block — the **length** lives in the header word where a
`struct`/`enum` keeps its tag, so `len(a)` is one header load and `a[i]` loads `[i + 1]` in both
native backends; the interpreter reuses `Value::Agg` as the oracle. The index *store* `a[i] = e`
(deferred MARV-4 → MARV-9 → here) is a functional element update under mutable value semantics: with
the array's length statically known it rebuilds the array, taking the written value at position `i`
and the old element elsewhere — reusing the array-read + two-arm `bool` `Match` machinery, so it
needs no new backend primitive. `tests/run/arrays.mv` (literal/index/`len`/loop/store) asserts
interp == Cranelift == wasm.
· **MARV-33** runtime-length slices `[]T`, extending MARV-30 from fixed-length arrays. A slice
shares the array's boxed `[len, e0, …]` layout — only its length is a runtime value — so `len`/index
fall straight out of the array codegen, and a fixed-length array now **coerces** to a slice
(`compatible`/`coerces_to` in the checker: `[N]T` → `[]T`, also through a second-class reference
`&[N]T` → `&[]T`), which is how a `[]T` binding/parameter receives an array literal. The element
*store* `s[i] = e` cannot use MARV-30's static unroll (the length is unknown), so it lowers to a new
`Core::IndexSet { base, index, value }` node: the backends read the element count from the header,
allocate a fresh `[len, …]` block, copy it with a **runtime loop**, and overwrite element `i` — a
functional update under mutable value semantics, leaving the source block untouched (`spec/01` §4);
the interpreter clones the `Value::Agg` fields as the oracle. `tests/run/slices.mv`
(literal/index/`len`/`while`-loop/store + a `total` over a slice of structs) and both `examples/`
demos assert interp == Cranelift == wasm. This also makes `examples/report.mv`'s `total` shape
(a `while` over `len(sales)` reading `sales[i].amount`) runnable, closing the slice half of MARV-20.
The debug Tier-1 bounds check on a runtime index landed as MARV-34.
· **MARV-34** Tier-1 debug bounds check on runtime array/slice indexing, closing MARV-33's
follow-up. A runtime subscript outside `0..len` on an element read `a[i]`/`s[i]` or a slice element
store `s[i] = e` (`Core::IndexSet`) now **aborts** in debug builds instead of trapping or touching
adjacent memory — one *unsigned* compare against the `[len, e0, …]` header word (covering negative
subscripts too). The interpreter reports a structured `RunError::BoundsCheckFailed { index, len }`
(like `requires`/`invariant` violations); Cranelift calls a `marv_rt_bounds_fail(index, len)` host
hook that prints the same report and aborts the process; WASM emits an `unreachable` trap (an abort
hook would be a host *import*, breaking the pure-module-imports-nothing manifest). Both codegen
crates grew `compile_with(…, Options { bounds_checks })` and the CLI a `marv build --release` flag
that omits the check — release in-bounds codegen is byte-identical to before. The differential
harnesses now carry an out-of-bounds corpus asserting all three backends abort (Cranelift via a
re-spawned child process, since the abort is process-fatal). One honest gap: a *fixed-length array*
store with a runtime index is unrolled at lowering into per-element selects, so an out-of-range
index there silently no-ops (memory-safely) on all three backends — guarding it means changing the
lowering (and every in-bounds program's Core hash), so it stays a follow-up.
· **MARV-20** `for` execution over slices: with MARV-30/33 supplying the `len`/index runtime,
the `for` desugar runs end to end with no further front-end work. The differential corpora now
pin `for` over a `[]i64` slice, `for` over a slice of structs (the `report.mv` `total` shape),
nested `for`s (the builder-depth-keyed `#for<d>` index names stay unique), and two sequential
`for`s in one block (same depth ⇒ same index name; the second shadows the first harmlessly).
`examples/slices.mv` gained a `for`-based `sum_for`; interp == Cranelift == wasm throughout.
· **MARV-8** reachability-pruned builds: `marv build` compiles **only the definitions reachable
from the entry point**, so a module that mixes backend-supported functions with not-yet-supported
ones builds as long as the entry never references the unsupported ones (the M4/M5 annoyance —
`examples/geometry.mv`'s `max` now builds and runs on both backends despite its sibling
`translate`, whose method call doesn't lower yet). `marv_core::reach::reachable_mask` resolves the
entry exactly as the backends do at call time (explicit `--entry`, bare or qualified, else `main`,
else the sole function) and walks its transitive closure over the same `Global`/`Nominal` edges
the content store links (`collect_global_syms` moved from `marv-store::resolve` into
`marv-core::reach` and is shared by both). Both codegen crates gained
`compile_reachable(…, entry)`; the wasm artifact exports only the pruned closure. When no entry
resolves, the whole module compiles (pre-MARV-8 behavior, same `NoSuchEntry`/unsupported errors);
whole-module `compile`/`compile_with` remains the API for `commit`/audit flows and the
differential corpus, and the checker still checks every definition — pruning is codegen-only.
`tests/run/pruned_sibling.mv` pins the behavior in both backend harnesses.

## Full application language wave (MARV-48)

**MARV-48** is the next umbrella after MARV-40. MARV-40 made dynamic,
heap-backed application logic possible by landing `Alloc`, `List[T]`, string
manipulation, List/string verification, and three app-shaped examples. MARV-48
tracks the remaining pieces needed for ordinary application boundaries: package
discovery, bytes/UTF-8, the first HTTP request capability/host ABI slice, JSON,
the first scoped `Spawn` task-handle slice, unsafe audit metadata, hash-backed
scalar collection paths, and generic non-recursive ADT verification are now done.
Post-MARV-48 work includes production server/listener runtime support, raw FFI
operations, package/query polish, and deeper verification.

The first implementation wave kept scope narrow and is now complete; the next production
wave is tracked by MARV-62 through MARV-74. Close-once resource capability safety landed in
MARV-64, and the first listener-accepted HTTP router slice landed in MARV-63:

1. ~~**MARV-60**~~ ✅ — keep this roadmap and status docs aligned with the tracker.
2. ~~**MARV-49**~~ ✅ — make non-`std` project/package/module discovery real. MARV-14
   already delivered the pinned content-addressed store; MARV-49 is the
   developer-facing source/project layer above it.
3. ~~**MARV-54**~~ ✅ — add practical bytes + UTF-8 utilities, because file/network/HTTP
   boundaries need byte payloads before JSON or HTTP can be honest.
4. ~~**MARV-53**~~ ✅ — add the HTTP/server capability and host ABI story; its listener/router
   runtime slice continued as MARV-63 after MARV-64's linear resource lifecycle slice.

The rest of the MARV-48 wave is also landed: ~~**MARV-50**~~ ✅ first-slice
maps/sets, ~~**MARV-51**~~ ✅ collection literals, ~~**MARV-52**~~ ✅ iterators,
~~**MARV-55**~~ ✅ JSON, ~~**MARV-56**~~ ✅ `Spawn`, ~~**MARV-57**~~ ✅ unsafe
audit metadata, ~~**MARV-58**~~ ✅ loop early return, ~~**MARV-59**~~ ✅ generic
non-recursive ADT verification, and ~~**MARV-61**~~ ✅ hash-backed scalar-key
maps/sets.

## Recommended order

**The spine** — the critical path to "you can write non-trivial programs in marv," in order:

```
MARV-1 enums+match ✅  →  MARV-4 construction/mutation ✅  →  MARV-2 loops ✅  →  MARV-3 error handling ✅
```

Each turns the language from "integer functions" into something progressively more real, and
they unblock the rest. Then:

- **Surface breadth:** ~~MARV-7~~ ✅, ~~MARV-5 generics~~ ✅, ~~MARV-6 capabilities-from-source~~ ✅
  (which closed the last big gap between the design and what real `.mv` can express).
- **Compounds on the surface:** ~~MARV-9 aggregate codegen~~ ✅ → MARV-10; and
  ~~MARV-11 verification expansion~~ ✅.
- **Application/runtime wave:** MARV-48 is done: MARV-49 through MARV-61 covered
  project/source-module discovery, maps/sets, collection literals, iterators,
  bytes/UTF-8, HTTP request capability/host ABI, JSON, `Spawn`, unsafe audit
  metadata, loop early return, generic non-recursive ADT verification, and the
  scalar hash-backed Map/Set path.
- **Longer horizon:** MARV-71 through MARV-74 self-hosting; MARV-39 trap-freedom verification;
  richer package/version metadata on top of MARV-67 package manifests and the
  MARV-14 pinned store.

**Parallel track (no surface dependency — pick up anytime):** ~~MARV-8 (reachability-pruned
builds)~~ ✅ and ~~MARV-12 (doc-comments + spans)~~ ✅ are both done — the track is clear.

## Phases

- **Phase 1 · Surface.** Grow what the parser/lowering accept. The single biggest unblocker;
  most other work compounds on it. Today's parsed subset: `mod`/`import`,
  `struct`/`enum`/`fn`/`interface`/`impl` (incl. `pure fn`, generic parameter lists with
  interface bounds `[T: Ord]`, generic `struct`/`enum`), `let`/`var`, assignment (`lvalue = e`,
  incl. `p.x = e`), `if`/`else`, `match` (constructor + `_` patterns, payload binding), enum
  constructor application, struct literals (`Name { f: e, … }`), index expressions (`a[i]`),
  `while`/`for` loops with `invariant` clauses, generic type arguments (`Option[T]`), the binary
  operators, calls/recursion (incl. monomorphized generic calls), field projection, and
  `requires`/`ensures` contracts.
- **Phase 2 · Backends.** Reachability-pruned builds (**done**, MARV-8); runtime layout for
  aggregates/enums across all three backends in lockstep; then AOT/LLVM/WASM-component packaging.
- **Phase 3 · Verification.** Extend the Tier-2 verified subset (ADTs, arrays, bounded
  quantifiers, sound integer division) and loop invariants (Tier 1 + Tier 2), keeping every
  gap an honest `unsupported`.
- **Phase 4 · Self-hosting & store.** Port compiler passes to marv (Stage-1, differential vs
  the Rust Stage-0 oracle) as the surface allows. The pinned content store is done (MARV-14);
  developer-facing source and package discovery is done through MARV-49 and MARV-67.
- **Phase 5 · Infra/polish.** Doc-comment preservation and real (definition-granular) source
  spans through to diagnostics/`typeAt`/`verify` are **done** (MARV-12). (Phase 0 —
  repo/CI/agent enablement — is also done.)
- **Phase 6 · Full application language.** MARV-48 delivered the next practical layer:
  project/package discovery beyond the special-cased `std` loader, bytes/UTF-8, the first
  HTTP request capability/host ABI slice, JSON/serialization, loop-body early returns, and
  the first scoped `Spawn` task-handle slice, unsafe audit metadata, generic non-recursive ADT
  verification, scalar hash-backed `Map`/`Set` operations, MARV-64's linear resource
  capability lifecycle slice, MARV-63's listener-accepted HTTP router slice, and MARV-66's
  recursive/materialized JSON DOM, and MARV-67's manifest-backed package graph loading.
  Remaining post-MARV-48 work covers host-backed multi-request HTTP serving, raw FFI
  execution/linking, richer package/version metadata, and broader verification.

## How a task is meant to be picked up

Each `MARV-#` task is self-contained: **Goal · Why · Scope (with file paths) · Acceptance ·
Files**, plus spec citations and its blockers. Combined with this map and the repo's
`CLAUDE.md` / `AGENTS.md` / `spec/` / `docs/`, a fresh agent or contributor can take any
unblocked task cold. Respect the non-negotiable invariants (one canonical form, no ambient
authority, no hidden control flow/allocation, local reasoning, determinism) in `CLAUDE.md`
and `spec/01`. Large tasks (MARV-5, MARV-10, MARV-11) should be split into sub-tasks when
started.
