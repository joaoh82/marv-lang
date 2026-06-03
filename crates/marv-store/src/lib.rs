//! # marv-store — content-addressed code store + lockfile (milestone M7)
//!
//! The content-addressed store and lockfile that turn marv's per-definition
//! content hashing (`spec/02` §F) into the reuse properties of `spec/01` §8:
//!
//! - **Reproducibility.** A build is pinned to a set of hashes via a [`Lockfile`];
//!   the same source always commits to the same hashes.
//! - **No dependency hell.** Two libraries pinning different hashes of the "same"
//!   function are just two keys in the [`Store`]; both coexist.
//! - **Free renames.** Identity is the **dag hash** ([`resolve`]) — the content
//!   hash with references resolved to *their* dag hashes (and recursive cycles to
//!   positional placeholders), so it commits to the dependency DAG and depends on
//!   no names. Renaming any definition changes no hashes.
//! - **Dedup & provenance.** Identical definitions collapse to one entry, and the
//!   store answers "has this exact hash been reviewed before?" — [`Store::is_reviewed`].
//!
//! [`commit`] (the `marv/commit` method, `spec/03` §3.4) freezes a module's
//! definitions into the store, rebinds their names in the lockfile, and returns
//! the [`CommitReport`] delta (what's new vs. already-reviewed).

mod resolve;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use marv_core::ir::{Def, Hash};
use serde::{Deserialize, Serialize};

pub use resolve::{resolve, Resolved};

/// One stored definition, keyed in the [`Store`] by its dag hash. Identity is
/// the hash; `name` is the last label seen (informational — renaming does not
/// change the hash).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredDef {
    /// Dag-hash identity, wire form (`b3:…`).
    pub hash: String,
    /// Last-seen label for this definition (purely informational).
    pub name: String,
    /// The name-free, references-resolved Core definition (the Merkle node).
    pub def: Def,
    /// Dag hashes this definition references — its Merkle-DAG out-edges.
    pub deps: Vec<String>,
    /// Whether this exact hash has been frozen/reviewed (`spec/01` §8 provenance).
    pub reviewed: bool,
}

/// The content-addressed store: dag hash (`b3:…`) → definition. A `BTreeMap`
/// keeps serialization deterministic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    pub defs: BTreeMap<String, StoredDef>,
}

/// A lockfile: a name → dag-hash binding set pinning a build (`spec/01` §8).
/// Names are *labels over hashes*; rebinding a name (a rename or a version bump)
/// never disturbs the stored definitions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lockfile {
    /// Qualified name (`math.clamp`) → dag hash (`b3:…`).
    pub bindings: BTreeMap<String, String>,
}

/// Whether a committed definition was new to the store or already present
/// (hence already reviewable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStatus {
    /// First time this dag hash is seen — added to the store.
    New,
    /// This exact dag hash was already in the store (dedup); `reviewed` reports
    /// whether it had been frozen before.
    Existing { reviewed: bool },
}

/// One definition's outcome within a [`CommitReport`].
#[derive(Debug, Clone)]
pub struct CommitEntry {
    pub name: String,
    pub qualified: String,
    pub hash: String,
    pub status: CommitStatus,
}

/// The delta a [`commit`] produced (the `marv/commit` result, `spec/03` §3.4).
#[derive(Debug, Clone, Default)]
pub struct CommitReport {
    pub entries: Vec<CommitEntry>,
    /// Names whose lockfile binding now points to a *different* hash than before.
    pub rebound: Vec<String>,
}

impl CommitReport {
    pub fn added(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == CommitStatus::New)
            .count()
    }
    pub fn deduped(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.status, CommitStatus::Existing { .. }))
            .count()
    }
}

impl Store {
    pub fn new() -> Self {
        Store::default()
    }

    /// Whether a dag hash (wire form) is present.
    pub fn contains(&self, hash: &str) -> bool {
        self.defs.contains_key(hash)
    }

    /// Whether a dag hash is present *and* has been reviewed (`spec/01` §8).
    pub fn is_reviewed(&self, hash: &str) -> bool {
        self.defs.get(hash).map(|d| d.reviewed).unwrap_or(false)
    }

    pub fn get(&self, hash: &str) -> Option<&StoredDef> {
        self.defs.get(hash)
    }

