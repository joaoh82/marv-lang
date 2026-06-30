//! Shared front-to-back pipeline for the `check`, `build`, and `run`
//! subcommands.
//!
//! A workspace file is loaded one of two ways (`spec/03` §3.1): marv `.mv`
//! source is parsed and lowered through the front end, while a `.core.json`
//! file is a Core-IR snapshot ([`marv_db::CoreModuleSpec`]) ingested directly —
//! the only way, until the capability surface lands, to express a body that
//! `perform`s a capability (see `marv_db::corespec`). Both paths converge on the
//! same triple — module path, definitions, and the [`World`] they resolve
//! against — which `check`/`build`/`run` then consume identically.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use marv_core::ir::{Def, Hash, Type};
use marv_core::{lower_modules, symbol_hash};
use marv_db::{qualify, CoreModuleSpec};
use marv_package::{load_package_containing, PackageError, PackageGraph};
use marv_store::{DefMeta, StoredOpSig, StoredVariant};
use marv_syntax::{parse, Module};
use marv_types::{check_bounds, check_def, Diagnostic, Severity, World};

type ModuleIndex = HashMap<Vec<String>, Vec<(PathBuf, Module)>>;
type RuntimeDef = (Hash, String, Def);
type RuntimeAlias = (String, Hash);

/// A loaded program: everything `check`/`build`/`run` need, independent of
/// whether it came from source or a Core snapshot.
pub struct Loaded {
    pub module_path: String,
    pub defs: Vec<(String, Def)>,
    pub runtime_defs: Vec<RuntimeDef>,
    pub runtime_aliases: Vec<RuntimeAlias>,
    /// Declaration metadata aligned with `defs`, persisted into `marv-store`
    /// so fetched blobs can rebuild a hash-keyed declaration world.
    pub store_meta: Vec<DefMeta>,
    /// Parameter names per definition, aligned with `defs`. Core erases names,
    /// so these come from the AST (source) or the snapshot's `params` field;
    /// `verify` uses them to label counterexamples.
    pub param_names: Vec<Vec<String>>,
    pub world: World,
    /// Interface-bound / coherence diagnostics from monomorphization
    /// (`spec/01` §§3.3–3.4), already paired with a context name. Empty for a
    /// Core snapshot (which carries no generics metadata).
    pub bound_diags: Vec<(String, Diagnostic)>,
}

/// A failure to even load a file (before any checking).
pub enum LoadError {
    Io(String),
    /// A parse, lower, or Core-deserialization error, already formatted.
    Front(String),
    /// The file extension is neither `.mv` nor `.core.json`.
    UnknownKind(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "{e}"),
            LoadError::Front(e) => write!(f, "{e}"),
            LoadError::UnknownKind(p) => write!(
                f,
                "{p}: unrecognized extension (expected `.mv` source or `.core.json`)"
            ),
        }
    }
}

/// Load a file by extension: `.mv` → parse + lower; `*.core.json` → ingest Core.
pub fn load(path: &str) -> Result<Loaded, LoadError> {
    let src = std::fs::read_to_string(path).map_err(|e| LoadError::Io(format!("{path}: {e}")))?;
    if is_core_file(path) {
        load_core(&src)
    } else if path.ends_with(".mv") {
        load_source(&src, Path::new(path))
    } else {
        Err(LoadError::UnknownKind(path.to_string()))
    }
}

/// Whether a path names a Core-IR snapshot (`*.core.json`).
fn is_core_file(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    name.ends_with(".core.json")
}

