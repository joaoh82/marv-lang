//! Minimal package manifest loading for marv source projects.
//!
//! MARV-67 intentionally starts with a tiny deterministic `marv.toml` subset:
//!
//! ```toml
//! [package]
//! name = "app"
//! roots = ["src"]
//!
//! [dependencies.util]
//! path = "../util"
//! ```
//!
//! Every package root is scanned recursively under its declared roots. Local
//! path dependencies are loaded transitively and must have a matching
//! `[package].name`, so the dependency graph is deterministic and agent-readable.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use marv_syntax::{parse, Module};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageManifest {
    pub name: String,
    pub roots: Vec<String>,
    pub dependencies: Vec<PackageDependency>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDependency {
    pub name: String,
    pub path: String,
}

#[derive(Clone)]
pub struct PackageSource {
    pub package: String,
    pub path: PathBuf,
    pub text: String,
    pub module: Module,
}

pub struct PackageGraph {
    pub root: PathBuf,
    pub manifest: PackageManifest,
    pub sources: Vec<PackageSource>,
}

#[derive(Debug)]
pub enum PackageError {
    Io(String),
    Manifest(String),
    Source(String),
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageError::Io(e) => write!(f, "{e}"),
            PackageError::Manifest(e) => write!(f, "{e}"),
            PackageError::Source(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PackageError {}

pub fn find_package_root_for_file(path: &Path) -> Option<PathBuf> {
    let start = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut dir = start.parent();
    while let Some(d) = dir {
        if d.join("marv.toml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

pub fn load_package_containing(path: &Path) -> Result<Option<PackageGraph>, PackageError> {
    find_package_root_for_file(path)
        .map(|root| load_package(&root))
        .transpose()
}

pub fn load_package(root: &Path) -> Result<PackageGraph, PackageError> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let manifest = read_manifest(&root)?;
    let mut seen = BTreeSet::new();
    let mut sources = Vec::new();
    load_package_into(&root, &manifest.name, &mut seen, &mut sources)?;
    Ok(PackageGraph {
        root,
        manifest,
        sources,
    })
}

fn load_package_into(
    root: &Path,
    expected_name: &str,
    seen: &mut BTreeSet<PathBuf>,
    sources: &mut Vec<PackageSource>,
) -> Result<(), PackageError> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !seen.insert(root.clone()) {
        return Ok(());
    }
    let manifest = read_manifest(&root)?;
    if manifest.name != expected_name {
        return Err(PackageError::Manifest(format!(
            "{}: dependency name mismatch: manifest declares package `{}`, but dependency was declared as `{expected_name}`",
            root.join("marv.toml").display(),
            manifest.name
        )));
    }
    collect_package_sources(&root, &manifest, sources)?;
    for dep in &manifest.dependencies {
        let dep_root = root.join(&dep.path);
        load_package_into(&dep_root, &dep.name, seen, sources)?;
    }
    Ok(())
}

fn read_manifest(root: &Path) -> Result<PackageManifest, PackageError> {
    let path = root.join("marv.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| PackageError::Io(format!("{}: {e}", path.display())))?;
    parse_manifest(&text).map_err(|e| PackageError::Manifest(format!("{}: {e}", path.display())))
}

pub fn parse_manifest(text: &str) -> Result<PackageManifest, String> {
    #[derive(Clone)]
    enum Section {
        None,
        Package,
        Dependency(String),
    }

    let mut section = Section::None;
    let mut package_name: Option<String> = None;
    let mut roots: Vec<String> = Vec::new();
    let mut deps: HashMap<String, String> = HashMap::new();

    for (line_no, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                return Err(format!("line {}: unterminated section header", line_no + 1));
            }
            let name = &line[1..(line.len() - 1)];
            if name == "package" {
                section = Section::Package;
            } else if let Some(dep) = name.strip_prefix("dependencies.") {
                if dep.is_empty() {
                    return Err(format!("line {}: empty dependency name", line_no + 1));
                }
                section = Section::Dependency(dep.to_string());
            } else {
                return Err(format!("line {}: unknown section `{name}`", line_no + 1));
            }
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {}: expected `key = value`", line_no + 1));
        };
        let key = key.trim();
        let value = value.trim();
        match &section {
            Section::Package => match key {
                "name" => package_name = Some(parse_string(value, line_no + 1)?),
                "roots" => roots = parse_string_array(value, line_no + 1)?,
                other => {
                    return Err(format!(
                        "line {}: unknown package key `{other}`",
                        line_no + 1
                    ));
                }
            },
            Section::Dependency(name) => match key {
                "path" => {
                    if deps
                        .insert(name.clone(), parse_string(value, line_no + 1)?)
                        .is_some()
                    {
                        return Err(format!(
                            "line {}: duplicate dependency `{name}` path",
                            line_no + 1
                        ));
                    }
                }
                other => {
                    return Err(format!(
                        "line {}: unknown dependency key `{other}`",
                        line_no + 1
                    ));
                }
            },
            Section::None => {
                return Err(format!(
                    "line {}: key `{key}` appears before a section",
                    line_no + 1
                ));
            }
        }
    }

    let name = package_name.ok_or_else(|| "[package].name is required".to_string())?;
    if roots.is_empty() {
        roots.push(".".to_string());
    }
    let mut dependencies: Vec<_> = deps
        .into_iter()
        .map(|(name, path)| PackageDependency { name, path })
        .collect();
    dependencies.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(PackageManifest {
        name,
        roots,
        dependencies,
    })
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(idx) => &line[..idx],
        None => line,
    }
}

fn parse_string(value: &str, line: usize) -> Result<String, String> {
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        Ok(value[1..(value.len() - 1)].to_string())
    } else {
        Err(format!("line {line}: expected a double-quoted string"))
    }
}

