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
| **MARV-6** capabilities/`perform` from source | 1 · Surface | MARV-5, MARV-3 | — | medium |
| ~~**MARV-7** scalars & collections (str/char, slices/arrays, `as`)~~ ✅ done | 1 · Surface | — (pairs w/ 4) | — | medium |
| **MARV-8** reachability-pruned compilation | 2 · Backends | — *(independent)* | — | medium |
| **MARV-9** aggregate/enum codegen (interp + Cranelift + WASM) | 2 · Backends | MARV-1, MARV-4 | 10 | medium |
| **MARV-10** AOT native + LLVM + WASM component/WIT | 2 · Backends | MARV-9 | — | low |
| **MARV-11** verified-subset expansion + loop invariants + `old`/quantifiers | 3 · Verification | MARV-2, MARV-1 | — | medium |
| ~~**MARV-12** formatter doc-comments + real source spans~~ ✅ done | 5 · Infra/polish | — *(independent)* | — | medium |
| **MARV-13** port more compiler passes to marv (self-hosting) | 4 · Self-hosting | Phase-1 surface *(incremental now)* | — | low |
| **MARV-14** persistent on-disk store + cross-module resolution | 4 · Store | — *(std linking wants Phase 1)* | — | low |

Done (Phase 0 · Infra/agent): **MARV-15** repo housekeeping · **MARV-16** CI/CD + release ·
**MARV-17** agent enablement (AGENTS.md, MCP server, skill).
Done (Phase 1 · Surface): **MARV-1** enums + `match` (surface parser + lowering →
`Ctor`/`Match`; generic parameter lists + type arguments; `std/option`+`std/result` now parse
and lower; `examples/color.mv` runs) · **MARV-4** construction/mutation (struct literals →
`Ctor`, index `a[i]` → `Prim{Index}`, assignment `lvalue = e` and `var` reassignment under
mutable value semantics — rebinding in ANF, field updates rebuild the aggregate;
`examples/mutation.mv` runs). Index *store* `a[i] = e` is deferred to MARV-9 (array/slice
store) · **MARV-2** `while`/`for` loops → `Core::Loop` (loop-carried `var`s threaded as the
node's `state`, body yields the next-state tuple, the loop yields the final tuple; Tier-1
`invariant` checking in the interpreter; SSA loop blocks in Cranelift + WASM via compile-time
register/local tuples; `examples/loops.mv` runs and agrees across all three backends). `for`
parses + desugars to an index loop but awaits slice/`len` (MARV-7) to execute; loop bodies that
end in an `if`/`match`/`return` await branch-join lowering; Tier-2 SMT for invariants is MARV-11
· **MARV-3** error handling (`error E { … }` decls, `!T`/bare-`!` error unions → `Result[T,
error-union]`, `E.Variant` → `Core::Raise`, postfix `?`; **full cross-call error-set inference**
via a fixpoint over the call graph in `marv-db`, surfaced through `marv/errorSet`; exhaustive
`match` over a caught error value; `examples/errors.mv` checks clean). `?` is a success-value
pass-through (errors propagate by unwinding, so error programs run on the interpreter);
capability-op error sets are MARV-6, cross-*module* propagation is MARV-14, and `Result`-value
codegen is MARV-9.
· **MARV-7** scalars & collections (`char` literals `'a'`/`'\n'`; `as` casts → a new
`Core::Cast { value, to }` node carrying the target type — scalar↔scalar legality checked
(`E0104`), constant-narrowing rejected statically, integer width truncation/wrapping run
*identically across interpreter + Cranelift + WASM* and differential-tested in
`tests/run/casts.mv`; fixed-array type `[N]T` parses; `len(x)` → `Prim{Len}` as a builtin;
`examples/casts.mv` runs and checks clean). The value domain is still 64-bit, so sub-width
semantics surface only at the cast boundary — per-width **arithmetic** wrapping, array/slice
*literals*, index *stores*, and backend `len`/index over aggregates remain MARV-9.
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

## Recommended order

**The spine** — the critical path to "you can write non-trivial programs in marv," in order:

```
MARV-1 enums+match ✅  →  MARV-4 construction/mutation ✅  →  MARV-2 loops ✅  →  MARV-3 error handling ✅
```

Each turns the language from "integer functions" into something progressively more real, and
they unblock the rest. Then:

- **Surface breadth:** ~~MARV-7~~ ✅, ~~MARV-5 generics~~ ✅ → MARV-6 capabilities-from-source
  (which closes the last big gap between the design and what real `.mv` can express).
- **Compounds on the surface:** MARV-9 aggregate codegen (after 1 + 4) → MARV-10; and
  MARV-11 verification expansion (after 2).
- **Longer horizon:** MARV-13 more self-hosting, MARV-14 persistent store.

**Parallel track (no surface dependency — pick up anytime):** MARV-8 (reachability-pruned
builds) and MARV-12 (doc-comments + spans). Good independent work to run alongside the spine.

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
- **Phase 2 · Backends.** Reachability-pruned builds; runtime layout for aggregates/enums
  across all three backends in lockstep; then AOT/LLVM/WASM-component packaging.
- **Phase 3 · Verification.** Extend the Tier-2 verified subset (ADTs, arrays, bounded
  quantifiers, sound integer division) and loop invariants (Tier 1 + Tier 2), keeping every
  gap an honest `unsupported`.
- **Phase 4 · Self-hosting & store.** Port compiler passes to marv (Stage-1, differential vs
  the Rust Stage-0 oracle) as the surface allows; mature the content store into a real
  cross-module package system.
- **Phase 5 · Infra/polish.** Doc-comment preservation and real (definition-granular) source
  spans through to diagnostics/`typeAt`/`verify` are **done** (MARV-12). (Phase 0 —
  repo/CI/agent enablement — is also done.)

## How a task is meant to be picked up

Each `MARV-#` task is self-contained: **Goal · Why · Scope (with file paths) · Acceptance ·
Files**, plus spec citations and its blockers. Combined with this map and the repo's
`CLAUDE.md` / `AGENTS.md` / `spec/` / `docs/`, a fresh agent or contributor can take any
unblocked task cold. Respect the non-negotiable invariants (one canonical form, no ambient
authority, no hidden control flow/allocation, local reasoning, determinism) in `CLAUDE.md`
and `spec/01`. Large tasks (MARV-5, MARV-10, MARV-11) should be split into sub-tasks when
started.
