//! Hand-written lexer for the M0 subset (`spec/02-grammar-and-core-ir.md` §A).
//!
//! Whitespace within a line is insignificant, but **newlines are significant**:
//! a run of one or more line breaks collapses to a single [`Tok::Nl`] token,
//! which the parser uses as a statement separator. This is what makes the
//! grammar bijective without semicolons — e.g. `let a = foo` followed by a tail
//! `(x)` reads as two block elements, not as the call `foo(x)`, because a `Nl`
//! sits between them and the expression parser stops at it.
//!
//! Leading newlines and `//` line comments are dropped. Doc comments (`///`) are
//! not part of identity (`spec/02` §D) and are likewise dropped in M0.

/// A lexical token. Spans are intentionally omitted in M0 — the front end's job
/// here is the round-trip property, not diagnostics (those arrive in M2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    // Keywords (the M0 subset of `spec/02` §A `keyword`).
    Mod,
    Import,
    Fn,
    Pure,
    Linear,
    Struct,
    Enum,
    Error,
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
    As,

    Ident(String),
    Int(i64),
    Str(String),
    Char(char),

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
        "linear" => Tok::Linear,
        "struct" => Tok::Struct,
        "enum" => Tok::Enum,
        "error" => Tok::Error,
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
        "as" => Tok::As,
        _ => return None,
    })
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

/// Tokenize `src` into a flat token stream terminated by [`Tok::Eof`].
pub fn lex(src: &str) -> Result<Vec<Tok>, LexError> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();

    // Push a `Nl`, collapsing runs and never emitting a leading newline.
    let push_nl = |out: &mut Vec<Tok>| {
        if !out.is_empty() && out.last() != Some(&Tok::Nl) {
            out.push(Tok::Nl);
        }
    };

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
                push_nl(&mut out);
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                // Line comment (covers `//` and `///`): skip to end of line; the
                // newline itself is handled on the next iteration.
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '"' => {
                let (s, next) = lex_string(&chars, i)?;
                out.push(Tok::Str(s));
                i = next;
            }
            '\'' => {
                let (c, next) = lex_char(&chars, i)?;
                out.push(Tok::Char(c));
                i = next;
            }
            c if c.is_ascii_digit() => {
                let (n, next) = lex_int(&chars, i)?;
                out.push(Tok::Int(n));
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
            }
            _ => {
                let (tok, next) = lex_punct(&chars, i)?;
                out.push(tok);
                i = next;
            }
        }
    }

    out.push(Tok::Eof);
    Ok(out)
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