fn load_source(src: &str, path: &Path) -> Result<Loaded, LoadError> {
    let module = parse(src).map_err(|e| LoadError::Front(format!("parse error: {e}")))?;
    let module_path = module.name.join(".");

    // Resolve source imports into a deterministic module set and lower the
    // package together (`lower_modules` shares one constructor/interface/cap
    // registry across it). The first module is always the user's entry file;
    // imported definitions are flattened below under their fully-qualified
    // names so run/build/commit can reach their bodies too.
    let imported_modules = resolve_source_imports(&module, path)?;
    let mut all: Vec<Module> = Vec::with_capacity(1 + imported_modules.len());
    all.push(module.clone());
    all.extend(imported_modules);

    let lowered = lower_modules(&all).map_err(|e| LoadError::Front(format!("lower error: {e}")))?;
    let world = World::from_modules(&lowered);
    // Interface-bound and coherence checks over the whole set's generics metadata.
    let bound_diags = check_bounds(&lowered);

    let (runtime_defs, runtime_aliases) = runtime_defs_for_modules(&lowered, &module_path);

    let mut defs = Vec::new();
    let mut store_meta = Vec::new();
    let mut param_names = Vec::new();
    for (idx, lowered_module) in lowered.into_iter().enumerate() {
        let ast_module = &all[idx];
        let prefix = ast_module.name.join(".");
        let metas = store_meta_for_module(&lowered_module, ast_module);
        for (entry, meta) in lowered_module.defs.into_iter().zip(metas.into_iter()) {
            let name = if idx == 0 {
                entry.name.clone()
            } else {
                qualify(&prefix, &entry.name)
            };
            param_names.push(fn_param_names(ast_module, &entry.name));
            store_meta.push(meta);
            defs.push((name, entry.def));
        }
    }
    Ok(Loaded {
        module_path,
        defs,
        runtime_defs,
        runtime_aliases,
        store_meta,
        param_names,
        world,
        bound_diags,
    })
}

/// Resolve every source import in `main` to parsed modules, following imports
/// transitively. `std.*` is discovered from `MARV_STD` or the nearest `std/`
/// directory as before; non-`std` modules are discovered from the nearest
/// ancestor with `marv.toml`, falling back to the entry file's directory. The
/// index keys on each file's declared `mod` path, so file names remain an
/// implementation detail and duplicate module declarations are reported
/// explicitly.
fn resolve_source_imports(main: &Module, path: &Path) -> Result<Vec<Module>, LoadError> {
    if main.imports.is_empty() {
        return Ok(Vec::new());
    }

    let package_graph = load_package_containing(path).map_err(package_error)?;
    let fallback_root;
    let project_root;
    let (project_index, project_label) = if let Some(graph) = package_graph.as_ref() {
        project_root = graph.root.clone();
        (
            module_index_from_package(graph),
            format!(
                "package `{}` at {}",
                graph.manifest.name,
                graph.root.display()
            ),
        )
    } else {
        fallback_root = find_project_root(path);
        project_root = fallback_root.clone();
        (
            module_index(&fallback_root, true, Some("std"))?,
            format!("the source root {}", fallback_root.display()),
        )
    };
    let std_index = find_std_dir(path)
        .map(|std_dir| module_index(&std_dir, false, None))
        .transpose()?;

    let mut selected: Vec<Module> = Vec::new();
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    seen.insert(main.name.clone());
    let mut queue: VecDeque<Vec<String>> = main.imports.iter().map(|i| i.path.clone()).collect();
    while let Some(mp) = queue.pop_front() {
        if !seen.insert(mp.clone()) {
            continue;
        }
        let m = if mp.first().map(|s| s == "std").unwrap_or(false) {
            let Some(idx) = std_index.as_ref() else {
                continue;
            };
            let Ok(module) = resolve_indexed_module(idx, &mp, Path::new("std"), "std") else {
                continue;
            };
            module
        } else {
            resolve_indexed_module(&project_index, &mp, &project_root, &project_label)?
        };
        for imp in &m.imports {
            queue.push_back(imp.path.clone());
        }
        selected.push(m.clone());
    }
    Ok(selected)
}

fn package_error(e: PackageError) -> LoadError {
    match e {
        PackageError::Io(e) => LoadError::Io(e),
        PackageError::Manifest(e) | PackageError::Source(e) => LoadError::Front(e),
    }
}

fn module_index_from_package(graph: &PackageGraph) -> ModuleIndex {
    let mut index = HashMap::new();
    for source in &graph.sources {
        index
            .entry(source.module.name.clone())
            .or_insert_with(Vec::new)
            .push((source.path.clone(), source.module.clone()));
    }
    index
}

