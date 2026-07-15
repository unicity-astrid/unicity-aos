//! `aos` — the product command surface for Unicity AOS.
//!
//! Unicity AOS is a distribution built on Astrid Runtime. AOS-owned commands
//! shadow matching runtime roots; every other root passes through unchanged to
//! the bundled runtime under the product-owned home and workspace layout.

use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
#[cfg(any(not(unix), test))]
use std::process::ExitStatus;
use std::process::{Command, ExitCode};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, CommandFactory, Parser, Subcommand};
use unicity_aos_bootstrap::{AOS_WORKSPACE_STATE_DIR, AosHome};

// Product-owned commands are parsed here. Unknown roots bypass this parser and
// are delegated byte-for-byte to the bundled runtime by `main`.
#[derive(Parser)]
#[command(name = "Unicity AOS", bin_name = "aos")]
#[command(version)]
#[command(about = "Unicity Agent Operating System")]
#[command(long_about = None)]
#[command(
    after_help = "All other commands are inherited from the bundled runtime. Running `aos` without a command displays product help until the native AOS chat surface lands."
)]
struct ProductCli {
    #[command(subcommand)]
    command: Option<ProductCommand>,
}

#[derive(Subcommand)]
enum ProductCommand {
    /// Initialize Unicity CE using the manifest bundled with this release.
    Init(InitArgs),
    /// Show product status from the typed local runtime operation.
    Status(StatusArgs),
    /// Import compatible state from a standalone runtime installation.
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Update AOS and its coordinated runtime executable set.
    #[command(name = "update", alias = "self-update", alias = "self_update")]
    Update,
    /// Serve the loopback-only product health endpoint.
    ServeHealth,
}

#[derive(Args)]
struct StatusArgs {
    /// Print a machine-readable JSON status object.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent CLI switches forwarded to the runtime"
)]
struct InitArgs {
    /// Enable verbose runtime output.
    #[arg(short, long)]
    verbose: bool,
    /// Principal whose AOS environment is provisioned.
    #[arg(long)]
    target_principal: Option<String>,
    /// Accept defaults without prompting.
    #[arg(short = 'y', long = "yes")]
    yes: bool,
    /// Forbid network access during initialization.
    #[arg(long)]
    offline: bool,
    /// Permit an unsigned distribution artifact.
    #[arg(long)]
    allow_unsigned: bool,
    /// Accept and pin a changed distribution signing key.
    #[arg(long)]
    accept_new_key: bool,
    /// Supply a distribution variable as KEY=VALUE; repeat as needed.
    #[arg(long = "var", value_name = "KEY=VALUE")]
    vars: Vec<String>,
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Copy compatible state from a standalone runtime home.
    Runtime {
        /// Absolute path to the standalone runtime home.
        #[arg(long, value_name = "ABSOLUTE_LEGACY_HOME")]
        from: std::path::PathBuf,
    },
}

