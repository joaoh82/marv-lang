use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "marv-cli-wasm-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("create temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn marv() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_marv"));
    cmd.current_dir(repo_root());
    cmd
}

fn run(mut cmd: Command) -> Output {
    let output = cmd.output().expect("run command");
    if !output.status.success() {
        panic!(
            "command failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
}

#[test]
fn wasm_component_writes_component_and_wit_sidecar() {
    let tmp = TempDir::new("component");
    let wasm = tmp.path().join("factorial.wasm");
    let wit = tmp.path().join("factorial.wit");

    let mut build = marv();
    build
        .args(["build", "--target", "wasm-component", "--out"])
        .arg(&wasm)
        .args(["examples/factorial.mv", "--entry", "factorial"]);
    let output = run(build);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("via wasm-component"),
        "stderr should name the component target, got:\n{stderr}"
    );
    assert!(
        stderr.contains("WIT:"),
        "component builds should report the WIT sidecar, got:\n{stderr}"
    );

    let bytes = std::fs::read(&wasm).expect("read component");
    assert_eq!(
        &bytes[..8],
        &[0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00],
        "wasm-component should write a component artifact"
    );
    let wit = std::fs::read_to_string(&wit).expect("read WIT sidecar");
    assert!(
        wit.contains("export demo-factorial: func(arg0: s64) -> s64;"),
        "WIT should expose the selected export, got:\n{wit}"
    );
}

#[test]
fn wasm_core_writes_core_module_without_wit_sidecar() {
    let tmp = TempDir::new("core");
    let wasm = tmp.path().join("factorial.wasm");
    let wit = tmp.path().join("factorial.wit");

    let mut build = marv();
    build
        .args(["build", "--target", "wasm-core", "--out"])
        .arg(&wasm)
        .args(["examples/factorial.mv", "--entry", "factorial"]);
    let output = run(build);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("via wasm-core"),
        "stderr should name the core target, got:\n{stderr}"
    );

    let bytes = std::fs::read(&wasm).expect("read core module");
    assert_eq!(
        &bytes[..8],
        &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
        "wasm-core should preserve core-module output"
    );
    assert!(
        !wit.exists(),
        "core-module builds should not write a WIT sidecar"
    );
}