fn find_project_root(path: &Path) -> PathBuf {
    let start = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut dir = start.parent();
    while let Some(d) = dir {
        if d.join("marv.toml").is_file() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    start.parent().unwrap_or(Path::new(".")).to_path_buf()
}

fn module_index(
    root: &Path,
    recursive: bool,
    skip_dir: Option<&str>,
) -> Result<ModuleIndex, LoadError> {
    let mut index = HashMap::new();
    collect_modules(root, recursive, skip_dir, &mut index)?;
    Ok(index)
}

fn resolve_indexed_module<'a>(
    index: &'a ModuleIndex,
    mp: &[String],
    root: &Path,
    label: &str,
) -> Result<&'a Module, LoadError> {
    match index.get(mp).map(|v| v.as_slice()) {
        Some([(_, module)]) => Ok(module),
        Some(candidates) => Err(LoadError::Front(format!(
            "ambiguous import `{}` under {}: {} source files declare that `mod` path ({})",
            mp.join("."),
            root.display(),
            candidates.len(),
            candidates
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
        None => Err(LoadError::Front(format!(
            "cannot resolve import `{}`: no source module with that `mod` path was found in {label}",
            mp.join(".")
        ))),
    }
}

fn collect_modules(
    dir: &Path,
    recursive: bool,
    skip_dir: Option<&str>,
    out: &mut ModuleIndex,
) -> Result<(), LoadError> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| LoadError::Io(format!("{}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| LoadError::Io(format!("{}: {e}", dir.display())))?;
        let p = entry.path();
        if p.is_dir() {
            if recursive {
                if skip_dir
                    .and_then(|s| p.file_name().and_then(|n| n.to_str()).map(|n| n == s))
                    .unwrap_or(false)
                {
                    continue;
                }
                collect_modules(&p, recursive, skip_dir, out)?;
            }
            continue;
        }
        if p.extension().and_then(|s| s.to_str()) != Some("mv") {
            continue;
        }
        let src = std::fs::read_to_string(&p)
            .map_err(|e| LoadError::Io(format!("{}: {e}", p.display())))?;
        let m = parse(&src)
            .map_err(|e| LoadError::Front(format!("parse error in {}: {e}", p.display())))?;
        out.entry(m.name.clone()).or_default().push((p, m));
    }
    Ok(())
}

/// Locate the `std/` source directory: the `MARV_STD` environment variable if
/// set, otherwise the nearest ancestor of the source file that contains a `std/`
/// directory with `.mv` files.
fn find_std_dir(path: &Path) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MARV_STD") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }
    let start = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut dir = start.parent();
    while let Some(d) = dir {
        let cand = d.join("std");
        if cand.is_dir()
            && std::fs::read_dir(&cand)
                .map(|mut it| {
                    it.any(|e| {
                        e.ok()
                            .map(|e| e.path().extension().and_then(|s| s.to_str()) == Some("mv"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        {
            return Some(cand);
        }
        dir = d.parent();
    }
    None
}

/// The parameter names of a named function in the AST (empty for non-functions).
fn fn_param_names(module: &marv_syntax::Module, name: &str) -> Vec<String> {
    for item in &module.items {
        if let marv_syntax::Item::Fn(f) = item {
            if f.name == name {
                return f.params.iter().map(|p| p.name.clone()).collect();
            }
        }
    }
    Vec::new()
}

fn load_core(src: &str) -> Result<Loaded, LoadError> {
    let spec: CoreModuleSpec = serde_json::from_str(src)
        .map_err(|e| LoadError::Front(format!("core ingest error: {e}")))?;
    let world = spec.world.build();
    let module_path = spec.module.clone();
    let param_names: Vec<Vec<String>> = spec.defs.iter().map(|d| d.params.clone()).collect();
    let defs: Vec<(String, Def)> = spec.defs.into_iter().map(|d| (d.name, d.def)).collect();
    let (runtime_defs, runtime_aliases) = runtime_defs_for_defs(&module_path, &defs);
    let store_meta = vec![DefMeta::default(); param_names.len()];
    Ok(Loaded {
        module_path,
        defs,
        runtime_defs,
        runtime_aliases,
        store_meta,
        param_names,
        world,
        bound_diags: Vec::new(),
    })
}

fn runtime_defs_for_modules(
    modules: &[marv_core::LoweredModule],
    main_module_path: &str,
) -> (Vec<RuntimeDef>, Vec<RuntimeAlias>) {
    let mut defs = Vec::new();
    let mut aliases = Vec::new();
    for module in modules {
        let module_path = module.module.join(".");
        for entry in &module.defs {
            let qualified = qualify(&module_path, &entry.name);
            let h = symbol_hash(&qualified);
            defs.push((h, qualified.clone(), entry.def.clone()));
            if module_path == main_module_path {
                aliases.push((qualified, h));
                aliases.push((entry.name.clone(), h));
            }
        }
    }
    (defs, aliases)
}

fn runtime_defs_for_defs(
    module_path: &str,
    defs: &[(String, Def)],
) -> (Vec<RuntimeDef>, Vec<RuntimeAlias>) {
    let mut runtime_defs = Vec::new();
    let mut aliases = Vec::new();
    for (name, def) in defs {
        let qualified = qualify(module_path, name);
        let h = symbol_hash(&qualified);
        runtime_defs.push((h, qualified.clone(), def.clone()));
        aliases.push((qualified, h));
        aliases.push((name.clone(), h));
    }
    (runtime_defs, aliases)
}

fn store_meta_for_module(
    m: &marv_core::LoweredModule,
    ast_module: &marv_syntax::Module,
) -> Vec<DefMeta> {
    let cap_ops_by_name: std::collections::HashMap<&str, Vec<StoredOpSig>> = m
        .interfaces
        .iter()
        .filter(|iface| iface.is_capability)
        .map(|iface| {
            let ops = iface
                .method_sigs
                .iter()
                .map(|sig| StoredOpSig {
                    consumes_receiver: matches!(sig.params.first(), Some(Type::Linear(_))),
                    // Drop the receiver; `Perform` operands carry only the
                    // non-receiver arguments.
                    params: sig.params.iter().skip(1).cloned().collect(),
                    ret: sig.ret.clone(),
                    errors: Vec::new(),
                })
                .collect();
            (iface.name.as_str(), ops)
        })
        .collect();

    m.defs
        .iter()
        .map(|entry| DefMeta {
            enum_variants: entry
                .enum_variants
                .as_ref()
                .map(|vars| {
                    vars.iter()
                        .map(|v| StoredVariant {
                            name: v.name.clone(),
                            fields: v.fields.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            capability_ops: cap_ops_by_name
                .get(entry.name.as_str())
                .cloned()
                .unwrap_or_default(),
            unsafe_sites: unsafe_sites_for_fn(ast_module, &entry.name),
        })
        .collect()
}

fn unsafe_sites_for_fn(module: &marv_syntax::Module, name: &str) -> Vec<String> {
    for item in &module.items {
        if let marv_syntax::Item::Fn(f) = item {
            if f.name == name && f.is_unsafe {
                return f
                    .docs
                    .iter()
                    .find_map(|d| d.trim_start().strip_prefix("SAFETY:"))
                    .map(|s| vec![s.trim().to_string()])
                    .unwrap_or_else(|| vec![String::new()]);
            }
        }
    }
    Vec::new()
}

impl Loaded {
    /// Run the M2 checker over every definition, returning each diagnostic
    /// paired with the qualified name of the definition it was raised in, in
    /// source order.
    pub fn check(&self) -> Vec<(String, Diagnostic)> {
        let mut out = Vec::new();
        for (name, def) in &self.defs {
            let qualified = qualify(&self.module_path, name);
            for d in check_def(&self.world, def, Some(name)) {
                out.push((qualified.clone(), d));
            }
        }
        // Interface-bound / coherence diagnostics (monomorphization,
        // `spec/01` §§3.3–3.4) come pre-paired with their context name.
        out.extend(self.bound_diags.iter().cloned());
        out
    }
}

/// Whether any diagnostic is error severity (the gate `build`/`run` enforce
/// before producing or executing code — `spec/03` §5 step 2).
pub fn any_errors(diags: &[(String, Diagnostic)]) -> bool {
    diags.iter().any(|(_, d)| d.severity == Severity::Error)
}

/// Print diagnostics to stderr in a stable, human- and machine-skimmable form,
/// echoing the fix titles the checker attached (`spec/03` §2 — fix-carrying
/// diagnostics).
pub fn print_diags(diags: &[(String, Diagnostic)]) {
    for (def, d) in diags {
        eprintln!(
            "{}[{}] {}: {}",
            d.severity.as_str(),
            d.code.as_str(),
            def,
            d.message
        );
        for r in &d.related {
            eprintln!("    note: {}", r.message);
        }
        for fix in &d.fixes {
            eprintln!("    fix: {} (confidence {:.2})", fix.title, fix.confidence);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marv_core::ir::{Core, Hash};
    use marv_core::symbol_hash;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repo root is two levels above crates/marv-cli")
            .to_path_buf()
    }

    /// Every `Ctor` in a Core term, as `(nominal hash, tag, field count)`.
    fn collect_ctors(c: &Core, out: &mut Vec<(Hash, u32, usize)>) {
        match c {
            Core::Ctor { ty, tag, fields } => out.push((*ty, *tag, fields.len())),
            Core::Let { value, body } => {
                collect_ctors(value, out);
                collect_ctors(body, out);
            }
            Core::Lam { body, .. } => collect_ctors(body, out),
            Core::Match { branches, .. } => {
                branches.iter().for_each(|b| collect_ctors(&b.body, out))
            }
            Core::Loop { cond, body, .. } => {
                collect_ctors(cond, out);
                collect_ctors(body, out);
            }
            _ => {}
        }
    }

    fn ctors_of(loaded: &Loaded, name: &str) -> Vec<(Hash, u32, usize)> {
        let def = &loaded
            .defs
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no def `{name}`"))
            .1;
        let mut out = Vec::new();
        if let Some(body) = &def.body {
            collect_ctors(body, &mut out);
        }
        out
    }

    /// The MARV-18 acceptance path: `marv check std/result.mv` as a single file
    /// resolves the imported `Option`'s constructors to real `Ctor`s with the
    /// `std.option.Option` nominal and declaration-order tags, and checks clean.
    #[test]
    fn single_file_check_of_std_result_is_clean() {
        let path = repo_root().join("std/result.mv");
        let loaded = load(path.to_str().unwrap()).unwrap_or_else(|e| panic!("load: {e}"));
        let diags = loaded.check();
        assert!(
            !any_errors(&diags),
            "expected a clean check, got: {:?}",
            diags
                .iter()
                .map(|(n, d)| format!("{n}: {}", d.message))
                .collect::<Vec<_>>()
        );
        let option = symbol_hash("std.option.Option");
        let ctors = ctors_of(&loaded, "ok");
        assert!(
            ctors.contains(&(option, 1, 1)),
            "`Option.Some(x)` is a tag-1 Ctor of std.option.Option: {ctors:?}"
        );
        assert!(
            ctors.contains(&(option, 0, 0)),
            "`Option.None` is a tag-0 Ctor of std.option.Option: {ctors:?}"
        );
    }

    /// A scratch workspace: `<tmp>/std/` holds the prelude modules given as
    /// `(file name, source)`, and `<tmp>/<main>` holds the file under test
    /// (`find_std_dir` discovers `std/` as the file's sibling). The directory is
    /// removed on drop.
    struct Workspace {
        dir: PathBuf,
        main: PathBuf,
    }

    impl Workspace {
        fn new(tag: &str, std_files: &[(&str, &str)], main: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("marv18-{tag}-{}", std::process::id()));
            let std_dir = dir.join("std");
            std::fs::create_dir_all(&std_dir).expect("create std dir");
            for (name, src) in std_files {
                std::fs::write(std_dir.join(name), src).expect("write std file");
            }
            let main_path = dir.join("main.mv");
            std::fs::write(&main_path, main).expect("write main file");
            Workspace {
                dir,
                main: main_path,
            }
        }

        fn write(&self, rel: &str, src: &str) -> PathBuf {
            let path = self.dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create parent dir");
            }
            std::fs::write(&path, src).expect("write package file");
            path
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const PALETTE: &str = "\
mod std.palette

enum Color {
    Red,
    Rgb(i64),
}
";

    /// The single-file path over an arbitrary (non-`std/result.mv`) module that
    /// imports, constructs, and matches an enum from another module.
    #[test]
    fn single_file_path_resolves_imported_enum() {
        let ws = Workspace::new(
            "ok",
            &[("palette.mv", PALETTE)],
            "mod main\nimport std.palette (Color)\n\npure fn wrap(x: i64) -> Color {\n    \
             Color.Rgb(x)\n}\n\npure fn brightness(c: Color) -> i64 {\n    match c {\n        \
             Color.Red => 100,\n        Color.Rgb(v) => v,\n    }\n}\n",
        );
        let loaded = load(ws.main.to_str().unwrap()).unwrap_or_else(|e| panic!("load: {e}"));
        let diags = loaded.check();
        assert!(
            !any_errors(&diags),
            "expected a clean check, got: {:?}",
            diags
                .iter()
                .map(|(n, d)| format!("{n}: {}", d.message))
                .collect::<Vec<_>>()
        );
        let color = symbol_hash("std.palette.Color");
        let ctors = ctors_of(&loaded, "wrap");
        assert!(
            ctors.contains(&(color, 1, 1)),
            "`Color.Rgb(x)` is a tag-1 Ctor of std.palette.Color: {ctors:?}"
        );
    }

    /// The MARV-8 acceptance path: `examples/geometry.mv` mixes
    /// backend-supported functions (`max`) with ones the Cranelift backend
    /// cannot lower yet (`translate`'s method call). Building pruned to the
    /// `max` entry succeeds; whole-module compilation (the audit path) still
    /// refuses the same module.
    #[test]
    fn build_of_geometry_prunes_to_the_entry() {
        let path = repo_root().join("examples/geometry.mv");
        let loaded = load(path.to_str().unwrap()).unwrap_or_else(|e| panic!("load: {e}"));
        assert!(!any_errors(&loaded.check()), "geometry.mv checks clean");

        let whole = marv_codegen_cl::compile(&loaded.module_path, &loaded.defs, &loaded.world);
        assert!(
            whole.is_err(),
            "whole-module compilation still rejects the unsupported sibling"
        );

        let opts = marv_codegen_cl::Options::default();
        let jit = marv_codegen_cl::compile_reachable(
            &loaded.module_path,
            &loaded.defs,
            &loaded.world,
            &opts,
            "max",
        )
        .unwrap_or_else(|e| panic!("pruned compile of geometry.mv: {e}"));
        assert_eq!(jit.run_i64("max", &[3, 7]).expect("run max"), 7);
    }

    #[test]
    fn local_source_imports_are_runnable_buildable_and_committable() {
        let ws = Workspace::new(
            "pkg",
            &[],
            "mod app\nimport math (double)\n\npure fn main() -> i64 {\n    double(21)\n}\n",
        );
        ws.write(
            "math.mv",
            "mod math\n\npure fn double(x: i64) -> i64 {\n    (x * 2)\n}\n",
        );

        let loaded = load(ws.main.to_str().unwrap()).unwrap_or_else(|e| panic!("load: {e}"));
        assert!(!any_errors(&loaded.check()), "package should check cleanly");
        assert!(
            loaded.defs.iter().any(|(name, _)| name == "math.double"),
            "imported bodies are part of the loaded definition set"
        );

        let program = marv_interp::Program::new(
            &loaded.module_path,
            loaded.defs.clone(),
            loaded.world.clone(),
        );
        assert_eq!(
            program
                .run("", &[], &[])
                .expect("run package main")
                .value
                .render(),
            "42"
        );

        let opts = marv_codegen_cl::Options::default();
        let jit = marv_codegen_cl::compile_reachable(
            &loaded.module_path,
            &loaded.defs,
            &loaded.world,
            &opts,
            "",
        )
        .unwrap_or_else(|e| panic!("compile package: {e}"));
        assert_eq!(jit.run_i64("", &[]).expect("jit package main"), 42);

        let mut store = marv_store::Store::new();
        let mut lock = marv_store::Lockfile::new();
        let report = marv_store::commit_with_meta(
            &mut store,
            &mut lock,
            &loaded.module_path,
            &loaded.store_entries(),
        );
        assert_eq!(report.added(), 2);
        assert!(lock.bindings.contains_key("app.main"));
        assert!(lock.bindings.contains_key("math.double"));
    }

    #[test]
    fn manifest_package_loads_local_path_dependency() {
        let ws = Workspace::new(
            "manifest",
            &[],
            "mod scratch\n\npure fn unused() -> i64 {\n    0\n}\n",
        );
        let app_main = ws.write(
            "app/src/main.mv",
            "mod app.main\nimport util.math (double)\n\npure fn main() -> i64 {\n    double(21)\n}\n",
        );
        ws.write(
            "app/marv.toml",
            "[package]\nname = \"app\"\nroots = [\"src\"]\n\n[dependencies.util]\npath = \"../util\"\n",
        );
        ws.write(
            "util/marv.toml",
            "[package]\nname = \"util\"\nroots = [\"src\"]\n",
        );
        ws.write(
            "util/src/math.mv",
            "mod util.math\n\npure fn double(x: i64) -> i64 {\n    (x * 2)\n}\n",
        );

        let loaded = load(app_main.to_str().unwrap()).unwrap_or_else(|e| panic!("load: {e}"));
        assert!(!any_errors(&loaded.check()), "package should check cleanly");
        assert_eq!(loaded.module_path, "app.main");
        assert!(
            loaded
                .defs
                .iter()
                .any(|(name, _)| name == "util.math.double"),
            "dependency bodies are part of the manifest-controlled module set"
        );

        let program = marv_interp::Program::new(
            &loaded.module_path,
            loaded.defs.clone(),
            loaded.world.clone(),
        );
        assert_eq!(
            program
                .run("", &[], &[])
                .expect("run manifest package")
                .value
                .render(),
            "42"
        );

        let mut store = marv_store::Store::new();
        let mut lock = marv_store::Lockfile::new();
        let report = marv_store::commit_with_meta(
            &mut store,
            &mut lock,
            &loaded.module_path,
            &loaded.store_entries(),
        );
        assert_eq!(report.added(), 2);
        assert!(lock.bindings.contains_key("app.main.main"));
        assert!(lock.bindings.contains_key("util.math.double"));
    }

    #[test]
    fn missing_local_import_is_a_clear_load_error() {
        let ws = Workspace::new(
            "missing",
            &[],
            "mod app\nimport math (double)\n\npure fn main() -> i64 {\n    double(21)\n}\n",
        );
        let err = match load(ws.main.to_str().unwrap()) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("load should fail when a local import is missing"),
        };
        assert!(
            err.contains("cannot resolve import `math`"),
            "error names the missing import: {err}"
        );
    }

    /// When the imported enum's source cannot be resolved (the module is not in
    /// `std/`), loading fails with an explicit import-resolution error — not a
    /// misleading projection error or a silently wrong lowering.
    #[test]
    fn unresolvable_imported_enum_is_a_clear_error() {
        let ws = Workspace::new(
            "err",
            // `std/` exists (so discovery succeeds) but has no `palette` module.
            &[(
                "option.mv",
                "mod std.option\n\nenum Option[T] {\n    None,\n    Some(T),\n}\n",
            )],
            "mod main\nimport std.palette (Color)\n\npure fn wrap(x: i64) -> Color {\n    \
             Color.Rgb(x)\n}\n",
        );
        let err = match load(ws.main.to_str().unwrap()) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("load should fail when the imported enum is unresolvable"),
        };
        assert!(
            err.contains("cannot resolve `Color`") && err.contains("std.palette"),
            "error names the import and its module: {err}"
        );
    }
}
