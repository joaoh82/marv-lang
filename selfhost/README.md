# selfhost/ — Stage-1 self-hosting (M7)

The bootstrap plan (see the project `CLAUDE.md`): Stage 0 is the compiler in
Rust; once marv can compile itself, the compiler is rewritten in marv and
self-hosts, with the Rust Stage-0 compiler kept permanently as a
**differential-testing oracle**.

This directory holds the **first self-hosting steps**: Stage-0 compiler/runtime
structures and routines re-implemented in marv, with each executable slice kept
small enough to test against the Rust compiler.

| File | The pass |
|------|----------|
| [`prim_eval.mv`](prim_eval.mv) | The interpreter's total-primitive kernel (`marv-interp`'s `eval_prim`): given a `PrimOp` tag and two operands, compute the result. |
| [`model.mv`](model.mv) | Stage-1 data model for source spans, tokens, type syntax, parsed AST nodes, Core nodes, diagnostics, and representative AST/Core samples. |

`crates/marv-interp/tests/selfhost.rs` runs the marv `eval_prim` through the
interpreter and asserts it matches the **Rust Stage-0 kernel** (the real
interpreter evaluating a `Core::Prim`) across every primitive and the exact
operations the M4 corpus performs.

The same test target parses, canonically formats, lowers, checks, and runs
`model.mv`'s representative AST/Core scoring helpers. This is not a parser,
lowerer, checker, or self-compile claim yet; it is the data shape MARV-72 and
MARV-73 can build on.

## Stage-1 model mapping

`model.mv` intentionally mirrors the Rust compiler model at the level needed by
the next self-hosting slices, while staying inside today's checked marv surface.

| Rust Stage-0 area | marv model | Notes |
|-------------------|------------|-------|
| Source spans and diagnostics | `SourceSpan`, `Severity`, `Diagnostic` | Byte spans mirror the current definition-granular span shape. Line/column derivation, related notes, and fix payloads stay parser/checker follow-ups. |
| Lexer output | `TokenKind`, `Token` | Token payloads use strings/integers for identifiers, keywords, literals, and symbols; exact lexer trivia is deferred. |
| Surface type syntax | `TypeKind`, `TypeNode`, `EffectSyntax` | Recursive edges are arena indices. Effect rows keep capability/error names as lists. |
| Parsed AST | `ImportDecl`, `Param`, `AstField`, `AstVariant`, `AstArm`, `AstItemKind`, `AstExprKind`, `AstModule` | AST/Core repeated edges use arena counts/ranges rather than embedding `List[T]` inside enum payloads. This keeps the executable examples within the current generic checker while preserving the parser/lowering shape. |
| Core IR | `CoreAtom`, `CoreBranch`, `CoreNodeKind`, `CoreNode`, `CoreDef`, `CoreModule` | Core nodes are arena-indexed. Names and scalar tags stand in for Stage-0 hashes/registries until lowering/checker slices own those tables. |

The representative helpers construct a parsed-module sample and a lowered-Core
sample, then score/traverse concrete nodes. They deliberately avoid claiming
generic list-pool indexing for user-defined node types until that surface is
needed and checked directly by later tickets.

## Why this pass first

It was the largest piece of the compiler expressible in the early parsed subset
(integer/boolean functions, `if`/`else`, recursion), and it remains a compact
oracle-friendly kernel. The surface now includes enums, generics, pattern matching,
collections, capabilities, and more application-language pieces, so later Stage-1
work can move larger AST/Core passes into this directory. Each port should stay
differentially tested against Stage 0.