#[cfg(unix)]
fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some(exit_code) = handle_product_command(&args) {
        return exit_code;
    }
    let runtime_args = runtime_args_for_dispatch(args);
    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };
    match home.exec_runtime_with_args(runtime_args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aos: failed to start bundled runtime: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some(exit_code) = handle_product_command(&args) {
        return exit_code;
    }
    let runtime_args = runtime_args_for_dispatch(args);
    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };
    match home.run_runtime_with_args(runtime_args) {
        Ok(status) => child_exit_code(status),
        Err(error) => {
            eprintln!("aos: failed to start bundled runtime: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(any(not(unix), test))]
fn child_exit_code(status: ExitStatus) -> ExitCode {
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(status.code().unwrap_or(1).clamp(1, i32::from(u8::MAX)) as u8)
    }
}

fn resolve_home() -> Result<AosHome, ExitCode> {
    AosHome::resolve().map_err(|error| {
        eprintln!("aos: failed to resolve product home: {error}");
        ExitCode::FAILURE
    })
}

fn handle_product_command(args: &[OsString]) -> Option<ExitCode> {
    if args.is_empty() {
        return offer_first_run_migration().or_else(|| Some(print_product_help()));
    }
    if let Some(root) = leading_owned_root(args) {
        eprintln!(
            "aos: AOS-owned command '{root}' cannot be preceded by runtime-global options; place supported product options after the command or use `astrid {root}` for the raw runtime form"
        );
        return Some(ExitCode::from(2));
    }

    let first = args.first()?.to_str()?;
    if !matches!(
        first,
        "-h" | "--help"
            | "-V"
            | "--version"
            | "help"
            | "init"
            | "status"
            | "migrate"
            | "update"
            | "self-update"
            | "self_update"
            | "serve-health"
    ) {
        return None;
    }

    let cli = match ProductCli::try_parse_from(
        std::iter::once(OsString::from("aos")).chain(args.iter().cloned()),
    ) {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = if error.use_stderr() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
            if let Err(print_error) = error.print() {
                eprintln!("aos: failed to print command help: {print_error}");
                return Some(ExitCode::FAILURE);
            }
            return Some(exit_code);
        }
    };

    match cli.command {
        Some(ProductCommand::Init(_)) => None,
        Some(ProductCommand::Status(args)) => Some(handle_status(args.json)),
        Some(ProductCommand::Migrate {
            command: MigrateCommand::Runtime { from },
        }) => Some(handle_migrate_runtime(&from)),
        Some(ProductCommand::Update) => Some(handle_self_update()),
        Some(ProductCommand::ServeHealth) => Some(handle_health_service()),
        None => Some(print_product_help()),
    }
}

fn runtime_args_for_dispatch(mut args: Vec<OsString>) -> Vec<OsString> {
    if args.first().is_some_and(|arg| arg == "init") {
        args.push(OsString::from("--grant-capsules"));
    }
    args
}

fn is_owned_root(value: &str) -> bool {
    matches!(
        value,
        "init" | "status" | "migrate" | "update" | "self-update" | "self_update" | "serve-health"
    )
}

fn leading_owned_root(args: &[OsString]) -> Option<&str> {
    let first = args.first()?.to_str()?;
    if !first.starts_with('-') || matches!(first, "-h" | "--help" | "-V" | "--version") {
        return None;
    }

    match leading_runtime_root_index(args) {
        Ok(Some(index)) => args
            .get(index)
            .and_then(|arg| arg.to_str())
            .filter(|root| is_owned_root(root)),
        Ok(None) => None,
        Err(()) => args
            .iter()
            .skip(1)
            .filter_map(|arg| arg.to_str())
            .find(|candidate| is_owned_root(candidate)),
    }
}

fn leading_runtime_root_index(args: &[OsString]) -> Result<Option<usize>, ()> {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str().ok_or(())?;
        if !arg.starts_with('-') {
            return Ok(Some(index));
        }
        if arg == "--" {
            return Ok((index + 1 < args.len()).then_some(index + 1));
        }
        if matches!(
            arg,
            "-v" | "--verbose"
                | "-y"
                | "--yes"
                | "--yolo"
                | "--autonomous"
                | "--print-session"
                | "--snapshot-tui"
                | "--emit-path"
        ) {
            index += 1;
            continue;
        }
        if matches!(
            arg,
            "--format"
                | "--principal"
                | "-p"
                | "--prompt"
                | "--session"
                | "--tui-width"
                | "--tui-height"
                | "--workspace-state-dir"
        ) {
            if index + 1 >= args.len() {
                return Err(());
            }
            index += 2;
            continue;
        }
        if [
            "--format=",
            "--principal=",
            "--prompt=",
            "--session=",
            "--tui-width=",
            "--tui-height=",
            "--workspace-state-dir=",
        ]
        .iter()
        .any(|prefix| arg.starts_with(prefix))
        {
            index += 1;
            continue;
        }
        return Err(());
    }
    Ok(None)
}

