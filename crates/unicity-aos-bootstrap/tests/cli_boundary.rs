#![cfg(unix)]

use std::ffi::OsStr;
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
    bootstrap_args: PathBuf,
    home: PathBuf,
    child_path: PathBuf,
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
        let bootstrap_args = root.join("bootstrap-args");
        let home = root.join("runtime-home");
        let child_path = root.join("child-path");
        let fixture = Self {
            root,
            runtime,
            args,
            bootstrap_args,
            home,
            child_path,
        };
        fixture.install_capsules();
        fixture
    }

    fn install_capsules(&self) {
        let distro: toml::Value = include_str!("../../../distros/community/unicity-ce/Distro.toml")
            .parse()
            .expect("parse embedded distro fixture");
        let directory = self
            .home
            .join("releases")
            .join(env!("CARGO_PKG_VERSION"))
            .join("capsules");
        fs::create_dir_all(&directory).expect("create capsule fixture");
        for capsule in distro["capsule"].as_array().expect("capsule entries") {
            let source = capsule["source"].as_str().expect("capsule source");
            let name = Path::new(source).file_name().expect("capsule filename");
            fs::write(directory.join(name), b"fixture capsule").expect("write capsule fixture");
        }
    }

    fn default_capsule_dir(&self) -> PathBuf {
        self.home
            .join("releases")
            .join(env!("CARGO_PKG_VERSION"))
            .join("capsules")
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
            .env("AOS_HOME", &self.home)
            .env("UNICITY_AOS_RUNTIME_BIN", &self.runtime)
            .env("AOS_TEST_ARGS", &self.args)
            .env("AOS_TEST_BOOTSTRAP_ARGS", &self.bootstrap_args)
            .env("AOS_TEST_HOME", self.root.join("child-home"))
            .env("AOS_TEST_WORKSPACE", self.root.join("child-workspace"))
            .env("AOS_TEST_DISTRO", self.root.join("child-distro"))
            .env("AOS_TEST_PATH", &self.child_path);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const RECORDING_RUNTIME: &str = r#"#!/bin/sh
if [ "$1" = "--principal" ] && [ "$2" = "default" ] && [ "$3" = "init" ]; then
    output="$AOS_TEST_BOOTSTRAP_ARGS"
else
    output="$AOS_TEST_ARGS"
fi
for arg in "$@"; do
    printf '<%s>\n' "$arg"
done > "$output"
printf '%s\n' "$ASTRID_HOME" > "$AOS_TEST_HOME"
printf '%s\n' "$ASTRID_WORKSPACE_STATE_DIR" > "$AOS_TEST_WORKSPACE"
printf '%s\n' "$ASTRID_ENFORCED_DISTRO" > "$AOS_TEST_DISTRO"
printf '%s\n' "$PATH" > "$AOS_TEST_PATH"
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
        ".aos\n"
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
    let child_path = fs::read_to_string(&fixture.child_path).expect("read child PATH");
    assert_eq!(
        std::env::split_paths(OsStr::new(child_path.trim())).next(),
        fixture.runtime.parent().map(Path::to_path_buf)
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
fn inherited_stop_succeeds_only_after_the_runtime_is_confirmed_stopped() {
    let fixture = Fixture::new("confirmed-stop");
    fixture.install_runtime(
        r#"#!/bin/sh
for arg in "$@"; do
    echo "<$arg>"
done > "$AOS_TEST_ARGS"
echo 'error: connection lost waiting on astrid.v1.response.shutdown.test: connection lost: connection closed before astrid.v1.response.shutdown.test' >&2
exit 1
"#,
    );

    let ready_marker = fixture.home.join("runtime/run/system.ready");
    fs::create_dir_all(ready_marker.parent().expect("runtime run directory"))
        .expect("create runtime run directory");
    fs::write(&ready_marker, []).expect("create runtime ready marker");
    let marker_to_remove = ready_marker.clone();
    let shutdown = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        fs::remove_file(marker_to_remove).expect("remove runtime ready marker");
    });

    let output = fixture
        .command()
        .args(["--future-runtime-global", "future-value", "stop"])
        .output()
        .expect("run inherited stop");
    shutdown.join().expect("finish runtime shutdown");

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read delegated stop args"),
        "<--future-runtime-global>\n<future-value>\n<stop>\n"
    );
    assert!(!ready_marker.exists());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf8 stop output"),
        "Unicity AOS stopped.\n"
    );
}