    /// Number of distinct definitions stored.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

impl Lockfile {
    pub fn new() -> Self {
        Lockfile::default()
    }

    /// The dag hash a name is currently pinned to.
    pub fn get(&self, qualified: &str) -> Option<&String> {
        self.bindings.get(qualified)
    }

    /// `symbol_hash(qualified) → dag_hash` for every binding — the table
    /// [`resolve`] uses to link a new module against already-committed ones.
    fn external_index(&self) -> HashMap<Hash, Hash> {
        let mut idx = HashMap::new();
        for (name, hash) in &self.bindings {
            if let Some(h) = Hash::from_b3(hash) {
                idx.insert(marv_core::symbol_hash(name), h);
            }
        }
        idx
    }
}

/// Module-qualify a name (mirrors `marv_db::qualify`).
fn qualify(module_path: &str, name: &str) -> String {
    if module_path.is_empty() {
        name.to_string()
    } else {
        format!("{module_path}.{name}")
    }
}

/// Freeze a module's definitions into `store` and rebind their names in `lock`
/// (the `marv/commit` operation, `spec/03` §3.4).
///
/// Definitions are identified by dag hash, so committing the same source twice
/// is idempotent (every hash is `Existing` the second time), structurally
/// identical definitions dedup, and a rename rebinds a name without changing any
/// hash. Returns the delta; freshly added definitions are marked **reviewed**
/// (committing *is* the freeze/review step).
pub fn commit(
    store: &mut Store,
    lock: &mut Lockfile,
    module_path: &str,
    defs: &[(String, Def)],
) -> CommitReport {
    let external = lock.external_index();
    let resolved = resolve(module_path, defs, &external);

    let mut report = CommitReport::default();
    for (i, (name, _)) in defs.iter().enumerate() {
        let qualified = qualify(module_path, name);
        let hash = resolved.dag_hashes[i].to_b3();

        let status = if let Some(existing) = store.defs.get(&hash) {
            CommitStatus::Existing {
                reviewed: existing.reviewed,
            }
        } else {
            let deps = resolved.deps[i].iter().map(|h| h.to_b3()).collect();
            store.defs.insert(
                hash.clone(),
                StoredDef {
                    hash: hash.clone(),
                    name: name.clone(),
                    def: resolved.resolved_defs[i].clone(),
                    deps,
                    reviewed: true,
                },
            );
            CommitStatus::New
        };

        // Rebind the name; record a rebinding when it moved to a new hash.
        if let Some(prev) = lock.bindings.get(&qualified) {
            if prev != &hash {
                report.rebound.push(qualified.clone());
            }
        }
        lock.bindings.insert(qualified.clone(), hash.clone());

        report.entries.push(CommitEntry {
            name: name.clone(),
            qualified,
            hash,
            status,
        });
    }
    report
}

// ---- persistence --------------------------------------------------------

/// The on-disk store/lockfile pair under a directory (default `.marv/`).
pub struct StoreDir {
    pub root: std::path::PathBuf,
}

impl StoreDir {
    pub fn new(root: impl AsRef<Path>) -> Self {
        StoreDir {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn store_path(&self) -> std::path::PathBuf {
        self.root.join("store.json")
    }
    fn lock_path(&self) -> std::path::PathBuf {
        self.root.join("lockfile.json")
    }

    /// Load the store and lockfile, or empty ones if they do not exist yet.
    pub fn load(&self) -> std::io::Result<(Store, Lockfile)> {
        let store = match std::fs::read_to_string(self.store_path()) {
            Ok(s) => serde_json::from_str(&s).map_err(to_io)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store::new(),
            Err(e) => return Err(e),
        };
        let lock = match std::fs::read_to_string(self.lock_path()) {
            Ok(s) => serde_json::from_str(&s).map_err(to_io)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Lockfile::new(),
            Err(e) => return Err(e),
        };
        Ok((store, lock))
    }

    /// Write the store and lockfile (creating the directory if needed),
    /// pretty-printed and deterministically ordered.
    pub fn save(&self, store: &Store, lock: &Lockfile) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(
            self.store_path(),
            serde_json::to_string_pretty(store).map_err(to_io)?,
        )?;
        std::fs::write(
            self.lock_path(),
            serde_json::to_string_pretty(lock).map_err(to_io)?,
        )?;
        Ok(())
    }
}

fn to_io(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e)
}