fn handle_self_update() -> ExitCode {
    if std::env::var_os("UNICITY_AOS_INSTALL_METHOD").as_deref() == Some(OsStr::new("homebrew")) {
        return command_exit_code(
            Command::new("brew")
                .args(["upgrade", "unicity-aos/tap/aos"])
                .status(),
            "run Homebrew upgrade",
        );
    }

    #[cfg(unix)]
    {
        let installer = std::env::temp_dir().join(format!(
            "unicity-aos-update-{}-{}.sh",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let create = create_private_update_file(&installer);
        if let Err(error) = create {
            eprintln!("aos: failed to stage product updater: {error}");
            return ExitCode::FAILURE;
        }

        let url = "https://aos.unicity.ai/install.sh";
        let download = Command::new("curl")
            .args(["--proto", "=https", "--tlsv1.2", "-fsSL", url, "-o"])
            .arg(&installer)
            .status();
        let download_code = command_exit_code(download, "download the product updater");
        if download_code != ExitCode::SUCCESS {
            let _ = std::fs::remove_file(&installer);
            return download_code;
        }

        let mut update = Command::new("sh");
        update
            .arg(&installer)
            .args(["--yes", "--no-migrate-prompt"])
            .env_remove("AOS_VERSION");
        if let Ok(executable) = std::env::current_exe()
            && let Some(bin_dir) = executable.parent()
        {
            update.env("AOS_BIN_DIR", bin_dir);
        }
        let status = update.status();
        let _ = std::fs::remove_file(&installer);
        command_exit_code(status, "run the product updater")
    }

    #[cfg(not(unix))]
    {
        eprintln!(
            "aos: automatic product updates are not available on this platform; install the latest AOS package"
        );
        ExitCode::FAILURE
    }
}

#[cfg(unix)]
fn create_private_update_file(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

fn command_exit_code(status: io::Result<std::process::ExitStatus>, operation: &str) -> ExitCode {
    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(1, i32::from(u8::MAX)) as u8),
        Err(error) => {
            eprintln!("aos: failed to {operation}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn handle_health_service() -> ExitCode {
    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };

    set_runtime_environment(&home);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("aos: failed to start product health runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(unicity_aos_bootstrap::health::serve_default()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aos: health service failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn handle_status(json: bool) -> ExitCode {
    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };
    set_runtime_environment(&home);
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("aos: failed to start status client: {error}");
            return ExitCode::FAILURE;
        }
    };
    let status = match runtime.block_on(unicity_aos_bootstrap::status::read()) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("aos: runtime status unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        match serde_json::to_string(&status) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("aos: failed to encode status: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!("Unicity AOS");
        println!("State: {}", status.state);
        println!("PID: {}", status.pid);
        println!("Uptime: {}s", status.uptime_secs);
        println!("Runtime version: {}", status.runtime_version);
        println!("Connected clients: {}", status.connected_clients);
        println!("Loaded capsules: {}", status.loaded_capsules.len());
    }
    ExitCode::SUCCESS
}

fn set_runtime_environment(home: &AosHome) {
    // Safety: this runs before the current-thread client runtime starts and before this
    // dedicated CLI process creates any other threads.
    unsafe {
        std::env::set_var("ASTRID_HOME", home.runtime_home());
        std::env::set_var("ASTRID_WORKSPACE_STATE_DIR", AOS_WORKSPACE_STATE_DIR);
    }
}
fn offer_first_run_migration() -> Option<ExitCode> {
    if !io::stdin().is_terminal() {
        return None;
    }
    let home = AosHome::resolve().ok()?;
    if home.migration_receipt().is_file() {
        return None;
    }
    let source = AosHome::default_legacy_runtime_home().ok()?;
    if !source.is_dir() {
        return None;
    }

    println!("Found a standalone runtime home at {}.", source.display());
    println!(
        "Unicity can copy compatible runtime state into {}. The existing home will stay unchanged.",
        home.runtime_home().display()
    );
    print!("Import it now? [y/N] ");
    io::stdout().flush().ok()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok()?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        println!(
            "Skipped. You can import later with `aos migrate runtime --from {}`.",
            source.display()
        );
        return Some(ExitCode::SUCCESS);
    }

    match home.migrate_runtime_from(&source) {
        Ok(unicity_aos_bootstrap::MigrationOutcome::Migrated) => {
            println!(
                "Unicity AOS: imported the standalone runtime; the source was left unchanged."
            );
            print_legacy_distro_handoff(&home);
            Some(ExitCode::SUCCESS)
        }
        Ok(unicity_aos_bootstrap::MigrationOutcome::AlreadyMigrated) => Some(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("aos: runtime migration failed: {error}");
            Some(ExitCode::FAILURE)
        }
    }
}

