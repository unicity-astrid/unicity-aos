#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Fixture {
    root: PathBuf,
    runtime: PathBuf,
    args: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "aos-cli-boundary-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create fixture");
        let runtime = root.join("fake-runtime");
        let args = root.join("args");
        let home = root.join("runtime-home");
        Self {
            root,
            runtime,
            args,
            home,
        }
    }

    fn install_runtime(&self, body: &str) {
        fs::write(&self.runtime, body).expect("write fake runtime");
        let mut permissions = fs::metadata(&self.runtime)
            .expect("runtime metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&self.runtime, permissions).expect("make runtime executable");
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aos"));
        command
            .env("UNICITY_AOS_HOME", &self.home)
            .env("UNICITY_AOS_RUNTIME_BIN", &self.runtime)
            .env("AOS_TEST_ARGS", &self.args)
            .env("AOS_TEST_HOME", self.root.join("child-home"));
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const RECORDING_RUNTIME: &str = r#"#!/bin/sh
for arg in "$@"; do
    printf '<%s>\n' "$arg"
done > "$AOS_TEST_ARGS"
printf '%s\n' "$ASTRID_HOME" > "$AOS_TEST_HOME"
exit "${AOS_TEST_EXIT:-0}"
"#;

#[test]
fn unowned_root_passes_through_with_argv_home_and_exit_code() {
    let fixture = Fixture::new("passthrough");
    fixture.install_runtime(RECORDING_RUNTIME);

    let output = fixture
        .command()
        .env("AOS_TEST_EXIT", "37")
        .args(["doctor", "--json", "space value", "$(not-a-shell)"])
        .output()
        .expect("run aos");

    assert_eq!(output.status.code(), Some(37));
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read delegated args"),
        "<doctor>\n<--json>\n<space value>\n<$(not-a-shell)>\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("child-home")).expect("read runtime home"),
        format!("{}\n", fixture.home.join("runtime").display())
    );
}

#[test]
fn product_help_version_and_usage_errors_never_delegate() {
    let fixture = Fixture::new("product-roots");
    fixture.install_runtime(RECORDING_RUNTIME);

    for (args, expected_success) in [
        (vec!["--help"], true),
        (vec!["--version"], true),
        (vec!["init", "--help"], true),
        (vec!["migrate"], false),
        (vec!["self-update", "unexpected"], false),
        (vec!["serve-health", "unexpected"], false),
    ] {
        let status = fixture
            .command()
            .args(args)
            .status()
            .expect("run product command");
        assert_eq!(status.success(), expected_success);
        assert!(!fixture.args.exists());
    }
}

#[test]
fn runtime_is_an_inherited_root_not_a_special_alias() {
    let fixture = Fixture::new("runtime-root");
    fixture.install_runtime(RECORDING_RUNTIME);

    let output = fixture
        .command()
        .args(["runtime", "status", "--json"])
        .output()
        .expect("run aos");

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read delegated args"),
        "<runtime>\n<status>\n<--json>\n"
    );
}

#[test]
fn product_init_pins_the_bundled_distro_and_rejects_overrides() {
    let fixture = Fixture::new("init");
    fixture.install_runtime(RECORDING_RUNTIME);

    let status = fixture
        .command()
        .args(["init", "--yes", "--var", "model=gpt-5"])
        .status()
        .expect("run product init");
    assert!(status.success());
    let args = fs::read_to_string(&fixture.args).expect("read init args");
    assert!(args.starts_with("<init>\n<--distro>\n"));
    assert!(args.contains("<--yes>\n<--var>\n<model=gpt-5>\n"));
    assert!(args.contains("/distributions/unicity-ce/Distro.toml>"));

    fs::remove_file(&fixture.args).expect("remove first invocation marker");
    let output = fixture
        .command()
        .args(["init", "--distro=other"])
        .output()
        .expect("run protected init");
    assert_eq!(output.status.code(), Some(1));
    assert!(!fixture.args.exists());
    assert!(
        String::from_utf8(output.stderr)
            .expect("utf8 stderr")
            .contains("astrid init")
    );
}

#[test]
fn native_status_does_not_invoke_the_runtime_cli() {
    let fixture = Fixture::new("status");
    fixture.install_runtime(RECORDING_RUNTIME);

    let output = fixture
        .command()
        .arg("status")
        .output()
        .expect("run aos status");

    assert!(!output.status.success());
    assert!(!fixture.args.exists());
    assert!(
        String::from_utf8(output.stderr)
            .expect("utf8 stderr")
            .contains("aos: runtime status unavailable")
    );
}

#[test]
fn unix_passthrough_preserves_signal_termination() {
    let fixture = Fixture::new("signal");
    let ready = fixture.root.join("ready");
    fixture.install_runtime(&format!(
        "#!/bin/sh\nprintf ready > '{}'\nexec sleep 30\n",
        shell_literal_path(&ready)
    ));

    let mut child = fixture
        .command()
        .arg("wait")
        .spawn()
        .expect("spawn inherited command");
    for _ in 0..200 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "runtime must replace the aos process");

    child.kill().expect("terminate delegated runtime");
    let status = child.wait().expect("wait for delegated runtime");
    assert_eq!(status.signal(), Some(9));
}

fn shell_literal_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}