#[test]
fn inherited_stop_does_not_mask_other_runtime_failures() {
    let fixture = Fixture::new("failed-stop");
    fixture.install_runtime(
        r#"#!/bin/sh
echo 'invalid stop argument' >&2
exit 2
"#,
    );

    let output = fixture
        .command()
        .args(["stop", "--invalid"])
        .output()
        .expect("run rejected inherited stop");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).expect("utf8 stop error"),
        "invalid stop argument\n"
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
fn inherited_help_dispatches_byte_for_byte_while_product_help_stays_owned() {
    let fixture = Fixture::new("help-inheritance");
    fixture.install_runtime(RECORDING_RUNTIME);

    for args in [
        vec!["help", "doctor"],
        vec!["help", "capsule"],
        vec!["help", "daemon", "start"],
    ] {
        let output = fixture
            .command()
            .args(&args)
            .output()
            .expect("run inherited help");
        assert!(output.status.success());
        let expected = args
            .iter()
            .map(|argument| format!("<{argument}>\n"))
            .collect::<String>();
        assert_eq!(
            fs::read_to_string(&fixture.args).expect("read delegated help"),
            expected
        );
        fs::remove_file(&fixture.args).expect("reset delegated args");
    }

    for args in [vec!["help"], vec!["help", "init"], vec!["help", "status"]] {
        let output = fixture
            .command()
            .args(args)
            .output()
            .expect("run product help");
        assert!(output.status.success());
        assert!(!fixture.args.exists());
    }
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
        fs::read_to_string(&fixture.bootstrap_args).expect("read bootstrap args"),
        "<--principal>\n<default>\n<init>\n<--target-principal>\n<default>\n"
    );
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
fn product_init_stops_before_runtime_dispatch_when_system_fleet_init_fails() {
    let fixture = Fixture::new("init-bootstrap-failure");
    fixture.install_runtime(RECORDING_RUNTIME);

    let output = fixture
        .command()
        .env("AOS_TEST_EXIT", "42")
        .arg("init")
        .output()
        .expect("run product init with a failing bootstrap installer");

    assert!(!output.status.success());
    assert!(fixture.bootstrap_args.exists());
    assert!(!fixture.args.exists());
    assert!(
        String::from_utf8(output.stderr)
            .expect("utf8 stderr")
            .contains("bundled CE system-fleet initializer exited")
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
    assert_eq!(
        fs::read_to_string(&fixture.bootstrap_args).expect("read bootstrap args"),
        "<--principal>\n<default>\n<init>\n<--target-principal>\n<default>\n<--yes>\n<--var>\n<model=gpt-5>\n"
    );
}

#[test]
fn offline_init_keeps_the_runtime_offline_flag_and_uses_only_local_capsules() {
    let fixture = Fixture::new("init-offline");
    fixture.install_runtime(RECORDING_RUNTIME);

    let status = fixture
        .command()
        .args(["init", "--offline"])
        .status()
        .expect("run offline product init");
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read offline args"),
        "<init>\n<--offline>\n<--grant-capsules>\n"
    );
    assert_eq!(
        fs::read_to_string(&fixture.bootstrap_args).expect("read bootstrap args"),
        "<--principal>\n<default>\n<init>\n<--target-principal>\n<default>\n<--offline>\n"
    );
    let manifest_path = fixture.home.join("distributions/unicity-ce/Distro.toml");
    let manifest: toml::Value = fs::read_to_string(manifest_path)
        .expect("read materialized manifest")
        .parse()
        .expect("parse materialized manifest");
    let capsules = manifest["capsule"].as_array().expect("capsule entries");
    assert_eq!(capsules.len(), 18);
    let expected_root = fixture
        .home
        .join("releases")
        .join(env!("CARGO_PKG_VERSION"))
        .join("capsules")
        .canonicalize()
        .expect("canonical capsule root");
    assert!(capsules.iter().all(|capsule| {
        let source = Path::new(capsule["source"].as_str().expect("source"));
        source.is_absolute() && source.parent() == Some(expected_root.as_path())
    }));
}

