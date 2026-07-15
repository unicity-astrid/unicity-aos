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
            .env("AOS_TEST_HOME", self.root.join("child-home"))
            .env("AOS_TEST_WORKSPACE", self.root.join("child-workspace"))
            .env("AOS_TEST_DISTRO", self.root.join("child-distro"));
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
printf '%s\n' "$ASTRID_WORKSPACE_STATE_DIR" > "$AOS_TEST_WORKSPACE"
printf '%s\n' "$ASTRID_ENFORCED_DISTRO" > "$AOS_TEST_DISTRO"
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
    assert_eq!(
        fs::read_to_string(fixture.root.join("child-workspace")).expect("read workspace"),
        ".unicity-os\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("child-distro")).expect("read distro"),
        format!(
            "{}\n",
            fixture
                .home
                .join("distributions/unicity-ce/Distro.toml")
                .display()
        )
    );
}

#[test]
fn leading_runtime_globals_on_unowned_roots_pass_through_exactly() {
    let fixture = Fixture::new("leading-global-passthrough");
    fixture.install_runtime(RECORDING_RUNTIME);

    let output = fixture
        .command()
        .args(["--principal", "alice", "doctor", "--json"])
        .output()
        .expect("run inherited command with a leading global");

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read delegated args"),
        "<--principal>\n<alice>\n<doctor>\n<--json>\n"
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
        (vec!["init", "--grant-capsules"], false),
        (vec!["init", "--principal", "alice"], false),
        (vec!["migrate"], false),
        (vec!["update", "unexpected"], false),
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
fn bare_aos_shows_product_help_instead_of_claiming_native_chat() {
    let fixture = Fixture::new("bare-help");
    fixture.install_runtime(RECORDING_RUNTIME);

    let output = fixture.command().output().expect("run bare aos");

    assert!(output.status.success());
    assert!(!fixture.args.exists());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("Running `aos` without a command displays product help"));
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
fn product_default_init_delegates_grants_without_inventing_a_target() {
    let fixture = Fixture::new("init-default");
    fixture.install_runtime(RECORDING_RUNTIME);

    let status = fixture
        .command()
        .args(["init"])
        .status()
        .expect("run product init");
    assert!(status.success());
    let args = fs::read_to_string(&fixture.args).expect("read init args");
    assert_eq!(args, "<init>\n<--grant-capsules>\n");
    assert_eq!(
        fs::read_to_string(fixture.root.join("child-distro")).expect("read enforced distro"),
        format!(
            "{}\n",
            fixture
                .home
                .join("distributions/unicity-ce/Distro.toml")
                .display()
        )
    );
}

#[test]
fn product_non_default_init_delegates_principal_and_capsule_grants() {
    let fixture = Fixture::new("init-principal");
    fixture.install_runtime(RECORDING_RUNTIME);

    let status = fixture
        .command()
        .args([
            "init",
            "--target-principal",
            "alice",
            "--yes",
            "--var",
            "model=gpt-5",
        ])
        .status()
        .expect("run product init for a non-default principal");
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read non-default init args"),
        "<init>\n<--target-principal>\n<alice>\n<--yes>\n<--var>\n<model=gpt-5>\n<--grant-capsules>\n"
    );
}

#[test]
fn product_init_preserves_authenticated_operator_and_separate_target() {
    let fixture = Fixture::new("init-operator-target");
    fixture.install_runtime(RECORDING_RUNTIME);

    let status = fixture
        .command()
        .args([
            "--principal",
            "operator",
            "init",
            "--target-principal",
            "alice",
            "--yes",
        ])
        .status()
        .expect("run product init with an explicit operator");

    assert!(status.success());
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read operator init args"),
        "<--principal>\n<operator>\n<init>\n<--target-principal>\n<alice>\n<--yes>\n<--grant-capsules>\n"
    );
}

#[test]
fn product_init_rejects_caller_distro_selection() {
    let fixture = Fixture::new("init-distro-override");
    fixture.install_runtime(RECORDING_RUNTIME);

    let output = fixture
        .command()
        .args(["init", "--distro=other"])
        .output()
        .expect("run protected init");
    assert_eq!(output.status.code(), Some(2));
    assert!(!fixture.args.exists());
    assert!(
        String::from_utf8(output.stderr)
            .expect("utf8 stderr")
            .contains("unexpected argument '--distro'")
    );
}