fn parse_string_array(value: &str, line: usize) -> Result<Vec<String>, String> {
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(format!("line {line}: expected an array of strings"));
    }
    let body = value[1..(value.len() - 1)].trim();
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in body.split(',') {
        out.push(parse_string(part.trim(), line)?);
    }
    Ok(out)
}

fn collect_package_sources(
    root: &Path,
    manifest: &PackageManifest,
    out: &mut Vec<PackageSource>,
) -> Result<(), PackageError> {
    for rel in &manifest.roots {
        let dir = root.join(rel);
        collect_sources_dir(root, &manifest.name, &dir, out)?;
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(())
}

fn collect_sources_dir(
    package_root: &Path,
    package_name: &str,
    dir: &Path,
    out: &mut Vec<PackageSource>,
) -> Result<(), PackageError> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| PackageError::Io(format!("{}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| PackageError::Io(format!("{}: {e}", dir.display())))?;
        let path = entry.path();
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == ".git" || n == ".marv" || n == "target" || n == "std")
                .unwrap_or(false);
            if !skip {
                collect_sources_dir(package_root, package_name, &path, out)?;
            }
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("mv") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| PackageError::Io(format!("{}: {e}", path.display())))?;
        let module = parse(&text)
            .map_err(|e| PackageError::Source(format!("parse error in {}: {e}", path.display())))?;
        if module.name.first().map(|s| s.as_str()) != Some(package_name) {
            return Err(PackageError::Source(format!(
                "{}: module `{}` must live under package prefix `{package_name}`",
                path.display(),
                module.name.join(".")
            )));
        }
        let path = path
            .canonicalize()
            .unwrap_or_else(|_| package_root.join(path));
        out.push(PackageSource {
            package: package_name.to_string(),
            path,
            text,
            module,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest() {
        let manifest = parse_manifest(
            "[package]\nname = \"app\"\nroots = [\"src\"]\n\n[dependencies.util]\npath = \"../util\"\n",
        )
        .expect("manifest parses");
        assert_eq!(manifest.name, "app");
        assert_eq!(manifest.roots, vec!["src"]);
        assert_eq!(manifest.dependencies[0].name, "util");
        assert_eq!(manifest.dependencies[0].path, "../util");
    }

    #[test]
    fn default_roots_to_package_root() {
        let manifest = parse_manifest("[package]\nname = \"app\"\n").expect("manifest parses");
        assert_eq!(manifest.roots, vec!["."]);
    }
}
