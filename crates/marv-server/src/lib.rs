//! # marv-server — JSON-RPC agent-protocol server (milestone M3)
//!
//! Wraps the [`marv_db`] incremental query engine in the agent-facing JSON-RPC
//! 2.0 protocol of `spec/03`. The transport is line-delimited JSON over any
//! reader/writer pair (stdio by default — see the `marv-server` binary); each
//! request names a `marv/*` method, and the response shapes match `spec/03` §4
//! exactly.
//!
//! ## Snapshots
//!
//! The agent owns an in-memory workspace as a **snapshot** — a set of
//! [`marv_db::SourceFile`] salsa inputs. `openSnapshot` creates one;
//! `applyEdits`/`applyFix`/`format` produce a *new* snapshot id, reusing the
//! input handles of unchanged files so salsa's per-file memoization carries
//! across snapshots (an edit to one file never recomputes the others —
//! `spec/03` §1). `closeSnapshot` discards one.
//!
//! ## Method catalog (`spec/03` §3)
//!
//! Workspace: `openSnapshot`, `applyEdits`, `closeSnapshot`. Read-only queries:
//! `check`, `typeAt`, `signature`, `errorSet`, `effects`, `callers`, `callees`,
//! `canonical`, `core`, `hash`. Mutation: `applyFix`, `format`. Verification:
//! `verify` (Tier-2 SMT discharge, M6). Reuse: `commit` (freeze into the
//! content-addressed store, M7). Build/run — `build`/`run` — belong to the CLI
//! today and report method-not-found here.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use marv_core::lower_modules;
use marv_core::symbol_hash;
use marv_db::{
    analyze, qualify, repair_core_text, DefInfo, DiagInfo, FileAnalysis, FixInfo, MarvDatabase,
    SourceFile, SourceKind, SrcSpan,
};
use marv_syntax::parse;
use marv_types::{check_def, World};
use serde_json::{json, Value};

/// A JSON-RPC error (`spec/03` is silent on codes, so we use the standard
/// JSON-RPC reservations plus an application range for protocol-specific faults).
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
        }
    }
    fn method_not_found(m: &str) -> Self {
        RpcError::new(-32601, format!("method not found: {m}"))
    }
    fn invalid_params(m: impl Into<String>) -> Self {
        RpcError::new(-32602, m)
    }
    fn app(m: impl Into<String>) -> Self {
        RpcError::new(-32000, m)
    }
}

type RpcResult = Result<Value, RpcError>;

/// One file within a snapshot: its identity plus the salsa input handle whose
/// `analyze` query holds the file's compiled view.
#[derive(Clone)]
struct SnapFile {
    path: String,
    kind: SourceKind,
    text: String,
    input: SourceFile,
}

/// An immutable workspace version — the unit `openSnapshot` returns and the
/// queries run against.
#[derive(Clone)]
struct Snapshot {
    files: Vec<SnapFile>,
}

/// The protocol server: one incremental database, the live snapshots, and the
/// content-addressed store/lockfile that `commit` freezes into.
pub struct Server {
    db: MarvDatabase,
    snapshots: BTreeMap<String, Snapshot>,
    next_id: u64,
    store: marv_store::Store,
    lock: marv_store::Lockfile,
}

impl Default for Server {
    fn default() -> Self {
        Server::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Server {
            db: MarvDatabase::default(),
            snapshots: BTreeMap::new(),
            next_id: 1,
            store: marv_store::Store::new(),
            lock: marv_store::Lockfile::new(),
        }
    }

    // ---- snapshot plumbing ---------------------------------------------

    fn fresh_id(&mut self) -> String {
        let id = format!("s{}", self.next_id);
        self.next_id += 1;
        id
    }

