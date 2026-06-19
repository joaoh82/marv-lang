//! Runtime values and the side-channel records the interpreter produces.
//!
//! The Core IR is names-erased and curried (`spec/02` §C), so the value domain
//! is small: scalars, aggregates (structs *and* enum variants share one shape —
//! a tag plus fields), capability tokens, and partial applications (the runtime
//! mirror of currying — a top-level function with some, but not yet all, of its
//! arguments supplied).

use marv_core::ir::Hash;

/// A runtime value.
///
/// Every integer width collapses to `i64` here: the interpreter is the
/// *semantics oracle* the Cranelift backend is differentially tested against
/// ([`crate`] docs), and that backend computes in 64-bit registers, so matching
/// its 64-bit wrapping is what keeps the two in agreement. Narrower-width
/// semantics are a later refinement (they belong in both backends at once).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i64),
    /// IEEE-754 double; stored as bits-comparable `f64`.
    Float(f64),
    Str(String),
    /// A struct or enum value: `tag` selects the variant (products use tag 0,
    /// `spec/02` §C `Ctor`), `fields` are its components in declaration order.
    Agg {
        tag: u32,
        fields: Vec<Value>,
    },
    /// A growable list value. The public type is `std.collections.List[T]`;
    /// the interpreter keeps the capacity explicit so `with_capacity`/`push`
    /// mirror the compiled backends' `[len, cap, e0, …]` layout.
    List {
        items: Vec<Value>,
        cap: usize,
    },
    /// An unforgeable capability token, named for effect reporting. Injected at
    /// the entry point by [`crate::Program::run`] from the host's grant set; it
    /// can only be received or passed on, never constructed (`spec/01` §5).
    Cap(String),
    /// A partially-applied top-level function: `func` is the callee's symbol
    /// hash, `got` the arguments supplied so far. Saturating it (one arg per
    /// curried `Lam`) triggers the call (`spec/02` §C — application is curried).
    Partial {
        func: Hash,
        got: Vec<Value>,
    },
}

impl Value {
    /// The boolean a value denotes in a condition, or `None` if it is not a
    /// boolean (a type error the M2 checker rules out before execution).
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Render a value the way `marv run` prints an entry point's result — the
    /// same textual form the differential harness compares across backends.
    pub fn render(&self) -> String {
        match self {
            Value::Unit => "()".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(x) => format!("{x}"),
            Value::Str(s) => s.clone(),
            Value::Agg { tag, fields } => {
                let inner: Vec<String> = fields.iter().map(Value::render).collect();
                format!("#{tag}({})", inner.join(", "))
            }
            Value::List { items, .. } => {
                let inner: Vec<String> = items.iter().map(Value::render).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Cap(name) => format!("<cap {name}>"),
            Value::Partial { .. } => "<partial>".to_string(),
        }
    }
}

/// One observed capability effect, in the shape `marv/run` reports
/// (`spec/03` §4.5: `{"cap":"Fs","op":"read","arg":"./data.csv"}`). The
/// interpreter records these as a body `perform`s them, so a run's full
/// authority use is auditable after the fact.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub cap: String,
    pub op: u32,
    pub args: Vec<Value>,
}
