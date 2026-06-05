//! Fix-carrying diagnostics (`spec/03-compiler-protocol.md` §2).
//!
//! The checker does not merely reject — wherever a repair is *mechanically
//! derivable* it ships a [`Fix`] (edits + confidence) alongside the
//! [`Diagnostic`]. The five mechanical cases the M2 gate covers are exactly the
//! ones §2 names: a missing capability parameter, a missing error in the
//! declared set, a non-exhaustive `match`, an unused/duplicated `linear` value,
//! and an escaping reference.
//!
//! ## Spans are optional (scope honesty)
//!
//! `spec/02` §F rule 4 excludes source spans from the Core IR, and the M0 AST
//! does not carry spans either, so a check run over Core today has *no* byte
//! offsets to attach. [`Diagnostic::span`] and [`Edit::span`] are therefore
//! [`Option`]: the diagnostic's `code`/`message`/`fixes` are always populated,
//! and the byte-precise insertion point of an edit fills in once the front end
//! threads spans through lowering (a documented future wiring, mirroring the M1
//! "scope honesty" note). The `new_text` of every fix is always present, so an
//! agent already learns *what* to insert.

use std::fmt;

/// Severity of a [`Diagnostic`] (`spec/03` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    /// The lowercase wire spelling used by the JSON-RPC protocol (`spec/03` §2).
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// A 0-based `{line, col}` position (`spec/03` §2). Column is a UTF-8 code-unit
/// offset within the line, matching the byte offsets the protocol also carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

/// A source range (`spec/03` §2): a file plus a UTF-8 byte interval, with the
/// `{line, col}` rendering of each endpoint for human-facing tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub file: String,
    pub start_byte: u32,
    pub end_byte: u32,
    pub start: Position,
    pub end: Position,
}

/// A text edit (`spec/03` §2): replace the text in `span` with `new_text`. A
/// `None` span is an insertion/replacement whose location is not yet known to
/// the checker (see the module docs on span scope honesty); `new_text` is always
/// meaningful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub span: Option<Span>,
    pub new_text: String,
}

impl Edit {
    /// An edit whose insertion point is not yet resolvable (spans are not in the
    /// Core IR, `spec/02` §F rule 4) but whose inserted text is known.
    pub fn insert(new_text: impl Into<String>) -> Self {
        Edit {
            span: None,
            new_text: new_text.into(),
        }
    }
}

/// A machine-actionable fix (`spec/03` §2): an ordered set of edits with a
/// human-readable title and a confidence in `[0, 1]`. Higher confidence is what
/// the agent loop (`spec/03` §5) keys its "apply automatically" threshold on.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub title: String,
    pub edits: Vec<Edit>,
    pub confidence: f32,
}

impl Fix {
    /// A single-edit fix.
    pub fn new(title: impl Into<String>, edit: Edit, confidence: f32) -> Self {
        Fix {
            title: title.into(),
            edits: vec![edit],
            confidence,
        }
    }
}

/// A related location attached to a [`Diagnostic`] (`spec/03` §2). The span is
/// optional for the same reason [`Diagnostic::span`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Related {
    pub span: Option<Span>,
    pub message: String,
}

/// A diagnostic (`spec/03` §2). Carries a stable [`Code`], a severity, an
/// optional span, a message, related locations, and fixes ordered best-first
/// (`fixes[0]` is the highest-confidence repair; the list may be empty when no
/// repair is mechanically derivable).
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    pub span: Option<Span>,
    pub message: String,
    pub related: Vec<Related>,
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    /// An error-severity diagnostic with no span, no related notes, and no fixes
    /// yet — the common starting point that builder methods refine.
    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Diagnostic {
            code,
            severity: Severity::Error,
            span: None,
            message: message.into(),
            related: Vec::new(),
            fixes: Vec::new(),
        }
    }

    /// Attach a fix, returning `self` for chaining.
    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fixes.push(fix);
        self
    }

    /// Attach a related note, returning `self` for chaining.
    pub fn with_related(mut self, message: impl Into<String>) -> Self {
        self.related.push(Related {
            span: None,
            message: message.into(),
        });
        self
    }
}