    /// Create a salsa input for one file from its protocol description
    /// (`{path, text}` for source, `{path, core: {...}}` for ingested Core).
    fn make_file(&mut self, spec: &Value) -> Result<SnapFile, RpcError> {
        let path = spec
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("file is missing `path`"))?
            .to_string();
        let (kind, text) = if let Some(core) = spec.get("core") {
            // Ingested Core: store the canonical JSON of the supplied module.
            (SourceKind::Core, core.to_string())
        } else if let Some(t) = spec.get("text").and_then(Value::as_str) {
            (SourceKind::Source, t.to_string())
        } else {
            return Err(RpcError::invalid_params(
                "file needs either `text` (source) or `core` (Core IR)",
            ));
        };
        let input = SourceFile::new(&self.db, path.clone(), kind, text.clone());
        Ok(SnapFile {
            path,
            kind,
            text,
            input,
        })
    }

    fn register(&mut self, files: Vec<SnapFile>) -> String {
        let id = self.fresh_id();
        self.snapshots.insert(id.clone(), Snapshot { files });
        id
    }

    fn snapshot<'a>(&'a self, params: &Value) -> Result<&'a Snapshot, RpcError> {
        let id = params
            .get("snapshotId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("missing `snapshotId`"))?;
        self.snapshots
            .get(id)
            .ok_or_else(|| RpcError::app(format!("unknown snapshot `{id}`")))
    }

    fn analyze_file(&self, f: &SnapFile) -> FileAnalysis {
        analyze(&self.db, f.input)
    }

    /// All `(file_path, def)` pairs across a snapshot.
    fn all_defs(&self, snap: &Snapshot) -> Vec<(String, DefInfo)> {
        let mut out = Vec::new();
        for f in &snap.files {
            for d in self.analyze_file(f).defs {
                out.push((f.path.clone(), d));
            }
        }
        out
    }

    /// Locate a definition by qualified (`report.load`) or bare (`load`) name,
    /// returning its info and the path of the file that declares it.
    fn find_def(&self, snap: &Snapshot, name: &str) -> Result<(String, DefInfo), RpcError> {
        for f in &snap.files {
            for d in self.analyze_file(f).defs {
                if d.qualified == name || d.name == name {
                    return Ok((f.path.clone(), d));
                }
            }
        }
        Err(RpcError::app(format!("unknown definition `{name}`")))
    }

    fn def_param(&self, params: &Value) -> Result<String, RpcError> {
        params
            .get("def")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| RpcError::invalid_params("missing `def`"))
    }

    // ---- dispatch -------------------------------------------------------

    /// Handle one parsed JSON-RPC request object, returning the full response
    /// envelope (`result` or `error`). Notifications (no `id`) still get a value
    /// back; the caller may drop it.
    pub fn handle_request(&mut self, req: Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        match self.dispatch(method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": e.code, "message": e.message}
            }),
        }
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> RpcResult {
        let m = method.strip_prefix("marv/").unwrap_or(method);
        match m {
            "openSnapshot" => self.open_snapshot(params),
            "applyEdits" => self.apply_edits(params),
            "closeSnapshot" => self.close_snapshot(params),
            "check" => self.check(params),
            "typeAt" => self.type_at(params),
            "signature" => self.signature(params),
            "errorSet" => self.error_set(params),
            "effects" => self.effects(params),
            "unsafeSites" => self.unsafe_sites(params),
            "callers" => self.call_edges(params, Direction::Callers),
            "callees" => self.call_edges(params, Direction::Callees),
            "canonical" => self.canonical(params),
            "core" => self.core(params),
            "hash" => self.hash(params),
            "verify" => self.verify(params),
            "commit" => self.commit(params),
            "applyFix" => self.apply_fix(params),
            "format" => self.format(params),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    // ---- workspace methods ---------------------------------------------

    fn open_snapshot(&mut self, params: &Value) -> RpcResult {
        let files = params
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcError::invalid_params("`files` array required"))?;
        let mut snap_files = Vec::with_capacity(files.len());
        for spec in files {
            snap_files.push(self.make_file(spec)?);
        }
        let id = self.register(snap_files);
        Ok(json!({ "snapshotId": id }))
    }

    fn apply_edits(&mut self, params: &Value) -> RpcResult {
        let base = self.snapshot(params)?.clone();
        // path -> (kind, text) starting from the base snapshot.
        let mut texts: BTreeMap<String, (SourceKind, String)> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for f in &base.files {
            texts.insert(f.path.clone(), (f.kind, f.text.clone()));
            order.push(f.path.clone());
        }

        // Whole-file replacements.
        if let Some(fs) = params.get("files").and_then(Value::as_array) {
            for spec in fs {
                let path = spec
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::invalid_params("file replacement missing `path`"))?;
                let (kind, text) = if let Some(core) = spec.get("core") {
                    (SourceKind::Core, core.to_string())
                } else if let Some(t) = spec.get("text").and_then(Value::as_str) {
                    (SourceKind::Source, t.to_string())
                } else {
                    return Err(RpcError::invalid_params(
                        "replacement needs `text` or `core`",
                    ));
                };
                if !texts.contains_key(path) {
                    order.push(path.to_string());
                }
                texts.insert(path.to_string(), (kind, text));
            }
        }

        // Byte-range edits, grouped per file and applied right-to-left.
        if let Some(edits) = params.get("edits").and_then(Value::as_array) {
            let mut per_file: BTreeMap<String, Vec<(usize, usize, String)>> = BTreeMap::new();
            for e in edits {
                let file = e
                    .get("file")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::invalid_params("edit missing `file`"))?;
                let span = e.get("span").unwrap_or(e);
                let start = span
                    .get("startByte")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| RpcError::invalid_params("edit missing `startByte`"))?
                    as usize;
                let end = span
                    .get("endByte")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| RpcError::invalid_params("edit missing `endByte`"))?
                    as usize;
                let new_text = e
                    .get("newText")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                per_file
                    .entry(file.to_string())
                    .or_default()
                    .push((start, end, new_text));
            }
            for (path, mut spans) in per_file {
                let (kind, text) = texts
                    .get(&path)
                    .cloned()
                    .ok_or_else(|| RpcError::app(format!("edit targets unknown file `{path}`")))?;
                spans.sort_by(|a, b| b.0.cmp(&a.0));
                let mut buf = text;
                for (s, e, t) in spans {
                    if s > e || e > buf.len() {
                        return Err(RpcError::invalid_params("edit span out of range"));
                    }
                    buf.replace_range(s..e, &t);
                }
                texts.insert(path, (kind, buf));
            }
        }

        // Rebuild the file list, creating new inputs only for changed files.
        let mut new_files = Vec::with_capacity(order.len());
        for path in order {
            let (kind, text) = texts.get(&path).cloned().unwrap();
            let unchanged = base
                .files
                .iter()
                .find(|f| f.path == path && f.kind == kind && f.text == text);
            match unchanged {
                Some(f) => new_files.push(f.clone()),
                None => {
                    let input = SourceFile::new(&self.db, path.clone(), kind, text.clone());
                    new_files.push(SnapFile {
                        path,
                        kind,
                        text,
                        input,
                    });
                }
            }
        }
        let id = self.register(new_files);
        Ok(json!({ "snapshotId": id }))
    }

    fn close_snapshot(&mut self, params: &Value) -> RpcResult {
        let id = params
            .get("snapshotId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("missing `snapshotId`"))?;
        let existed = self.snapshots.remove(id).is_some();
        Ok(json!({ "closed": existed }))
    }

    // ---- read-only queries ---------------------------------------------

    fn check(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let scope = params.get("scope");
        let want_def = scope.and_then(|s| s.get("def")).and_then(Value::as_str);
        let want_file = scope.and_then(|s| s.get("file")).and_then(Value::as_str);

        if snap.files.len() > 1 && snap.files.iter().all(|f| f.kind == SourceKind::Source) {
            let diags = self.check_source_snapshot(&snap, want_def, want_file)?;
            return Ok(json!({ "diagnostics": diags }));
        }

        let mut diags = Vec::new();
        for f in &snap.files {
            if let Some(wf) = want_file {
                if f.path != wf {
                    continue;
                }
            }
            for d in self.analyze_file(f).diagnostics {
                if let Some(wd) = want_def {
                    if d.def.as_deref() != Some(wd) {
                        continue;
                    }
                }
                diags.push(diag_to_json(f, &d));
            }
        }
        Ok(json!({ "diagnostics": diags }))
    }

    fn check_source_snapshot(
        &mut self,
        snap: &Snapshot,
        want_def: Option<&str>,
        want_file: Option<&str>,
    ) -> Result<Vec<Value>, RpcError> {
        let mut modules = Vec::with_capacity(snap.files.len());
        for f in &snap.files {
            let module = parse(&f.text)
                .map_err(|e| RpcError::app(format!("parse error in {}: {e}", f.path)))?;
            modules.push(module);
        }
        let lowered =
            lower_modules(&modules).map_err(|e| RpcError::app(format!("lower error: {e}")))?;
        let world = World::from_modules(&lowered);

        let mut diags = Vec::new();
        for ((f, module), lowered_module) in
            snap.files.iter().zip(modules.iter()).zip(lowered.iter())
        {
            if let Some(wf) = want_file {
                if f.path != wf {
                    continue;
                }
            }
            let module_path = module.name.join(".");
            for entry in &lowered_module.defs {
                let qualified = qualify(&module_path, &entry.name);
                if let Some(wd) = want_def {
                    if wd != entry.name && wd != qualified {
                        continue;
                    }
                }
                for d in check_def(&world, &entry.def, Some(&entry.name)) {
                    let info = DiagInfo {
                        code: d.code.as_str().to_string(),
                        severity: d.severity.as_str().to_string(),
                        message: d.message,
                        def: Some(qualified.clone()),
                        span: None,
                        related: d.related.into_iter().map(|r| r.message).collect(),
                        fixes: d
                            .fixes
                            .into_iter()
                            .map(|fx| FixInfo {
                                title: fx.title,
                                edits: fx
                                    .edits
                                    .into_iter()
                                    .map(|e| marv_db::EditInfo {
                                        span: None,
                                        new_text: e.new_text,
                                    })
                                    .collect(),
                                confidence: fx.confidence,
                            })
                            .collect(),
                    };
                    diags.push(diag_to_json(f, &info));
                }
            }
        }
        Ok(diags)
    }

    fn signature(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let name = self.def_param(params)?;
        let (_file, d) = self.find_def(&snap, &name)?;
        Ok(json!({
            "name": d.qualified,
            "params": d.params.iter()
                .map(|p| json!({"name": p.name, "type": p.ty}))
                .collect::<Vec<_>>(),
            "ret": d.ret,
            "effects": d.effects,
            "errorSet": d.error_set,
            "pure": d.pure,
            "requires": Vec::<Value>::new(),
            "ensures": Vec::<Value>::new(),
            "hash": d.hash,
        }))
    }

    fn effects(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let name = self.def_param(params)?;
        let (_f, d) = self.find_def(&snap, &name)?;
        Ok(json!({ "effects": d.effects }))
    }

    fn error_set(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let name = self.def_param(params)?;
        let (_f, d) = self.find_def(&snap, &name)?;
        Ok(json!({ "errorSet": d.error_set }))
    }

    fn unsafe_sites(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let mut sites = Vec::new();
        for f in &snap.files {
            let analysis = self.analyze_file(f);
            for d in &analysis.defs {
                if let Some(justification) = &d.unsafe_site {
                    sites.push(json!({
                        "file": f.path,
                        "def": d.qualified,
                        "hash": d.hash,
                        "justification": justification,
                        "span": span_json(&f.path, d.span.as_ref()),
                    }));
                }
            }
        }
        Ok(json!({ "sites": sites }))
    }

    fn core(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let name = self.def_param(params)?;
        let (_f, d) = self.find_def(&snap, &name)?;
        let core: Value = serde_json::from_str(&d.core_json).unwrap_or(Value::Null);
        Ok(json!({
            "hash": d.hash,
            "core": core,
            "deps": d.callee_hashes,
            "alphaCanonical": true,
        }))
    }

    fn hash(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let name = self.def_param(params)?;
        let (_f, d) = self.find_def(&snap, &name)?;
        Ok(json!({ "hash": d.hash }))
    }

    /// `marv/verify` — discharge a definition's contracts via SMT (Tier 2),
    /// returning `proved` / `failed` (with a counterexample) / `unsupported`
    /// (`spec/03` §3.3, §4.3). Re-derives the full Core `Def` (the distilled
    /// `DefInfo` does not carry contracts) and hands it to `marv-verify`.
    fn verify(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let name = self.def_param(params)?;
        for f in &snap.files {
            let Ok((_module_path, defs, world)) = marv_db::verify_inputs(f.kind, &f.text) else {
                continue;
            };
            if let Some(vd) = defs
                .into_iter()
                .find(|d| d.qualified == name || d.name == name)
            {
                let outcome = marv_verify::verify_def(&vd.def, &vd.params, &world);
                let mut result = verify_result_json(&outcome);
                // Point the agent at the verified definition's source (MARV-12).
                if let (Some(s), Some(obj)) = (vd.span.as_ref(), result.as_object_mut()) {
                    obj.insert("relatedSpan".into(), span_json(&f.path, Some(s)));
                }
                return Ok(result);
            }
        }
        Err(RpcError::app(format!("unknown definition `{name}`")))
    }

    /// `marv/commit` — freeze a snapshot's definitions into the content-addressed
    /// store and return the lockfile delta (`spec/03` §3.4). Identity is the dag
    /// hash, so re-committing is idempotent, renames are free, and the response
    /// reports which hashes are new vs. already reviewed.
    fn commit(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let mut entries: Vec<Value> = Vec::new();
        let mut rebound: Vec<String> = Vec::new();
        let (mut added, mut deduped) = (0u64, 0u64);

        for f in &snap.files {
            let Ok((module_path, defs, _world)) = marv_db::verify_inputs(f.kind, &f.text) else {
                continue;
            };
            let pairs: Vec<(String, marv_core::ir::Def)> =
                defs.into_iter().map(|d| (d.name, d.def)).collect();
            let report = marv_store::commit(&mut self.store, &mut self.lock, &module_path, &pairs);
            for e in &report.entries {
                let (status, reviewed) = match e.status {
                    marv_store::CommitStatus::New => ("new", true),
                    marv_store::CommitStatus::Existing { reviewed } => ("existing", reviewed),
                };
                if status == "new" {
                    added += 1;
                } else {
                    deduped += 1;
                }
                entries.push(json!({
                    "name": e.qualified,
                    "hash": e.hash,
                    "status": status,
                    "reviewed": reviewed,
                }));
            }
            rebound.extend(report.rebound.iter().cloned());
        }

        Ok(json!({
            "committed": entries,
            "added": added,
            "alreadyReviewed": deduped,
            "rebound": rebound,
            "storeSize": self.store.len(),
        }))
    }

    fn type_at(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let file = params
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("missing `file`"))?;
        let byte = params
            .get("byte")
            .or_else(|| params.get("offset"))
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::invalid_params("missing `byte` offset"))?
            as u32;
        let f = snap
            .files
            .iter()
            .find(|f| f.path == file)
            .ok_or_else(|| RpcError::app(format!("unknown file `{file}`")))?;
        let analysis = self.analyze_file(f);
        // The enclosing definition is the one whose header starts at the greatest
        // byte not exceeding the offset. Spans are real but def-granular (the Core
        // IR is span-free by design), so `typeAt` resolves to the whole def.
        let enclosing = analysis
            .defs
            .iter()
            .filter(|d| d.span.map(|s| s.start_byte <= byte).unwrap_or(false))
            .max_by_key(|d| d.span.unwrap().start_byte);
        match enclosing {
            Some(d) => {
                let params_ty: Vec<&str> = d.params.iter().map(|p| p.ty.as_str()).collect();
                Ok(json!({
                    "def": d.qualified,
                    "type": format!("fn({}) -> {}", params_ty.join(", "), d.ret),
                    "effects": d.effects,
                    "span": span_json(&f.path, d.span.as_ref()),
                }))
            }
            None => Err(RpcError::app("no definition encloses that offset")),
        }
    }

    fn call_edges(&mut self, params: &Value, dir: Direction) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        let name = self.def_param(params)?;
        let (_f, target) = self.find_def(&snap, &name)?;
        let all = self.all_defs(&snap);

        // Symbol-hash → qualified name, for resolving `Global` references back to
        // names. `Atom::Global` keys on the *symbol* hash of the callee's name,
        // not its content hash, so we index by that.
        let mut by_symbol: BTreeMap<String, String> = BTreeMap::new();
        for (_p, d) in &all {
            by_symbol.insert(symbol_hash(&d.qualified).to_b3(), d.qualified.clone());
        }

        let edges: Vec<String> = match dir {
            Direction::Callees => target
                .callee_hashes
                .iter()
                .map(|h| by_symbol.get(h).cloned().unwrap_or_else(|| h.clone()))
                .collect(),
            Direction::Callers => {
                let target_sym = symbol_hash(&target.qualified).to_b3();
                all.iter()
                    .filter(|(_p, d)| d.callee_hashes.contains(&target_sym))
                    .map(|(_p, d)| d.qualified.clone())
                    .collect()
            }
        };
        let key = match dir {
            Direction::Callees => "callees",
            Direction::Callers => "callers",
        };
        Ok(json!({ key: edges }))
    }

    fn canonical(&mut self, params: &Value) -> RpcResult {
        let snap = self.snapshot(params)?.clone();
        if let Some(def) = params.get("def").and_then(Value::as_str) {
            let (_f, d) = self.find_def(&snap, def)?;
            return Ok(json!({ "text": d.canonical }));
        }
        if let Some(file) = params.get("file").and_then(Value::as_str) {
            let f = snap
                .files
                .iter()
                .find(|f| f.path == file)
                .ok_or_else(|| RpcError::app(format!("unknown file `{file}`")))?;
            return Ok(json!({ "text": self.analyze_file(f).canonical }));
        }
        Err(RpcError::invalid_params("canonical needs `def` or `file`"))
    }

    // ---- mutation -------------------------------------------------------

    fn apply_fix(&mut self, params: &Value) -> RpcResult {
        let base = self.snapshot(params)?.clone();
        let code = params
            .get("diagnosticCode")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("missing `diagnosticCode`"))?;
        let want_def = params.get("def").and_then(Value::as_str);

        // Find the matching diagnostic (and the file/def it belongs to).
        let mut target: Option<(String, DiagInfo)> = None;
        'outer: for f in &base.files {
            for d in self.analyze_file(f).diagnostics {
                if d.code == code
                    && want_def
                        .map(|w| d.def.as_deref() == Some(w))
                        .unwrap_or(true)
                {
                    target = Some((f.path.clone(), d));
                    break 'outer;
                }
            }
        }
        let (file_path, diag) = target.ok_or_else(|| {
            RpcError::app(format!("no `{code}` diagnostic to fix in this snapshot"))
        })?;
        let def_name = diag.def.clone();

        // Rebuild the snapshot with the target file repaired.
        let mut new_files = Vec::with_capacity(base.files.len());
        for f in &base.files {
            if f.path != file_path {
                new_files.push(f.clone());
                continue;
            }
            let repaired = match f.kind {
                SourceKind::Core => repair_core_text(&f.text, def_name.as_deref())
                    .ok_or_else(|| RpcError::app("could not repair Core file"))?,
                SourceKind::Source => {
                    // Source fixes are textual edits at the resolved offsets the
                    // front end now threads (MARV-12). Apply the first fix whose
                    // every edit carries a span; otherwise report that this
                    // diagnostic's fix has no resolvable source location yet.
                    let edits = diag
                        .fixes
                        .iter()
                        .find(|fx| {
                            !fx.edits.is_empty() && fx.edits.iter().all(|e| e.span.is_some())
                        })
                        .map(|fx| fx.edits.as_slice());
                    match edits {
                        Some(edits) => apply_src_edits(&f.text, edits)?,
                        None => {
                            return Err(RpcError::app(
                                "this fix has no resolvable source span (the reachable source \
                                 diagnostics today carry no insertion fix); the Core-level \
                                 capability/error repairs are mechanized over ingested Core",
                            ));
                        }
                    }
                }
            };
            let input = SourceFile::new(&self.db, f.path.clone(), f.kind, repaired.clone());
            new_files.push(SnapFile {
                path: f.path.clone(),
                kind: f.kind,
                text: repaired,
                input,
            });
        }
        let id = self.register(new_files);

        // Re-check the repaired snapshot, scoped to the repaired definition.
        let snap = self.snapshots.get(&id).unwrap().clone();
        let mut diags = Vec::new();
        for f in &snap.files {
            for d in self.analyze_file(f).diagnostics {
                if def_name.is_none() || d.def == def_name {
                    diags.push(diag_to_json(f, &d));
                }
            }
        }
        Ok(json!({ "snapshotId": id, "diagnostics": diags }))
    }

    fn format(&mut self, params: &Value) -> RpcResult {
        let base = self.snapshot(params)?.clone();
        let mut new_files = Vec::with_capacity(base.files.len());
        let mut wire = Vec::with_capacity(base.files.len());
        for f in &base.files {
            let canonical = self.analyze_file(f).canonical;
            wire.push(json!({ "path": f.path, "text": canonical }));
            if canonical == f.text {
                new_files.push(f.clone());
            } else {
                let input = SourceFile::new(&self.db, f.path.clone(), f.kind, canonical.clone());
                new_files.push(SnapFile {
                    path: f.path.clone(),
                    kind: f.kind,
                    text: canonical,
                    input,
                });
            }
        }
        let id = self.register(new_files);
        Ok(json!({ "snapshotId": id, "files": wire }))
    }
}

