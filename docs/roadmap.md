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
| ~~**MARV-11** verified-subset expansion + loop invariants + `old`/quantifiers~~ ✅ done — loop-invariant slice landed as **MARV-22** (Hoare-style initiation/consecution/use VCs); the rest extends the contract language to expressions (arithmetic, `len`, indexing, `p.x`), adds surface + Tier-1/Tier-2 `forall`/`exists` over `lo..hi` and `old(e)` in `ensures`, encodes truncate-toward-zero `/` `%` soundly over SMT's Euclidean ops, and encodes arrays/slices (SMT arrays + length) and non-recursive structs/enums (unpacked tag + fields, havocked from the `World`); `examples/quantifiers.mv` proves end to end. Honest residue: calls, floats, casts, recursive/generic ADTs are `unsupported`; fixed-width integer wrapping landed as **MARV-38** | 3 · Verification | ~~MARV-2~~ ✅, ~~MARV-1~~ ✅ | 22 | medium |
| ~~**MARV-38** Tier-2 fixed-width integer wraparound (close the mathematical-integers soundness gap)~~ ✅ done — every `+ - * / %` and unary `-` is reduced through a two's-complement `wrap64` over SMT `Int`s (a bitvector sort was ruled out: nonlinear `div`/`mul` is intractable — the division identity times out as a 64-bit bitvector but discharges in well under a second as wrapped `Int`s), and every havocked int/length is range-constrained to `[i64::MIN, i64::MAX]`; `ensures result > x` for `x + 1` is now refuted at `x = i64::MAX`. Tier 2 is *correctly stricter*: an accumulator claimed `>= 0` whose running sum can overflow (`examples/loops.mv`'s `sum_to`) no longer proves; the bounded `count_down_sum` does | 3 · Verification | ~~MARV-11~~ ✅ | — | medium |
| ~~**MARV-12** formatter doc-comments + real source spans~~ ✅ done | 5 · Infra/polish | — *(independent)* | — | medium |
| **MARV-13** port more compiler passes to marv (self-hosting) | 4 · Self-hosting | Phase-1 surface *(incremental now)* | — | low |
| ~~**MARV-14** persistent on-disk store + cross-module resolution~~ ✅ done | 4 · Store | — *(std linking wants Phase 1)* | — | low |
| **MARV-48** full application language surface + runtime epic | 6 · Application language | MARV-40 | 49–60 | medium |
| ~~**MARV-49** project/source-module discovery beyond `std`~~ ✅ done | 6 · Application language | MARV-14 | package metadata/query polish, 53, 60 | high |
| **MARV-50** `Map[K, V]` and `Set[T]` in `std` | 6 · Std collections | MARV-42 | 51, 55 | medium |
| **MARV-51** collection literals for `List`/`Map`/`Set` | 6 · Surface ergonomics | MARV-42, 50 for map/set forms | — | medium |
| **MARV-52** real `Iter[T]` protocol | 6 · Surface/stdlib | MARV-42 | 50 | medium |
| **MARV-53** HTTP/server runtime capability + host ABI | 6 · Runtime/capabilities | 49, 54, MARV-27 *(for linear resource safety)* | — | high |
| **MARV-54** bytes + UTF-8 stdlib utilities | 6 · Std/runtime | MARV-42, MARV-43 | 53, 55 | high |
| **MARV-55** JSON + serialization stdlib | 6 · Std/app data | 54, 50 *(or list-of-pairs first)* | 53 | medium |
| **MARV-56** capability-gated structured concurrency (`Spawn`) | 6 · Runtime/capabilities | MARV-27 *(if task/channel handles become linear)* | — | medium |
| **MARV-57** `unsafe`/FFI surface + `unsafeSites` audit query | 6 · Audit/escape hatch | MARV-12 | — | medium |
| **MARV-58** early `return` inside loop bodies | 6 · Surface/control flow | MARV-21 | — | low |
| **MARV-59** Tier-2 recursive/generic ADTs | 6 · Verification | MARV-11, MARV-38 | 13 | medium |
| **MARV-60** roadmap/docs cleanup for MARV-48 | 5 · Infra/polish | MARV-48 | 49–59 | low |

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
its narrowing closure** (holding `Io` authorizes the `Fs`/`Net`/… it can narrow to), so a `pure fn`
that performs — or any function reaching a capability it never received — is `MissingCapability`
(E0110) *from source*. The interpreter injects granted caps at the entry boundary and a narrowing
op returns the narrowed capability value; the CLI resolves `import std.*` to the `std/` sources
(transitively) so the capability interfaces are in scope (`MARV_STD` overrides discovery; MARV-49
extends the same source-module loading to local non-`std` imports). `std/capabilities.mv` parses/checks; `examples/hello.mv`
(`io.stdout().write(...)`) and `examples/read_file.mv` (`io.fs()` → `fs.read`) check, infer their
rows, and run under `marv run --grant Io`. Cranelift n/a (rejects `Perform`); WASM lowers a
`perform` to a host import but capability *narrowing* on WASM, and `linear` capabilities (a `Conn`
that must be `close`d), are follow-ups.
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
discovery, richer std collections, bytes/UTF-8, JSON, server/network runtime
capabilities, structured concurrency, `unsafe`/FFI auditability, and deeper
verification.