/// A stable, documented diagnostic code (`spec/03` §6: codes never change
/// meaning; messages may improve). Agents may key behavior on these.
///
/// The numbering is grouped by the check family that raises it:
///
/// | Range  | Family                                   |
/// |--------|------------------------------------------|
/// | E01xx  | types (mismatch, applying a non-function)|
/// | E011x  | capabilities (effect row, provenance)    |
/// | E012x  | error sets                               |
/// | E013x  | exhaustiveness                           |
/// | E014x  | linearity                                |
/// | E015x  | second-class references                  |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    /// A value's type does not match the type required by its context.
    TypeMismatch,
    /// Something that is not a function appears in call position.
    NotAFunction,
    /// A primitive operation was applied to operands of the wrong type.
    BadPrimOperand,
    /// An `as` cast was applied between types that have no defined conversion
    /// (`spec/01` §3.1 — only scalar↔scalar conversions are legal).
    BadCast,
    /// A function exercises a capability its declared effect row does not list.
    MissingCapability,
    /// A `Perform` names a capability that is not a capability value in scope
    /// (no ambient authority: power must be *received*, never summoned).
    UnauthorizedPerform,
    /// A capability value was produced by construction/computation rather than
    /// received or narrowed — capabilities are unforgeable (`spec/01` §5).
    ForgedCapability,
    /// A function can raise an error its declared error set does not list.
    MissingError,
    /// A `match` does not cover every variant of its scrutinee.
    NonExhaustiveMatch,
    /// A `linear` value is never consumed (it must be used exactly once).
    LinearUnused,
    /// A `linear` value is consumed more than once along some path.
    LinearDuplicated,
    /// A `linear` value is consumed on some control paths but not all.
    LinearNotAllPaths,
    /// A second-class reference escapes its call frame (stored in an aggregate,
    /// returned, or captured) — forbidden by the memory model (`spec/01` §4).
    EscapingReference,
}

impl Code {
    /// The stable wire string, e.g. `"E0101"` (`spec/03` §2, §6).
    pub fn as_str(self) -> &'static str {
        match self {
            Code::TypeMismatch => "E0101",
            Code::NotAFunction => "E0102",
            Code::BadPrimOperand => "E0103",
            Code::BadCast => "E0104",
            Code::MissingCapability => "E0110",
            Code::UnauthorizedPerform => "E0111",
            Code::ForgedCapability => "E0112",
            Code::MissingError => "E0120",
            Code::NonExhaustiveMatch => "E0130",
            Code::LinearUnused => "E0140",
            Code::LinearDuplicated => "E0141",
            Code::LinearNotAllPaths => "E0142",
            Code::EscapingReference => "E0150",
        }
    }

    /// A one-line description of the rule, for the `marv/errorCodes` catalog
    /// (`spec/03` §6) and human-facing help.
    pub fn summary(self) -> &'static str {
        match self {
            Code::TypeMismatch => "a value's type does not match the type its context requires",
            Code::NotAFunction => "a non-function value is applied as if it were a function",
            Code::BadPrimOperand => {
                "a primitive operation is applied to an operand of the wrong type"
            }
            Code::BadCast => "an `as` cast is applied between types with no defined conversion",
            Code::MissingCapability => {
                "a function exercises a capability its declared effect row does not list"
            }
            Code::UnauthorizedPerform => {
                "a `Perform` names a capability that is not a capability value in scope"
            }
            Code::ForgedCapability => {
                "a capability was produced by construction instead of being received or narrowed"
            }
            Code::MissingError => {
                "a function can raise an error its declared error set does not list"
            }
            Code::NonExhaustiveMatch => "a `match` does not cover every variant of its scrutinee",
            Code::LinearUnused => {
                "a `linear` value is never consumed (it must be used exactly once)"
            }
            Code::LinearDuplicated => "a `linear` value is consumed more than once along some path",
            Code::LinearNotAllPaths => {
                "a `linear` value is consumed on some control paths but not all"
            }
            Code::EscapingReference => "a second-class reference escapes its call frame",
        }
    }

    /// Every code, in numeric order — the body of the `marv/errorCodes` query.
    pub fn catalog() -> &'static [Code] {
        &[
            Code::TypeMismatch,
            Code::NotAFunction,
            Code::BadPrimOperand,
            Code::BadCast,
            Code::MissingCapability,
            Code::UnauthorizedPerform,
            Code::ForgedCapability,
            Code::MissingError,
            Code::NonExhaustiveMatch,
            Code::LinearUnused,
            Code::LinearDuplicated,
            Code::LinearNotAllPaths,
            Code::EscapingReference,
        ]
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
