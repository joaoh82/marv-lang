//! Hand-written lexer for the M0 subset (`spec/02-grammar-and-core-ir.md` §A).
//!
//! Whitespace within a line is insignificant, but **newlines are significant**:
//! a run of one or more line breaks collapses to a single [`Tok::Nl`] token,
//! which the parser uses as a statement separator. This is what makes the
//! grammar bijective without semicolons — e.g. `let a = foo` followed by a tail
//! `(x)` reads as two block elements, not as the call `foo(x)`, because a `Nl`
//! sits between them and the expression parser stops at it.
//!
//! Leading newlines and `//` line comments are dropped. **Doc comments (`///`)**
//! are kept — they lex to a [`Tok::Doc`] carrying the comment text and attach to
//! the following item in the AST (`spec/02` §D). They remain excluded from a
//! definition's content hash (`spec/02` §F — not part of identity); only the
//! formatter and the AST preserve them.
//!
//! ## Spans (MARV-12)
//!
//! [`lex_spanned`] returns, alongside the token stream, a parallel vector of
//! `(start_byte, end_byte)` UTF-8 byte ranges — one per token. The parser carries
//! these through so a definition's header, signature, and capability-insertion
//! point have real source offsets, which the checker's diagnostics, `marv/typeAt`,
//! `marv/verify`, and `marv/applyFix` then report (`spec/03` §2). They are *source*
//! spans only; the Core IR still carries none (it is the names-erased identity).

/// A lexical token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    // Keywords (the M0 subset of `spec/02` §A `keyword`).
    Mod,
    Import,
    Fn,
    Pure,
    Unsafe,
    Linear,
    Struct,
    Enum,
    Error,
    Interface,
    Impl,
    Match,
    Let,
    Var,
    Return,
    If,
    Else,
    While,
    For,
    In,
    True,
    False,
    Mut,
    And,
    Or,
    Not,
    As,
    /// `forall` — contract-only bounded quantifier (`spec/02` §B `quant_expr`).
    Forall,
    /// `exists` — contract-only bounded quantifier (`spec/02` §B `quant_expr`).
    Exists,

    Ident(String),
    Int(i64),
    Str(String),
    Char(char),

    /// A doc comment line (`/// text`), carrying the text after `///` with a
    /// single leading space stripped and trailing whitespace trimmed. Attaches
    /// to the item that follows it (`spec/02` §D); excluded from content hashes.
    Doc(String),

    // Punctuation / delimiters.
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    DotDot,   // .. — quantifier domain range `lo..hi`
    Arrow,    // ->
    FatArrow, // =>
    Eq,       // =
    Amp,      // &

    // Binary operators.
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    Bang,     // `!` — error-union type prefix (`!T`)
    Question, // `?` — postfix error-propagation operator and optional-type prefix (`?T`)

    /// A collapsed run of one or more line breaks (significant separator).
    Nl,
    Eof,
}

/// An error raised while tokenizing. Any lex error makes [`crate::parse`] fail,
/// which the hybrid [`crate::format`] turns into a graceful whitespace-only
/// fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
}

impl LexError {
    fn new(message: impl Into<String>) -> Self {
        LexError {
            message: message.into(),
        }
    }
}

