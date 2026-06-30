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
| [`lower_check.mv`](lower_check.mv) | First lowering/checker slice for the same tiny grammar, turning the selfhost AST into `CoreModule` data and reporting tiny-scope diagnostics. |
| [`driver.mv`](driver.mv) | First compiler-shaped Stage-1 driver: sequences the tiny parser and lowering/checker slices over a documented corpus and exposes reproducible fingerprints. |

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

`lower_check.mv` consumes that tiny AST and builds the first marv-native
`CoreModule`: one function definition, an `i64`/`() -> i64` arrow type, a lambda
root node, and an integer/local/global atom body. Its harness compares the
supported fixture against Rust Stage-0's lowered Core shape and keeps an
explicit tiny-scope diagnostic path plus `PassError` for unsupported AST forms.

`driver.mv` is the first honest self-compile milestone. It does not compile the
compiler yet; it compiles the **Stage-1 tiny corpus** by running the marv-written
parser and lower/check passes in sequence, while the Rust Stage-0 compiler
remains the oracle that builds and tests those Stage-1 passes.

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

## Stage-1 lowering/checker slice

`lower_check.mv` intentionally tracks the parser slice rather than jumping ahead
of it. Supported today:

- Lower a parsed tiny module containing one function item.
- Represent nullary functions as a synthetic unit-parameter lambda, matching
  Stage 0's curried `fn() -> T` convention.
- Represent one-parameter `i64 -> i64` functions as a `TypeKind.Arrow` and
  `CoreNodeKind.Lam`.
- Lower integer tail expressions to `CoreAtom.Int`, parameter references to
  `CoreAtom.Var(0)`, and unresolved tiny-scope names to `CoreAtom.Global`.
- Return `Diagnostic` values for the tiny checker rule it owns, currently an
  unknown local in a nullary function body.

The differential test checks that `pure fn id(n: i64) -> i64 { n }` has the same
Stage-0 Core shape: `i64 -> i64`, lambda parameter `i64`, and body `Var(0)`.
The selfhost fingerprint for that fixture is `1234`, and the diagnostic count
is zero. A nullary body `n` produces one selfhost diagnostic while preserving the
observed Stage-0 lowering behavior (`n` is an unresolved global at Core level).

Unsupported AST forms raise `PassError`: structs, enums, bool/string/call/if/
match/struct-literal expressions, effects/capabilities, error sets, linear-use
analysis, contracts, multi-item modules, real name-resolution diagnostics, and
full type compatibility. Future MARV-73 follow-up slices should widen parser
coverage first when needed, then add lowering and checker rules for the same
fixtures so Stage 1 and Stage 0 stay differentially comparable.

## Stage-1 driver milestone

`driver.mv` defines the first self-hosting milestone precisely:

- **Input corpus:** one tiny module shaped like
  `mod demo; pure fn id(n: i64) -> i64 { n }` (canonical newlines in the test
  fixture).
- **Stage-1 pipeline:** `lex_tiny_fingerprint` + `parse_tiny_fingerprint` from
  `parser.mv`, then `lower_check_tiny_fingerprint` and
  `lower_check_tiny_diagnostics` from `lower_check.mv`.
- **Output:** deterministic integer fingerprints and diagnostic counts that the
  Rust harness compares against Stage-0 parser/lower/check facts.
- **Bootstrap flow:** Rust Stage 0 compiles and interprets the Stage-1 marv
  passes; Stage 1 compiles the tiny corpus; Rust Stage 0 remains the oracle and
  fallback for every unsupported construct.
- **Store flow:** pin the Stage-1 driver and its transitive source imports with
  `marv commit --store <store-dir> selfhost/driver.mv`; the lockfile/store are
  still produced by Rust Stage 0.

Useful smoke commands:

```sh
cargo run -p marv-cli -- check selfhost/driver.mv
cargo run -p marv-cli -- run --grant Alloc selfhost/driver.mv --entry compile_tiny_fingerprint 'mod demo

pure fn id(n: i64) -> i64 {
    n
}
'
cargo run -p marv-cli -- commit --store /tmp/marv-selfhost-store selfhost/driver.mv
```

The supported corpus fingerprint is `1376`; the bootstrap manifest fingerprint
is `1101376`; the supported diagnostic count is `0`; the intentionally bad
nullary body `n` yields diagnostic count `1`. Unsupported parser/lowerer paths
raise `FrontendError`/`PassError`. This is enough for the docs to say marv has
its **first tiny self-compile milestone**, and not enough to claim the compiler
or standard library self-hosts.

## Why this pass first

It was the largest piece of the compiler expressible in the early parsed subset
(integer/boolean functions, `if`/`else`, recursion), and it remains a compact
oracle-friendly kernel. The surface now includes enums, generics, pattern matching,
collections, capabilities, and more application-language pieces, so later Stage-1
work can move larger AST/Core passes into this directory. Each port should stay
differentially tested against Stage 0.
