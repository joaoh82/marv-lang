//! Golden + property tests for the canonical formatter (M0).
//!
//! Fixtures live at the repository root, not in this crate, so they are shared
//! across the workspace and match the layout in `spec/README.md`:
//!
//! - `tests/fmt/*.in.mv` paired with `*.out.mv` — formatter golden cases.
//! - `examples/*.mv` — sample programs that must already be canonical.
//!
//! Keep `examples/`, `tests/`, and `docs/` in sync with the formatter as it
//! grows (see CLAUDE.md, "Keeping examples, tests, and docs current").

use std::fs;
use std::path::{Path, PathBuf};

/// Walk up from this crate's manifest dir (`crates/marv-syntax`) to the repo root.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root is two levels above crates/marv-syntax")
        .to_path_buf()
}

#[test]
fn fmt_golden_fixtures() {
    let dir = repo_root().join("tests/fmt");
    let mut checked = 0;

    for entry in fs::read_dir(&dir).expect("read tests/fmt") {
        let in_path = entry.expect("dir entry").path();
        let file_name = in_path.file_name().unwrap().to_string_lossy().into_owned();
        if !file_name.ends_with(".in.mv") {
            continue;
        }

        let out_path = in_path.with_file_name(file_name.replace(".in.mv", ".out.mv"));
        let input = fs::read_to_string(&in_path).expect("read input fixture");
        let expected = fs::read_to_string(&out_path)
            .unwrap_or_else(|_| panic!("missing golden output {}", out_path.display()));

        let got = marv_syntax::format(&input);
        assert_eq!(got, expected, "format({file_name}) did not match golden output");

        // The formatter must be idempotent: formatting canonical input is a no-op.
        assert_eq!(
            marv_syntax::format(&got),
            got,
            "format is not idempotent for {file_name}"
        );

        checked += 1;
    }

    assert!(checked > 0, "no *.in.mv fixtures found in {}", dir.display());
}

#[test]
fn examples_are_canonical() {
    let dir = repo_root().join("examples");
    let mut checked = 0;

    for entry in fs::read_dir(&dir).expect("read examples") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("mv") {
            continue;
        }

        let src = fs::read_to_string(&path).expect("read example");
        assert_eq!(
            marv_syntax::format(&src),
            src,
            "example {} is not in canonical form; run `marv fmt {}`",
            path.display(),
            path.display()
        );

        checked += 1;
    }

    assert!(checked > 0, "no *.mv examples found in {}", dir.display());
}