fn keyword(word: &str) -> Option<Tok> {
    Some(match word {
        "mod" => Tok::Mod,
        "import" => Tok::Import,
        "fn" => Tok::Fn,
        "pure" => Tok::Pure,
        "unsafe" => Tok::Unsafe,
        "linear" => Tok::Linear,
        "struct" => Tok::Struct,
        "enum" => Tok::Enum,
        "error" => Tok::Error,
        "interface" => Tok::Interface,
        "impl" => Tok::Impl,
        "match" => Tok::Match,
        "let" => Tok::Let,
        "var" => Tok::Var,
        "return" => Tok::Return,
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "for" => Tok::For,
        "in" => Tok::In,
        "true" => Tok::True,
        "false" => Tok::False,
        "mut" => Tok::Mut,
        "and" => Tok::And,
        "or" => Tok::Or,
        "not" => Tok::Not,
        "as" => Tok::As,
        "forall" => Tok::Forall,
        "exists" => Tok::Exists,
        _ => return None,
    })
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

/// The token stream plus a parallel vector of `(start_byte, end_byte)` byte
/// ranges — one entry per token (see [`lex_spanned`]).
pub type SpannedTokens = (Vec<Tok>, Vec<(u32, u32)>);

/// Tokenize `src`, returning the token stream and a parallel vector of
/// `(start_byte, end_byte)` UTF-8 byte ranges — one entry per token, including
/// the terminating [`Tok::Eof`] (a zero-width span at end of input). The parser
/// threads these so definitions carry real source spans (MARV-12).
pub fn lex_spanned(src: &str) -> Result<SpannedTokens, LexError> {
    let chars: Vec<char> = src.chars().collect();
    // Byte offset of each char (and the total at the end), so a token spanning
    // char indices `[a, b)` has byte range `(byte_of[a], byte_of[b])`.
    let mut byte_of: Vec<u32> = Vec::with_capacity(chars.len() + 1);
    let mut b = 0u32;
    for c in &chars {
        byte_of.push(b);
        b += c.len_utf8() as u32;
    }
    byte_of.push(b);

    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();
    let mut spans: Vec<(u32, u32)> = Vec::new();

    while i < chars.len() {
        let c = chars[i];

        match c {
            ' ' | '\t' => {
                i += 1;
            }
            '\r' => {
                i += 1; // handle the matching '\n' (if any) on its own
            }
            '\n' => {
                // Push a `Nl`, collapsing runs and never emitting a leading one.
                if !out.is_empty() && out.last() != Some(&Tok::Nl) {
                    out.push(Tok::Nl);
                    spans.push((byte_of[i], byte_of[i + 1]));
                }
                i += 1;
            }
            // A doc comment `/// text` (but not `////…`, which is an ordinary
            // comment): keep it as a `Doc` token. `//` and `////+` are dropped.
            '/' if is_doc_comment(&chars, i) => {
                let start = i;
                i += 3; // past `///`
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                let raw: String = chars[start + 3..i].iter().collect();
                // Strip a single leading space and trailing whitespace so the
                // text round-trips through the canonical `/// text` spelling.
                let text = raw.strip_prefix(' ').unwrap_or(&raw).trim_end().to_string();
                out.push(Tok::Doc(text));
                spans.push((byte_of[start], byte_of[i]));
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                // Ordinary line comment (`//` or `////…`): skip to end of line;
                // the newline itself is handled on the next iteration.
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '"' => {
                let start = i;
                let (s, next) = lex_string(&chars, i)?;
                out.push(Tok::Str(s));
                spans.push((byte_of[start], byte_of[next]));
                i = next;
            }
            '\'' => {
                let start = i;
                let (c, next) = lex_char(&chars, i)?;
                out.push(Tok::Char(c));
                spans.push((byte_of[start], byte_of[next]));
                i = next;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                let (n, next) = lex_int(&chars, i)?;
                out.push(Tok::Int(n));
                spans.push((byte_of[start], byte_of[next]));
                i = next;
            }
            c if is_ident_start(c) => {
                let start = i;
                i += 1;
                while i < chars.len() && is_ident_continue(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                out.push(keyword(&word).unwrap_or(Tok::Ident(word)));
                spans.push((byte_of[start], byte_of[i]));
            }
            _ => {
                let start = i;
                let (tok, next) = lex_punct(&chars, i)?;
                out.push(tok);
                spans.push((byte_of[start], byte_of[next]));
                i = next;
            }
        }
    }

    out.push(Tok::Eof);
    spans.push((b, b));
    debug_assert_eq!(out.len(), spans.len());
    Ok((out, spans))
}

/// Whether the `/` at index `i` opens a doc comment: exactly `///` not followed
/// by a fourth `/` (so `////…` is an ordinary comment, matching Rust).
fn is_doc_comment(chars: &[char], i: usize) -> bool {
    chars.get(i) == Some(&'/')
        && chars.get(i + 1) == Some(&'/')
        && chars.get(i + 2) == Some(&'/')
        && chars.get(i + 3) != Some(&'/')
}

/// Lex a string literal starting at the opening `"` (index `start`). Returns the
/// unescaped contents and the index just past the closing quote.
fn lex_string(chars: &[char], start: usize) -> Result<(String, usize), LexError> {
    let mut i = start + 1; // skip opening quote
    let mut s = String::new();

    while i < chars.len() {
        match chars[i] {
            '"' => return Ok((s, i + 1)),
            '\\' => {
                let esc = chars
                    .get(i + 1)
                    .ok_or_else(|| LexError::new("unterminated escape in string literal"))?;
                let decoded = match esc {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '\\' => '\\',
                    '"' => '"',
                    other => {
                        return Err(LexError::new(format!("unknown string escape `\\{other}`")))
                    }
                };
                s.push(decoded);
                i += 2;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }

    Err(LexError::new("unterminated string literal"))
}

/// Lex a character literal starting at the opening `'` (index `start`). Returns
/// the single Unicode scalar it denotes and the index just past the closing `'`.
/// The same escapes as a string literal are accepted, plus `\'`.
fn lex_char(chars: &[char], start: usize) -> Result<(char, usize), LexError> {
    let mut i = start + 1; // skip opening quote
    let c = *chars
        .get(i)
        .ok_or_else(|| LexError::new("unterminated character literal"))?;
    let value = if c == '\\' {
        let esc = chars
            .get(i + 1)
            .ok_or_else(|| LexError::new("unterminated escape in character literal"))?;
        let decoded = match esc {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '\\' => '\\',
            '\'' => '\'',
            '"' => '"',
            '0' => '\0',
            other => {
                return Err(LexError::new(format!(
                    "unknown character escape `\\{other}`"
                )))
            }
        };
        i += 2;
        decoded
    } else if c == '\'' {
        return Err(LexError::new("empty character literal"));
    } else {
        i += 1;
        c
    };
    if chars.get(i) != Some(&'\'') {
        return Err(LexError::new(
            "unterminated character literal (expected a closing `'`)",
        ));
    }
    Ok((value, i + 1))
}

/// Lex an integer literal. Underscores between digits are accepted and dropped
/// (so `1_000` lexes to `1000`); the canonical form emits no underscores.
fn lex_int(chars: &[char], start: usize) -> Result<(i64, usize), LexError> {
    let mut i = start;
    let mut digits = String::new();

    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
        if chars[i] != '_' {
            digits.push(chars[i]);
        }
        i += 1;
    }

    let value = digits
        .parse::<i64>()
        .map_err(|_| LexError::new(format!("integer literal out of range: {digits}")))?;
    Ok((value, i))
}

/// Lex a punctuation/operator token with maximal munch.
fn lex_punct(chars: &[char], i: usize) -> Result<(Tok, usize), LexError> {
    let two = |a: char, b: char| chars.get(i) == Some(&a) && chars.get(i + 1) == Some(&b);

    if two('-', '>') {
        return Ok((Tok::Arrow, i + 2));
    }
    if two('=', '>') {
        return Ok((Tok::FatArrow, i + 2));
    }
    if two('=', '=') {
        return Ok((Tok::EqEq, i + 2));
    }
    if two('!', '=') {
        return Ok((Tok::BangEq, i + 2));
    }
    if two('<', '=') {
        return Ok((Tok::Le, i + 2));
    }
    if two('>', '=') {
        return Ok((Tok::Ge, i + 2));
    }
    if two('.', '.') {
        return Ok((Tok::DotDot, i + 2));
    }

    let tok = match chars[i] {
        '(' => Tok::LParen,
        ')' => Tok::RParen,
        '{' => Tok::LBrace,
        '}' => Tok::RBrace,
        '[' => Tok::LBracket,
        ']' => Tok::RBracket,
        ',' => Tok::Comma,
        ':' => Tok::Colon,
        '.' => Tok::Dot,
        '=' => Tok::Eq,
        '&' => Tok::Amp,
        '+' => Tok::Plus,
        '-' => Tok::Minus,
        '*' => Tok::Star,
        '/' => Tok::Slash,
        '%' => Tok::Percent,
        '<' => Tok::Lt,
        '>' => Tok::Gt,
        '!' => Tok::Bang,
        '?' => Tok::Question,
        other => return Err(LexError::new(format!("unexpected character `{other}`"))),
    };
    Ok((tok, i + 1))
}
