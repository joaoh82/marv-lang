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
| [`parser.mv`](parser.mv) | First lexer/parser slice for a tiny `.mv` grammar, producing the `model.mv` token and AST structures and failing unsupported forms with `FrontendError`. |

`crates/marv-interp/tests/selfhost.rs` runs the marv `eval_prim` through the
interpreter and asserts it matches the **Rust Stage-0 kernel** (the real
interpreter evaluating a `Core::Prim`) across every primitive and the exact
operations the M4 corpus performs.

The same test target parses, canonically formats, lowers, checks, and runs
`model.mv`'s representative AST/Core scoring helpers. This is not a parser,
lowerer, checker, or self-compile claim yet; it is the data shape MARV-72 and
MARV-73 can build on.

`parser.mv` starts the next layer up. Its differential harness parses the same
tiny source with Rust Stage 0, checks the expected module/function/type/body
shape, then runs the marv lexer/parser fingerprints through the interpreter.
The harness also asserts that out-of-slice syntax raises `FrontendError` instead
of being accepted with a misleading partial AST.

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

## Stage-1 lexer/parser slice

`parser.mv` is intentionally small and executable. It proves the pass boundary:
source text enters marv code, the lexer emits `TokenKind`/`Token` values, the
parser constructs `AstModule`/`AstItem`/`AstExpr` values from `model.mv`, and the
result is compared against the Rust Stage-0 parser on a supported fixture.

Supported today:

- ASCII identifiers, keywords, integer literals, whitespace skipping, `->`, and
  the punctuation needed by the tiny function grammar.
- A module header: `mod name`.
- One `pure fn` or `fn`, with zero or one `i64` parameter and an `-> i64`
  return type.
- A body whose tail expression is either an integer literal or an identifier.
- Byte-offset spans over the original source.

Known unsupported forms fail honestly with `FrontendError`: imports, comments
and doc comments, strings/chars, structs/enums/errors/interfaces/impls, multiple
items, generics, effects/capability rows, statements, `if`/`match`/loops,
operators, calls, field/index/slice syntax, full line/column diagnostics, and
the complete Stage-0 grammar. The path to full coverage is incremental: widen
the lexer token set, add parser routines one grammar family at a time, keep
unsupported forms typed and explicit, and extend the differential fixtures until
the marv parser can round-trip the same corpus as Rust Stage 0.

## Why this pass first

It was the largest piece of the compiler expressible in the early parsed subset
(integer/boolean functions, `if`/`else`, recursion), and it remains a compact
oracle-friendly kernel. The surface now includes enums, generics, pattern matching,
collections, capabilities, and more application-language pieces, so later Stage-1
work can move larger AST/Core passes into this directory. Each port should stay
differentially tested against Stage 0.