#[test]
fn unsupported_leading_globals_cannot_bypass_product_roots() {
    let fixture = Fixture::new("leading-global");
    fixture.install_runtime(RECORDING_RUNTIME);

    for args in [
        vec!["--principal", "alice", "status"],
        vec!["--format", "json", "init"],
        vec!["-p", "prompt text", "init"],
        vec!["--principal", "alice", "update"],
    ] {
        let output = fixture
            .command()
            .args(args)
            .output()
            .expect("run protected product root");

        assert_eq!(output.status.code(), Some(2));
        assert!(!fixture.args.exists());
    }
}

#[test]
fn malformed_or_ambiguous_product_principals_never_delegate() {
    let fixture = Fixture::new("malformed-principals");
    fixture.install_runtime(RECORDING_RUNTIME);

    for args in [
        vec!["--principal", "init"],
        vec!["--principal", "init", "--yes"],
        vec!["--principal=", "init"],
        vec!["--principal", "operator", "init", "--target-principal"],
        vec!["--principal", "operator", "init", "--target-principal="],
        vec!["--principal", "operator", "--principal", "other", "init"],
    ] {
        let output = fixture
            .command()
            .args(args)
            .output()
            .expect("run malformed product invocation");

        assert_eq!(output.status.code(), Some(2));
        assert!(!fixture.args.exists());
    }
}

#[test]
fn product_owns_and_refuses_distro_mutation() {
    let fixture = Fixture::new("owned-distro");
    fixture.install_runtime(RECORDING_RUNTIME);

    for args in [
        vec!["distro", "apply", "https://example.invalid/other.toml"],
        vec!["--principal", "operator", "distro", "apply", "other"],
    ] {
        let output = fixture
            .command()
            .args(args)
            .output()
            .expect("run protected distro command");

        assert_eq!(output.status.code(), Some(2));
        assert!(!fixture.args.exists());
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(stderr.contains("Unicity CE owns the distribution state"));
        assert!(stderr.contains("standalone `astrid distro ...`"));
    }
}

#[test]
fn direct_update_fails_closed_without_running_an_installer() {
    let fixture = Fixture::new("direct-update");
    fixture.install_runtime(RECORDING_RUNTIME);

    for alias in ["update", "self-update", "self_update"] {
        let output = fixture
            .command()
            .env_remove("UNICITY_AOS_INSTALL_METHOD")
            .arg(alias)
            .output()
            .expect("run staged direct update");

        assert!(!output.status.success());
        assert!(!fixture.args.exists());
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(stderr.contains("2026.1.0 is staged"));
        assert!(stderr.contains("no signed stable, dev, or nightly AOS update channel"));
    }
}

#[test]
fn homebrew_update_uses_the_formula_upgrade_path() {
    let fixture = Fixture::new("homebrew-update");
    fixture.install_runtime(RECORDING_RUNTIME);
    let bin = fixture.root.join("bin");
    fs::create_dir_all(&bin).expect("create fake bin");
    let brew = bin.join("brew");
    fs::write(
        &brew,
        r#"#!/bin/sh
for arg in "$@"; do
    printf '<%s>\n' "$arg"
done > "$AOS_TEST_ARGS"
exit 23
"#,
    )
    .expect("write fake brew");
    let mut permissions = fs::metadata(&brew).expect("brew metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&brew, permissions).expect("make fake brew executable");

    let output = fixture
        .command()
        .env("UNICITY_AOS_INSTALL_METHOD", "homebrew")
        .env("PATH", &bin)
        .arg("update")
        .output()
        .expect("run Homebrew product update");

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read brew args"),
        "<upgrade>\n<unicity-aos/tap/aos>\n"
    );
}

#[test]
fn native_status_does_not_invoke_the_runtime_cli() {
    let fixture = Fixture::new("status");
    fixture.install_runtime(RECORDING_RUNTIME);

    for args in [vec!["status"], vec!["status", "--json"]] {
        let output = fixture
            .command()
            .args(args)
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
}

#[test]
fn unix_passthrough_preserves_signal_termination() {
    let fixture = Fixture::new("signal");
    let ready = fixture.root.join("ready");
    fixture.install_runtime(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{}'\nexec sleep 30\n",
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
    assert_eq!(
        fs::read_to_string(&ready)
            .expect("read runtime pid")
            .trim()
            .parse::<u32>()
            .expect("parse runtime pid"),
        child.id(),
        "the runtime script must retain the aos process id"
    );

    child.kill().expect("terminate delegated runtime");
    let status = child.wait().expect("wait for delegated runtime");
    assert_eq!(status.signal(), Some(9));
}

fn shell_literal_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}
