# selfhost/ — Stage-1 self-hosting (M7)

The bootstrap plan (see the project `CLAUDE.md`): Stage 0 is the compiler in
Rust; once marv can compile itself, the compiler is rewritten in marv and
self-hosts, with the Rust Stage-0 compiler kept permanently as a
**differential-testing oracle**.

This directory holds the **first self-hosting step**: a genuine Stage-0 routine
re-implemented in marv, proven equivalent to the Rust original.

| File | The pass |
|------|----------|
| [`prim_eval.mv`](prim_eval.mv) | The interpreter's total-primitive kernel (`marv-interp`'s `eval_prim`): given a `PrimOp` tag and two operands, compute the result. |

`crates/marv-interp/tests/selfhost.rs` runs the marv `eval_prim` through the
interpreter and asserts it matches the **Rust Stage-0 kernel** (the real
interpreter evaluating a `Core::Prim`) across every primitive and the exact
operations the M4 corpus performs.

## Why this pass first

It is the largest piece of the compiler expressible in today's parsed subset
(integer/boolean functions, `if`/`else`, recursion). Passes that traverse the
AST or Core need `enum`s, generics, and pattern matching — surface the front end
does not parse yet (`std/` documents those declarations). As the surface grows,
more of the compiler moves here, each step differentially tested against Stage 0.
