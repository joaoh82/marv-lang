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

use std::path::{Path, PathBuf};

use marv_core::ir::Def;
use marv_core::lower_modules;
use marv_db::{qualify, CoreModuleSpec};
use marv_store::{DefMeta, StoredOpSig, StoredVariant};
use marv_syntax::{parse, Module};
use marv_types::{check_bounds, check_def, Diagnostic, Severity, World};

/// A loaded program: everything `check`/`build`/`run` need, independent of
/// whether it came from source or a Core snapshot.
pub struct Loaded {
    pub module_path: String,
    pub defs: Vec<(String, Def)>,
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

    // Resolve `import std.*` to its source modules so capability interfaces (and
    // any other std declarations) are in scope, and lower the whole set together
    // (`lower_modules` shares one constructor/interface/cap registry across it).
    // This is the minimal cross-module resolution MARV-6 needs; the persistent
    // on-disk store and general module graph are MARV-14.
    let std_modules = resolve_std_imports(&module, path)?;
    let mut all: Vec<Module> = Vec::with_capacity(1 + std_modules.len());
    all.push(module.clone());
    all.extend(std_modules);

    let lowered = lower_modules(&all).map_err(|e| LoadError::Front(format!("lower error: {e}")))?;
    let world = World::from_modules(&lowered);
    // Interface-bound and coherence checks over the whole set's generics metadata.
    let bound_diags = check_bounds(&lowered);

    // The user's file is module 0; `check`/`run` operate on its definitions
    // (the std modules are resolved-but-trusted library code).
    let store_meta = store_meta_for_module(lowered.first().expect("main module was lowered"));
    let main = lowered.into_iter().next().expect("main module was lowered");
    let param_names = main
        .defs
        .iter()
        .map(|e| fn_param_names(&module, &e.name))
        .collect();
    let defs = main.defs.into_iter().map(|e| (e.name, e.def)).collect();
    Ok(Loaded {
        module_path,
        defs,
        store_meta,
        param_names,
        world,
        bound_diags,
    })
}

/// Resolve every `import std.*` in `main` to its parsed source module, following
/// std→std imports transitively. Returns the std modules to lower alongside the
/// user's file. A non-`std` import is left unresolved (general cross-module
/// linking is MARV-14). Errors if the program imports std but no `std/` directory
/// can be found, or a named std module is missing.
fn resolve_std_imports(main: &Module, path: &Path) -> Result<Vec<Module>, LoadError> {
    let wanted: Vec<Vec<String>> = main
        .imports
        .iter()
        .map(|i| i.path.clone())
        .filter(|p| p.first().map(|s| s == "std").unwrap_or(false))
        .collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    let std_dir = find_std_dir(path).ok_or_else(|| {
        LoadError::Front(
            "program imports `std`, but no `std/` directory was found (set MARV_STD)".to_string(),
        )
    })?;

    // Parse every std source file once, indexing by its declared module path.
    let mut by_path: std::collections::HashMap<Vec<String>, Module> =
        std::collections::HashMap::new();
    let entries = std::fs::read_dir(&std_dir)
        .map_err(|e| LoadError::Io(format!("{}: {e}", std_dir.display())))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("mv") {
            continue;
        }
        let src = std::fs::read_to_string(&p)
            .map_err(|e| LoadError::Io(format!("{}: {e}", p.display())))?;
        let m = parse(&src)
            .map_err(|e| LoadError::Front(format!("parse error in {}: {e}", p.display())))?;
        by_path.insert(m.name.clone(), m);
    }

    // Transitively select the imported std modules (BFS over std→std imports). An
    // imported std module with no source file is *skipped*, not an error: general
    // cross-module resolution is MARV-14, so an unresolved name stays opaque to the
    // checker exactly as it did before std linking (e.g. `import std.collections`).
    let mut selected: Vec<Module> = Vec::new();
    let mut seen: std::collections::HashSet<Vec<String>> = std::collections::HashSet::new();
    let mut queue = wanted;
    while let Some(mp) = queue.pop() {
        if !seen.insert(mp.clone()) {
            continue;
        }
        let Some(m) = by_path.get(&mp) else {
            continue;
        };
        for imp in &m.imports {
            if imp.path.first().map(|s| s == "std").unwrap_or(false) {
                queue.push(imp.path.clone());
            }
        }
        selected.push(m.clone());
    }
    Ok(selected)
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
    let defs = spec.defs.into_iter().map(|d| (d.name, d.def)).collect();
    let store_meta = vec![DefMeta::default(); param_names.len()];
    Ok(Loaded {
        module_path,
        defs,
        store_meta,
        param_names,
        world,
        bound_diags: Vec::new(),
    })
}

fn store_meta_for_module(m: &marv_core::LoweredModule) -> Vec<DefMeta> {
    let cap_ops_by_name: std::collections::HashMap<&str, Vec<StoredOpSig>> = m
        .interfaces
        .iter()
        .filter(|iface| iface.is_capability)
        .map(|iface| {
            let ops = iface
                .method_sigs
                .iter()
                .map(|sig| StoredOpSig {
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
        })
        .collect()
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

    /// When the imported enum's source cannot be resolved (the module is not in
    /// `std/`), loading fails with the explicit unresolved-import error — not a
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
