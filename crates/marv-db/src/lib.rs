//! # marv-db — incremental query database (milestone M3)
//!
//! The salsa-backed, demand-driven incremental query engine that is the backbone
//! of the agent protocol (`spec/03` §1). The compiler pipeline
//! `parse → lower → typecheck → effects/errors` is modelled as a memoized query
//! keyed by file content: editing one file recomputes only that file's analysis,
//! so the `check`/`signature`/`core`/… queries the server exposes stay cheap to
//! call in a tight generate→check→repair loop (`spec/03` §5).
//!
//! ## Shape
//!
//! - [`SourceFile`] is the single salsa **input**: a workspace file's path, kind
//!   ([`SourceKind`] — marv source or ingested Core), and text. A "snapshot" in
//!   the protocol is just a set of these inputs (the server owns that mapping).
//! - [`analyze`] is the single salsa **tracked query**: it runs the full
//!   pipeline ([`analyze_text`]) and returns a [`FileAnalysis`]. salsa memoizes
//!   it per input and re-executes it only when that input's text changes.
//!
//! [`ANALYZE_RUNS`] counts query executions, so tests can prove the
//! incrementality property directly (edit file A ⇒ A re-runs, B does not).

pub mod analysis;
pub mod corespec;

use std::sync::atomic::{AtomicU64, Ordering};

pub use analysis::{
    analyze_text, qualify, repair_core_text, verify_inputs, DefInfo, DiagInfo, FileAnalysis,
    FixInfo, ParamInfo, SourceKind, VerifyDef,
};
pub use corespec::{
    CapSpec, CoreDefSpec, CoreModuleSpec, EnumSpec, ErrorSpec, GlobalSpec, OpSpec, StructSpec,
    VariantSpec, WorldSpec,
};

/// A single workspace file — the one salsa input. Its getters are
/// `file.path(db)`, `file.kind(db)`, `file.text(db)`; an edit is
/// `file.set_text(&mut db).to(new)` (needs `use salsa::Setter`).
#[salsa::input]
pub struct SourceFile {
    #[returns(clone)]
    pub path: String,
    pub kind: SourceKind,
    #[returns(clone)]
    pub text: String,
}

/// The incremental database. One per server process; snapshots are sets of
/// [`SourceFile`] inputs created in it.
#[salsa::db]
#[derive(Default, Clone)]
pub struct MarvDatabase {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for MarvDatabase {}

/// Total number of times [`analyze`] has actually executed its body (as opposed
/// to returning a memoized result). Tests read this to assert incrementality.
pub static ANALYZE_RUNS: AtomicU64 = AtomicU64::new(0);

/// The pipeline as a memoized salsa query: analyze one file. Re-executes only
/// when `file`'s inputs change; otherwise returns the cached [`FileAnalysis`].
#[salsa::tracked]
pub fn analyze(db: &dyn salsa::Database, file: SourceFile) -> FileAnalysis {
    ANALYZE_RUNS.fetch_add(1, Ordering::Relaxed);
    analyze_text(file.kind(db), &file.text(db))
}