The first implementation wave should keep scope narrow:

1. **MARV-60** — keep this roadmap and status docs aligned with the tracker.
2. **MARV-49** — make non-`std` project/package/module discovery real. MARV-14
   already delivered the pinned content-addressed store; MARV-49 is the
   developer-facing source/project layer above it.
3. **MARV-54** — add practical bytes + UTF-8 utilities, because file/network/HTTP
   boundaries need byte payloads before JSON or HTTP can be honest.
4. **MARV-53** — add the HTTP/server capability and host ABI story, coordinating
   with **MARV-27** for linear connection/listener lifecycle.

The rest can proceed in parallel where dependencies allow: **MARV-50** maps/sets,
**MARV-51** collection literals, **MARV-52** iterators, **MARV-55** JSON,
**MARV-56** `Spawn`, **MARV-57** `unsafeSites`, **MARV-58** loop early return,
and **MARV-59** recursive/generic ADT verification.

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
- **Application/runtime wave:** MARV-48, starting with MARV-60 docs cleanup,
  MARV-49 project/source-module discovery, MARV-54 bytes/UTF-8, and MARV-53
  HTTP/server runtime capability.
- **Longer horizon:** MARV-13 more self-hosting; MARV-10 AOT/LLVM/component
  packaging; MARV-27 linear capabilities; MARV-39 trap-freedom verification;
  richer package metadata and package-aware agent queries on top of the MARV-49
  source-module discovery and MARV-14 pinned store.

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
  the remaining developer-facing project/package discovery work is tracked in Phase 6 as
  MARV-49.
- **Phase 5 · Infra/polish.** Doc-comment preservation and real (definition-granular) source
  spans through to diagnostics/`typeAt`/`verify` are **done** (MARV-12). (Phase 0 —
  repo/CI/agent enablement — is also done.)
- **Phase 6 · Full application language.** MARV-48 tracks the next practical layer:
  project/package discovery beyond the special-cased `std` loader, `Map`/`Set`, collection
  literals, a real `Iter[T]` protocol, bytes/UTF-8, JSON/serialization, HTTP/server
  capabilities and host ABI, structured concurrency, `unsafe`/FFI auditability, loop early
  returns, and broader Tier-2 ADT verification.

## How a task is meant to be picked up

Each `MARV-#` task is self-contained: **Goal · Why · Scope (with file paths) · Acceptance ·
Files**, plus spec citations and its blockers. Combined with this map and the repo's
`CLAUDE.md` / `AGENTS.md` / `spec/` / `docs/`, a fresh agent or contributor can take any
unblocked task cold. Respect the non-negotiable invariants (one canonical form, no ambient
authority, no hidden control flow/allocation, local reasoning, determinism) in `CLAUDE.md`
and `spec/01`. Large tasks (MARV-5, MARV-10, MARV-11) should be split into sub-tasks when
started.
