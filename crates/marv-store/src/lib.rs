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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use marv_core::ir::{Def, Hash, Type};
use serde::{Deserialize, Serialize};

pub use resolve::{resolve, Resolved};

/// Declaration metadata that lives beside the name-erased Core definition.
/// Core carries the structural type, but enum variant names/field lists and
/// capability operation signatures are needed to rebuild a checker/codegen
/// world after fetching definitions by hash.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DefMeta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_variants: Vec<StoredVariant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_ops: Vec<StoredOpSig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredVariant {
    pub name: String,
    #[serde(default)]
    pub fields: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredOpSig {
    #[serde(default)]
    pub params: Vec<Type>,
    #[serde(default = "unit_type")]
    pub ret: Type,
    #[serde(default)]
    pub errors: Vec<Hash>,
}

fn unit_type() -> Type {
    Type::Unit
}

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
    /// Non-hashed declaration metadata needed when this blob is fetched without
    /// the original source module.
    #[serde(default)]
    pub meta: DefMeta,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    MissingLockBinding(String),
    InvalidHash(String),
    MissingDef(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::MissingLockBinding(n) => write!(f, "lockfile has no binding for `{n}`"),
            StoreError::InvalidHash(h) => write!(f, "`{h}` is not a valid b3 hash"),
            StoreError::MissingDef(h) => write!(f, "store is missing blob `{h}`"),
        }
    }
}

