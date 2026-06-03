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

use std::path::Path;

use marv_core::ir::Def;
use marv_core::lower_module;
use marv_db::{qualify, CoreModuleSpec};
use marv_syntax::parse;
use marv_types::{check_def, Diagnostic, Severity, World};

/// A loaded program: everything `check`/`build`/`run` need, independent of
/// whether it came from source or a Core snapshot.
pub struct Loaded {
    pub module_path: String,
    pub defs: Vec<(String, Def)>,
    /// Parameter names per definition, aligned with `defs`. Core erases names,
    /// so these come from the AST (source) or the snapshot's `params` field;
    /// `verify` uses them to label counterexamples.
    pub param_names: Vec<Vec<String>>,
    pub world: World,
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
        load_source(&src)
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

fn load_source(src: &str) -> Result<Loaded, LoadError> {
    let module = parse(src).map_err(|e| LoadError::Front(format!("parse error: {e}")))?;
    let module_path = module.name.join(".");
    let lowered =
        lower_module(&module).map_err(|e| LoadError::Front(format!("lower error: {e}")))?;
    let world = World::from_module(&lowered);
    // Parameter names per def (source order), recovered from the AST.
    let param_names = lowered
        .defs
        .iter()
        .map(|e| fn_param_names(&module, &e.name))
        .collect();
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    Ok(Loaded {
        module_path,
        defs,
        param_names,
        world,
    })
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
    let param_names = spec.defs.iter().map(|d| d.params.clone()).collect();
    let defs = spec.defs.into_iter().map(|d| (d.name, d.def)).collect();
    Ok(Loaded {
        module_path,
        defs,
        param_names,
        world,
    })
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
