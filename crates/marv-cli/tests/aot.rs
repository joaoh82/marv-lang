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
        let dir =
            std::env::temp_dir().join(format!("marv-cli-aot-{tag}-{}-{nanos}", std::process::id()));
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

fn cc_available() -> bool {
    Command::new("cc").arg("--version").output().is_ok()
}

#[test]
fn native_cranelift_writes_executable_and_deterministic_object() {
    let tmp = TempDir::new("factorial");
    let exe = tmp.path().join(if cfg!(windows) {
        "factorial.exe"
    } else {
        "factorial"
    });

    if cc_available() {
        let mut build = marv();
        build.args(["build", "--out"]).arg(&exe).args([
            "examples/factorial.mv",
            "--entry",
            "factorial",
        ]);
        run(build);

        let output = run({
            let mut cmd = Command::new(&exe);
            cmd.arg("6");
            cmd
        });
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "720");
    }

    let a = tmp.path().join("factorial-a.o");
    let b = tmp.path().join("factorial-b.o");
    for out in [&a, &b] {
        let mut build = marv();
        build
            .args(["build", "--emit", "object", "--out"])
            .arg(out)
            .args(["examples/factorial.mv", "--entry", "factorial"]);
        run(build);
    }

    let a_bytes = std::fs::read(&a).expect("read first object");
    let b_bytes = std::fs::read(&b).expect("read second object");
    assert!(!a_bytes.is_empty(), "object output should not be empty");
    assert_eq!(a_bytes, b_bytes, "object output should be deterministic");
}

#[test]
fn native_cranelift_executable_links_runtime_allocation_hooks() {
    if !cc_available() {
        return;
    }

    let tmp = TempDir::new("arrays");
    let exe = tmp.path().join(if cfg!(windows) {
        "arrays.exe"
    } else {
        "arrays"
    });

    let mut build = marv();
    build
        .args(["build", "--out"])
        .arg(&exe)
        .arg("examples/arrays.mv");
    run(build);

    let output = run(Command::new(&exe));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn native_aot_reports_reachable_capability_perform_without_artifact() {
    let tmp = TempDir::new("unsupported");
    let object = tmp.path().join("hello.o");

    let output = marv()
        .args(["build", "--emit", "object", "--out"])
        .arg(&object)
        .args(["examples/hello.mv", "--entry", "main"])
        .output()
        .expect("run marv");

    assert!(
        !output.status.success(),
        "capability perform should remain unsupported in native AOT"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported: capability perform"),
        "stderr should explain the unsupported construct, got:\n{stderr}"
    );
    assert!(
        !object.exists(),
        "unsupported native AOT build must not leave an object artifact"
    );
}
