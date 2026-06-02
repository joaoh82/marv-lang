//! # marv-syntax — front end (milestone M0)
//!
//! Lexer, recursive-descent parser, AST, and the canonical formatter. The
//! formatter is the parser's *inverse*: there is exactly one textual form of any
//! program (invariant #1, "one canonical form"). See `spec/02-grammar-and-core-ir.md`
//! §§A–B (lexical + surface grammar) and `spec/01-design-spec.md` §2.
//!
//! Acceptance gate (M0): the round-trip property `parse ∘ format == id` holds on
//! all canonical forms (proptest).
//!
//! ## Status
//!
//! The lexer/parser/AST are not implemented yet. What exists today is a minimal
//! **whitespace canonicalizer** — the conservative subset of the formatter that
//! does not yet require a parser: it normalizes line endings, expands tabs,
//! strips trailing whitespace, collapses runs of blank lines, and guarantees a
//! single trailing newline. This lets `marv fmt` do real, deterministic work
//! from the first commit. It will be replaced by the parse-and-reprint formatter
//! once the parser lands.

/// Width, in spaces, that a hard tab expands to in canonical form.
const TAB_WIDTH: usize = 4;

/// Normalize `src` toward canonical form.
///
/// This is the M0 whitespace-only subset of the canonical formatter (see the
/// module docs): it is deterministic and idempotent, but does **not** yet parse
/// or reflow code. The full `parse ∘ format == id` formatter replaces it once
/// the parser exists.
pub fn format(src: &str) -> String {
    // Normalize CRLF / CR to LF, then expand tabs and strip trailing whitespace
    // line by line.
    let unified = src.replace("\r\n", "\n").replace('\r', "\n");

    let mut out = String::with_capacity(unified.len());
    let mut blank_run = 0usize;

    for line in unified.split('\n') {
        let expanded = line.replace('\t', &" ".repeat(TAB_WIDTH));
        let trimmed = expanded.trim_end();

        if trimmed.is_empty() {
            // Collapse any run of blank lines down to a single blank line; the
            // leading blank lines of a file are dropped entirely.
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
    use super::format;

    #[test]
    fn strips_trailing_whitespace_and_normalizes_newlines() {
        assert_eq!(format("a  \r\nb\t\n"), "a\nb\n");
    }

    #[test]
    fn collapses_blank_runs_and_trims_file_edges() {
        assert_eq!(format("\n\nx\n\n\n\ny\n\n"), "x\n\ny\n");
    }

    #[test]
    fn expands_tabs_to_spaces() {
        assert_eq!(format("\tx\n"), "    x\n");
    }

    #[test]
    fn is_idempotent() {
        let once = format("\n\nfoo  \n\t bar\r\n\n");
        assert_eq!(format(&once), once);
    }
}