impl std::error::Error for StoreError {}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Closure {
    /// Fetched definitions in deterministic DFS order, roots first.
    pub defs: Vec<StoredDef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub removed: Vec<String>,
    pub retained: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditReport {
    pub entries: Vec<AuditEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub hash: String,
    pub name: String,
    pub reviewed: bool,
    pub reachable: bool,
    pub deps: Vec<String>,
    /// Placeholder for `marv/unsafeSites`; unsafe surface syntax is still spec
    /// only, so current Core blobs have no unsafe-site metadata to report.
    pub unsafe_sites: Vec<String>,
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
    pub fn external_index(&self) -> HashMap<Hash, Hash> {
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
    if module_path.is_empty() || name.contains('.') {
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
    let entries: Vec<(String, Def, DefMeta)> = defs
        .iter()
        .map(|(name, def)| (name.clone(), def.clone(), DefMeta::default()))
        .collect();
    commit_with_meta(store, lock, module_path, &entries)
}

/// [`commit`] with declaration metadata supplied by the front end. The metadata
/// is not part of the content identity; it lets fetched blobs reconstruct the
/// declaration world needed by checkers/backends.
pub fn commit_with_meta(
    store: &mut Store,
    lock: &mut Lockfile,
    module_path: &str,
    defs: &[(String, Def, DefMeta)],
) -> CommitReport {
    let external = lock.external_index();
    let core_defs: Vec<(String, Def)> = defs
        .iter()
        .map(|(name, def, _)| (name.clone(), def.clone()))
        .collect();
    let resolved = resolve(module_path, &core_defs, &external);

    let mut report = CommitReport::default();
    for (i, (name, _, meta)) in defs.iter().enumerate() {
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
                    meta: meta.clone(),
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

impl Store {
    /// Fetch one blob by hash.
    pub fn fetch(&self, hash: &str) -> Result<&StoredDef, StoreError> {
        if Hash::from_b3(hash).is_none() {
            return Err(StoreError::InvalidHash(hash.to_string()));
        }
        self.defs
            .get(hash)
            .ok_or_else(|| StoreError::MissingDef(hash.to_string()))
    }

    /// Resolve the full transitive closure rooted at lockfile names.
    pub fn closure_for_names(
        &self,
        lock: &Lockfile,
        roots: &[String],
    ) -> Result<Closure, StoreError> {
        let mut hashes = Vec::new();
        for root in roots {
            let hash = lock
                .bindings
                .get(root)
                .ok_or_else(|| StoreError::MissingLockBinding(root.clone()))?;
            hashes.push(hash.clone());
        }
        self.closure_for_hashes(&hashes)
    }

    /// Resolve the full transitive closure rooted at concrete dag hashes.
    pub fn closure_for_hashes(&self, roots: &[String]) -> Result<Closure, StoreError> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for root in roots {
            self.visit_closure(root, &mut seen, &mut out)?;
        }
        Ok(Closure { defs: out })
    }

    fn visit_closure(
        &self,
        hash: &str,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<StoredDef>,
    ) -> Result<(), StoreError> {
        if !seen.insert(hash.to_string()) {
            return Ok(());
        }
        let def = self.fetch(hash)?.clone();
        out.push(def.clone());
        for dep in &def.deps {
            self.visit_closure(dep, seen, out)?;
        }
        Ok(())
    }

    /// Remove blobs unreachable from every lockfile binding.
    pub fn gc(&mut self, lock: &Lockfile) -> GcReport {
        let roots: Vec<String> = lock.bindings.values().cloned().collect();
        let reachable = match self.closure_for_hashes(&roots) {
            Ok(c) => c.defs.into_iter().map(|d| d.hash).collect::<BTreeSet<_>>(),
            Err(_) => {
                return GcReport {
                    removed: Vec::new(),
                    retained: self.defs.len(),
                }
            }
        };
        let before: Vec<String> = self.defs.keys().cloned().collect();
        let mut removed = Vec::new();
        for hash in before {
            if !reachable.contains(&hash) {
                self.defs.remove(&hash);
                removed.push(hash);
            }
        }
        GcReport {
            removed,
            retained: self.defs.len(),
        }
    }

    /// Provenance/audit view over the store, marking blobs reachable from the
    /// current lockfile and listing their Merkle-DAG edges.
    pub fn audit(&self, lock: &Lockfile) -> AuditReport {
        let roots: Vec<String> = lock.bindings.values().cloned().collect();
        let reachable = self
            .closure_for_hashes(&roots)
            .map(|c| c.defs.into_iter().map(|d| d.hash).collect::<BTreeSet<_>>())
            .unwrap_or_default();
        let entries = self
            .defs
            .values()
            .map(|d| AuditEntry {
                hash: d.hash.clone(),
                name: d.name.clone(),
                reviewed: d.reviewed,
                reachable: reachable.contains(&d.hash),
                deps: d.deps.clone(),
                unsafe_sites: Vec::new(),
            })
            .collect();
        AuditReport { entries }
    }
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
    fn blobs_dir(&self) -> std::path::PathBuf {
        self.root.join("blobs").join("b3")
    }
    fn blob_path(&self, hash: &str) -> std::path::PathBuf {
        let hex = hash.strip_prefix("b3:").unwrap_or(hash);
        let (prefix, rest) = hex.split_at(hex.len().min(2));
        self.blobs_dir().join(prefix).join(format!("{rest}.json"))
    }
    fn lock_path(&self) -> std::path::PathBuf {
        self.root.join("lockfile.json")
    }

    /// Load the store and lockfile, or empty ones if they do not exist yet.
    pub fn load(&self) -> std::io::Result<(Store, Lockfile)> {
        let mut store = self.load_blobs()?;
        // Backward-compatible migration path from the original single JSON file.
        if store.is_empty() {
            store = match std::fs::read_to_string(self.store_path()) {
                Ok(s) => serde_json::from_str(&s).map_err(to_io)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store::new(),
                Err(e) => return Err(e),
            };
        }
        let lock = match std::fs::read_to_string(self.lock_path()) {
            Ok(s) => serde_json::from_str(&s).map_err(to_io)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Lockfile::new(),
            Err(e) => return Err(e),
        };
        Ok((store, lock))
    }

    /// Write the content-addressed blob store and lockfile (creating the
    /// directory if needed), pretty-printed and deterministically ordered.
    pub fn save(&self, store: &Store, lock: &Lockfile) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.blobs_dir())?;
        self.prune_blob_files(store)?;
        for (hash, def) in &store.defs {
            let path = self.blob_path(hash);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, serde_json::to_string_pretty(def).map_err(to_io)?)?;
        }
        std::fs::write(
            self.lock_path(),
            serde_json::to_string_pretty(lock).map_err(to_io)?,
        )?;
        Ok(())
    }

    fn load_blobs(&self) -> std::io::Result<Store> {
        let mut store = Store::new();
        let root = self.blobs_dir();
        let Ok(prefixes) = std::fs::read_dir(&root) else {
            return Ok(store);
        };
        for prefix in prefixes {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(prefix.path())? {
                let entry = entry?;
                if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let s = std::fs::read_to_string(entry.path())?;
                let def: StoredDef = serde_json::from_str(&s).map_err(to_io)?;
                store.defs.insert(def.hash.clone(), def);
            }
        }
        Ok(store)
    }

    fn prune_blob_files(&self, store: &Store) -> std::io::Result<()> {
        let root = self.blobs_dir();
        let Ok(prefixes) = std::fs::read_dir(&root) else {
            return Ok(());
        };
        for prefix in prefixes {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(prefix.path())? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Some(dir) = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                else {
                    continue;
                };
                let hash = format!("b3:{dir}{stem}");
                if !store.defs.contains_key(&hash) {
                    std::fs::remove_file(path)?;
                }
            }
        }
        Ok(())
    }
}

fn to_io(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e)
}