fn handle_migrate_runtime(source: &Path) -> ExitCode {
    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("aos: failed to resolve product home: {error}");
            return ExitCode::FAILURE;
        }
    };
    match home.migrate_runtime_from(source) {
        Ok(unicity_aos_bootstrap::MigrationOutcome::Migrated) => {
            println!(
                "Unicity AOS: imported the standalone runtime; the source was left unchanged."
            );
            print_legacy_distro_handoff(&home);
            ExitCode::SUCCESS
        }
        Ok(unicity_aos_bootstrap::MigrationOutcome::AlreadyMigrated) => {
            println!("Unicity AOS: this runtime migration is already complete.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("aos: runtime migration failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_legacy_distro_handoff(home: &AosHome) {
    let distros = match home.imported_legacy_distros() {
        Ok(distros) => distros,
        Err(error) => {
            eprintln!("aos: migrated runtime, but could not read the migration receipt: {error}");
            return;
        }
    };
    if !distros.is_empty() {
        println!(
            "Imported legacy distro state was preserved. Run `aos init` to deliberately apply Unicity CE; provider configuration and imported state remain in place."
        );
    }
}

fn print_product_help() -> ExitCode {
    if let Err(error) = ProductCli::command().print_help() {
        eprintln!("aos: failed to print command help: {error}");
        return ExitCode::FAILURE;
    }
    println!();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use super::create_private_update_file;
    use clap::Parser;

    use super::{
        ProductCli, ProductCommand, child_exit_code, handle_product_command, leading_owned_root,
        runtime_args_for_dispatch,
    };

    fn temporary_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "unicity-aos-product-init-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn product_cli_parses_owned_init_surface() {
        let cli = ProductCli::try_parse_from([
            "aos",
            "init",
            "--target-principal",
            "alice",
            "--verbose",
            "--yes",
            "--offline",
            "--allow-unsigned",
            "--accept-new-key",
            "--var",
            "model=gpt-5",
        ])
        .expect("parse product init");
        let Some(ProductCommand::Init(init)) = cli.command else {
            panic!("expected product init command");
        };
        assert_eq!(init.target_principal.as_deref(), Some("alice"));
        assert!(init.verbose);
        assert!(init.yes);
        assert!(init.offline);
        assert!(init.allow_unsigned);
        assert!(init.accept_new_key);
        assert_eq!(init.vars, ["model=gpt-5"]);
    }

    #[test]
    fn product_version_preserves_the_installer_contract() {
        let Err(version) = ProductCli::try_parse_from(["aos", "--version"]) else {
            panic!("--version exits through Clap");
        };

        assert_eq!(
            version.to_string(),
            format!("Unicity AOS {}\n", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn product_init_rejects_distro_overrides_before_runtime_dispatch() {
        assert!(
            handle_product_command(&[
                OsString::from("init"),
                OsString::from("--distro"),
                OsString::from("other"),
            ])
            .is_some()
        );
        assert!(
            handle_product_command(&[OsString::from("init"), OsString::from("--distro=other"),])
                .is_some()
        );
        assert!(ProductCli::try_parse_from(["aos", "init", "--grant-capsules"]).is_err());
        assert!(ProductCli::try_parse_from(["aos", "init", "--principal", "alice"]).is_err());
    }

    #[test]
    fn product_init_delegates_capsule_grants_to_the_runtime() {
        assert_eq!(
            runtime_args_for_dispatch(vec![OsString::from("init")]),
            [OsString::from("init"), OsString::from("--grant-capsules")]
        );
        assert_eq!(
            runtime_args_for_dispatch(vec![
                OsString::from("init"),
                OsString::from("--target-principal"),
                OsString::from("alice"),
            ]),
            [
                OsString::from("init"),
                OsString::from("--target-principal"),
                OsString::from("alice"),
                OsString::from("--grant-capsules"),
            ]
        );
        assert_eq!(
            runtime_args_for_dispatch(vec![
                OsString::from("init"),
                OsString::from("--target-principal"),
                OsString::from("default"),
            ]),
            [
                OsString::from("init"),
                OsString::from("--target-principal"),
                OsString::from("default"),
                OsString::from("--grant-capsules"),
            ]
        );
        assert_eq!(
            runtime_args_for_dispatch(vec![OsString::from("doctor")]),
            [OsString::from("doctor")]
        );
    }

    #[test]
    fn unowned_command_is_left_for_runtime_parser() {
        assert!(handle_product_command(&[OsString::from("doctor")]).is_none());
    }

    #[test]
    fn leading_runtime_globals_cannot_bypass_owned_roots() {
        assert_eq!(
            leading_owned_root(&[
                OsString::from("--principal"),
                OsString::from("alice"),
                OsString::from("status"),
            ]),
            Some("status")
        );
        assert!(
            handle_product_command(&[
                OsString::from("--principal"),
                OsString::from("alice"),
                OsString::from("status"),
            ])
            .is_some()
        );
        assert!(
            handle_product_command(&[
                OsString::from("--principal"),
                OsString::from("alice"),
                OsString::from("init"),
            ])
            .is_some()
        );
        assert!(
            handle_product_command(&[
                OsString::from("--principal"),
                OsString::from("init"),
                OsString::from("status"),
            ])
            .is_some()
        );
    }

    #[test]
    fn unknown_runtime_command_with_distro_flag_is_exact_passthrough() {
        assert!(
            handle_product_command(&[
                OsString::from("frobnicate"),
                OsString::from("--distro"),
                OsString::from("other"),
            ])
            .is_none()
        );
        assert!(handle_product_command(&[OsString::from("capsule")]).is_none());
    }

    #[test]
    fn clap_rejects_extra_product_arguments() {
        assert!(ProductCli::try_parse_from(["aos", "self-update", "extra"]).is_err());
        assert!(ProductCli::try_parse_from(["aos", "migrate", "runtime"]).is_err());
    }

    #[test]
    fn update_aliases_and_status_are_product_owned() {
        for command in ["update", "self-update", "self_update"] {
            let cli = ProductCli::try_parse_from(["aos", command]).expect("parse update alias");
            assert!(matches!(cli.command, Some(ProductCommand::Update)));
        }
        let cli =
            ProductCli::try_parse_from(["aos", "status", "--json"]).expect("parse product status");
        let Some(ProductCommand::Status(status)) = cli.command else {
            panic!("expected product status command");
        };
        assert!(status.json);
    }

    #[cfg(unix)]
    #[test]
    fn child_exit_mapping_preserves_codes_and_maps_signals_to_failure() {
        use std::os::unix::process::ExitStatusExt;

        let success = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .status()
            .expect("run successful child");
        let failure = std::process::Command::new("sh")
            .args(["-c", "exit 37"])
            .status()
            .expect("run failed child");

        assert_eq!(child_exit_code(success), std::process::ExitCode::SUCCESS);
        assert_eq!(child_exit_code(failure), std::process::ExitCode::from(37));
        assert_eq!(
            child_exit_code(std::process::ExitStatus::from_raw(9)),
            std::process::ExitCode::FAILURE
        );
    }

    #[cfg(unix)]
    #[test]
    fn product_updater_is_staged_privately() {
        let path = temporary_home();
        let file = create_private_update_file(&path).expect("create private updater");
        drop(file);
        let mode = fs::metadata(&path)
            .expect("read updater metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        fs::remove_file(path).expect("remove updater");
    }
}
