//! # marv-syntax — front end (milestone M0)
//!
//! Lexer, recursive-descent parser, AST, and the canonical formatter. The
//! formatter is the parser's *inverse*: there is exactly one textual form of any
//! program (invariant #1, "one canonical form"). See `spec/02-grammar-and-core-ir.md`
//! §§A–B (lexical + surface grammar) and `spec/01-design-spec.md` §2.
//!
//! Acceptance gate (M0): the round-trip property `parse ∘ format == id` holds
//! over generated in-subset ASTs (`tests/roundtrip.rs`), and the formatter is
//! idempotent.
//!
//! ## Scope
//!
//! M0 implements a *bounded but real* subset of the grammar, end to end
//! (lex → parse → format): module headers, imports, `struct` and `fn`
//! declarations (`pure fn` / `linear struct`), a small type language
//! (named, `[]T` slices, `&`/`&mut` references, `()` unit), block bodies,
//! `let`/`var`/`return` statements, and value expressions (int/bool/str
//! literals, paths, field access, calls, binary operators, `if`/`else`). See
//! [`ast`] for the precise shape and the two ambiguities designed out of it.
//!
//! ## The hybrid [`format`]
//!
//! [`format`] parses its input and reprints it canonically. Input that is
//! *outside* the M0 subset (or otherwise unparseable) falls back to a
//! whitespace-only canonicalizer — the conservative normalization the front end
//! shipped before the parser existed. As later milestones widen the parsed
//! subset, more programs take the full parse-and-reprint path. Callers that want
//! to require parsing should use [`parse`] + [`format_module`] directly.

pub mod ast;
mod formatter;
mod lexer;
mod parser;

pub use ast::*;
pub use formatter::format_module;
pub use parser::{parse, parse_with_spans, ParseError};

/// Width, in spaces, that a hard tab expands to in the whitespace fallback.
const TAB_WIDTH: usize = 4;

/// Normalize `src` toward canonical form.
///
/// If `src` parses as an M0 module it is reprinted in canonical form (the true
/// `format ∘ parse` formatter); otherwise it falls back to the whitespace-only
/// canonicalizer ([`canonicalize_whitespace`]). The result is always
/// deterministic and idempotent.
pub fn format(src: &str) -> String {
    match parser::parse(src) {
        Ok(module) => formatter::format_module(&module),
        Err(_) => canonicalize_whitespace(src),
    }
}

/// The whitespace-only fallback: normalize line endings, expand tabs, strip
/// trailing whitespace, collapse blank-line runs, drop leading blank lines, and
/// guarantee a single trailing newline. It does not parse or reflow code, so it
/// is the safe normalization for input the parser does not (yet) accept.
pub fn canonicalize_whitespace(src: &str) -> String {
    let unified = src.replace("\r\n", "\n").replace('\r', "\n");

    let mut out = String::with_capacity(unified.len());
    let mut blank_run = 0usize;

    for line in unified.split('\n') {
        let expanded = line.replace('\t', &" ".repeat(TAB_WIDTH));
        let trimmed = expanded.trim_end();

        if trimmed.is_empty() {
            blank_run += 1;
            continue;
        }

        if blank_run > 0 && !out.is_empty() {
            out.push('\n');
        }
        blank_run = 0;

        out.push_str(trimmed);
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{canonicalize_whitespace, format};

    #[test]
    fn fallback_strips_trailing_whitespace_and_normalizes_newlines() {
        // Not a module (no `mod`), so this exercises the whitespace fallback.
        assert_eq!(format("a  \r\nb\t\n"), "a\nb\n");
    }

    #[test]
    fn fallback_collapses_blank_runs_and_trims_file_edges() {
        assert_eq!(format("\n\nx\n\n\n\ny\n\n"), "x\n\ny\n");
    }

    #[test]
    fn fallback_expands_tabs_to_spaces() {
        assert_eq!(format("\tx\n"), "    x\n");
    }

    #[test]
    fn fallback_is_idempotent() {
        let once = canonicalize_whitespace("\n\nfoo  \n\t bar\r\n\n");
        assert_eq!(canonicalize_whitespace(&once), once);
    }

    #[test]
    fn parses_and_reprints_a_small_module() {
        // Messy but in-subset input is normalized: parentheses inserted around
        // the binary node, indentation fixed, blank lines collapsed.
        let src = "mod demo\n\nfn add(a: i32, b: i32) -> i32 {\n  a+b\n}\n";
        let expected = "mod demo\n\nfn add(a: i32, b: i32) -> i32 {\n    (a + b)\n}\n";
        assert_eq!(format(src), expected);
        // Idempotent: formatting canonical output is a no-op.
        assert_eq!(format(expected), expected);
    }
}