enum Direction {
    Callers,
    Callees,
}

/// Apply a fix's resolved edits to source text, replacing each edit's byte span
/// with its `new_text`. Edits are applied right-to-left (highest start byte
/// first) so earlier offsets stay valid (MARV-12).
fn apply_src_edits(text: &str, edits: &[marv_db::EditInfo]) -> Result<String, RpcError> {
    let mut spans: Vec<(usize, usize, &str)> = Vec::with_capacity(edits.len());
    for e in edits {
        let s = e
            .span
            .as_ref()
            .ok_or_else(|| RpcError::app("fix edit has no resolved span"))?;
        spans.push((s.start_byte as usize, s.end_byte as usize, &e.new_text));
    }
    spans.sort_by(|a, b| b.0.cmp(&a.0));
    let mut buf = text.to_string();
    for (start, end, new_text) in spans {
        if start > end
            || end > buf.len()
            || !buf.is_char_boundary(start)
            || !buf.is_char_boundary(end)
        {
            return Err(RpcError::app("fix edit span out of range"));
        }
        buf.replace_range(start..end, new_text);
    }
    Ok(buf)
}

/// Render a [`DiagInfo`] in the `spec/03` §2 wire shape. For source files the
/// `span` is the enclosing definition's header (MARV-12); it is `null` for
/// Core-ingested files (no source text). A fix edit carries its resolved span
/// where the front end can derive one (e.g. the `MissingCapability` parameter
/// point), else `null`; `newText` is always present.
fn diag_to_json(file: &SnapFile, d: &DiagInfo) -> Value {
    json!({
        "code": d.code,
        "severity": d.severity,
        "span": span_json(&file.path, d.span.as_ref()),
        "message": d.message,
        "related": d.related.iter()
            .map(|m| json!({"span": Value::Null, "message": m}))
            .collect::<Vec<_>>(),
        "fixes": d.fixes.iter().map(|fx| json!({
            "title": fx.title,
            "confidence": fx.confidence,
            "edits": fx.edits.iter()
                .map(|e| json!({"span": span_json(&file.path, e.span.as_ref()), "newText": e.new_text}))
                .collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// Render an optional [`SrcSpan`] in the `spec/03` §2 wire shape — a file plus a
/// UTF-8 byte interval and the `{line, col}` rendering of each endpoint — or
/// `null` when there is no span.
fn span_json(file: &str, span: Option<&SrcSpan>) -> Value {
    match span {
        Some(s) => json!({
            "file": file,
            "startByte": s.start_byte,
            "endByte": s.end_byte,
            "start": {"line": s.start_line, "col": s.start_col},
            "end": {"line": s.end_line, "col": s.end_col},
        }),
        None => Value::Null,
    }
}

/// Drive the server over a line-delimited JSON-RPC stream (one request object
/// per line; one response object per line). Returns on EOF.
pub fn serve<R: BufRead, W: Write>(
    server: &mut Server,
    reader: R,
    writer: &mut W,
) -> std::io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(req) => server.handle_request(req),
            Err(e) => json!({
                "jsonrpc": "2.0", "id": Value::Null,
                "error": {"code": -32700, "message": format!("parse error: {e}")}
            }),
        };
        writeln!(writer, "{}", serde_json::to_string(&response)?)?;
        writer.flush()?;
    }
    Ok(())
}

/// Render a [`marv_verify::VerifyOutcome`] as the `marv/verify` result object
/// (`spec/03` §4.3): `proved` / `failed` (with a counterexample) / `unsupported`
/// (with a Tier-1 runtime fallback note).
fn verify_result_json(outcome: &marv_verify::VerifyOutcome) -> Value {
    use marv_verify::VerifyOutcome;
    match outcome {
        VerifyOutcome::Proved => json!({ "status": "proved", "tier": 2 }),
        VerifyOutcome::Failed {
            obligation,
            counterexample,
            message,
        } => {
            let mut model = serde_json::Map::new();
            for (k, v) in counterexample {
                model.insert(k.clone(), scalar_json(v));
            }
            json!({
                "status": "failed",
                "obligation": obligation,
                "counterexample": Value::Object(model),
                "message": message,
            })
        }
        VerifyOutcome::Unsupported { reason } | VerifyOutcome::SolverUnavailable { reason } => {
            json!({
                "status": "unsupported",
                "reason": reason,
                "fallback": "runtime-checked (Tier 1)",
            })
        }
    }
}

/// Parse a model scalar back into a JSON number/bool where possible (so a
/// counterexample reads `{"x": -5}`, not `{"x": "-5"}`), else keep it a string.
fn scalar_json(v: &str) -> Value {
    if let Ok(n) = v.parse::<i64>() {
        return json!(n);
    }
    match v {
        "true" => json!(true),
        "false" => json!(false),
        other => json!(other),
    }
}