#[test]
fn package_manager_capsule_override_is_absolute_exact_and_enforced() {
    let fixture = Fixture::new("capsule-override");
    fixture.install_runtime(RECORDING_RUNTIME);
    let custom = fixture.root.join("homebrew/libexec/capsules");
    fs::create_dir_all(custom.parent().expect("custom capsule parent"))
        .expect("create custom capsule parent");
    fs::rename(fixture.default_capsule_dir(), &custom).expect("move capsules to package prefix");

    let output = fixture
        .command()
        .env("UNICITY_AOS_CAPSULE_DIR", &custom)
        .arg("doctor")
        .output()
        .expect("run with package-manager capsule directory");
    assert!(output.status.success());
    let manifest: toml::Value =
        fs::read_to_string(fixture.home.join("distributions/unicity-ce/Distro.toml"))
            .expect("read materialized override manifest")
            .parse()
            .expect("parse materialized override manifest");
    let canonical = custom.canonicalize().expect("canonical custom capsules");
    assert!(
        manifest["capsule"]
            .as_array()
            .expect("capsules")
            .iter()
            .all(
                |capsule| Path::new(capsule["source"].as_str().expect("source")).parent()
                    == Some(canonical.as_path())
            )
    );

    fs::remove_file(&fixture.args).expect("reset delegated args");
    let invalid = fixture
        .command()
        .env("UNICITY_AOS_CAPSULE_DIR", "relative/capsules")
        .arg("doctor")
        .output()
        .expect("run invalid override");
    assert!(!invalid.status.success());
    assert!(!fixture.args.exists());
    assert!(
        String::from_utf8(invalid.stderr)
            .expect("utf8 stderr")
            .contains("UNICITY_AOS_CAPSULE_DIR must be an absolute path")
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
fn direct_update_fails_closed_without_an_installed_trusted_updater() {
    let fixture = Fixture::new("direct-update");
    fixture.install_runtime(RECORDING_RUNTIME);

    for alias in ["update", "self-update", "self_update"] {
        let output = fixture
            .command()
            .env_remove("UNICITY_AOS_INSTALL_METHOD")
            .arg(alias)
            .output()
            .expect("run direct update without installed updater");

        assert!(!output.status.success());
        assert!(!fixture.args.exists());
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(stderr.contains("trusted installed updater is unavailable"));
    }
}

#[test]
fn direct_update_uses_the_installed_signed_updater() {
    let fixture = Fixture::new("direct-update-installed");
    fixture.install_runtime(RECORDING_RUNTIME);
    let libexec = fixture.home.join("libexec");
    fs::create_dir_all(&libexec).expect("create updater directory");
    let installer = libexec.join("install.sh");
    fs::write(
        &installer,
        r#"#!/bin/sh
for arg in "$@"; do
    printf '<%s>\n' "$arg"
done > "$AOS_TEST_ARGS"
exit 23
"#,
    )
    .expect("write installed updater");

    let output = fixture
        .command()
        .env_remove("UNICITY_AOS_INSTALL_METHOD")
        .args(["update", "--channel", "dev"])
        .output()
        .expect("run installed updater");
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read updater args"),
        "<--channel>\n<dev>\n<--yes>\n<--no-migrate-prompt>\n"
    );

    let output = fixture
        .command()
        .args(["update", "--version", "2026.13.0"])
        .output()
        .expect("run exact installed updater");
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(
        fs::read_to_string(&fixture.args).expect("read updater args"),
        "<--version>\n<2026.13.0>\n<--yes>\n<--no-migrate-prompt>\n"
    );

    for args in [
        vec!["update", "--version", "2026.01.0"],
        vec!["update", "--version", "2025.9.0"],
        vec!["update", "--channel", "dev", "--version", "2026.1.0"],
    ] {
        assert!(
            !fixture
                .command()
                .args(args)
                .status()
                .expect("reject update selector")
                .success()
        );
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

    let output = fixture
        .command()
        .env("UNICITY_AOS_INSTALL_METHOD", "homebrew")
        .env("PATH", &bin)
        .args(["update", "--channel", "nightly"])
        .output()
        .expect("reject non-stable Homebrew update");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn native_status_reports_stopped_without_invoking_the_runtime_cli() {
    let fixture = Fixture::new("status");
    fixture.install_runtime(RECORDING_RUNTIME);

    for args in [vec!["status"], vec!["status", "--json"]] {
        let output = fixture
            .command()
            .args(args)
            .output()
            .expect("run aos status");

        assert!(output.status.success());
        assert!(!fixture.args.exists());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
        assert!(stdout.contains("stopped"));
        assert!(stdout.contains("0.10.0"));
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
    for _ in 0..1_000 {
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
