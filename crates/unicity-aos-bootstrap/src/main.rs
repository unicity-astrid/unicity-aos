//! `aos` — the product command surface for Unicity AOS.
//!
//! Unicity AOS is a distribution built on Astrid Runtime. AOS commands override
//! the corresponding runtime roots; every other root passes through unchanged.

use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::path::Path;
#[cfg(any(not(unix), test))]
use std::process::ExitStatus;
use std::process::{Command, ExitCode};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use unicity_aos_bootstrap::AosHome;

fn main() -> ExitCode {
    run()
}

fn run() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match RootCommand::parse(&args) {
        RootCommand::NoArguments => offer_first_run_migration().unwrap_or_else(|| {
            print_help();
            ExitCode::SUCCESS
        }),
        RootCommand::Help => {
            print_help();
            ExitCode::SUCCESS
        }
        RootCommand::Version => {
            println!("Unicity AOS {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        RootCommand::SelfUpdate(args) => handle_self_update(args),
        RootCommand::Migrate(args) => handle_migrate_command(args),
        RootCommand::ServeHealth(args) => handle_health_service(args),
        RootCommand::Status(args) => handle_status(args),
        RootCommand::Init(args) => handle_init(args),
        RootCommand::Passthrough(args) => handle_runtime_passthrough(args),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RootCommand<'a> {
    NoArguments,
    Help,
    Version,
    SelfUpdate(&'a [OsString]),
    Migrate(&'a [OsString]),
    ServeHealth(&'a [OsString]),
    Status(&'a [OsString]),
    Init(&'a [OsString]),
    Passthrough(&'a [OsString]),
}

impl<'a> RootCommand<'a> {
    fn parse(args: &'a [OsString]) -> Self {
        let Some(command) = args.first() else {
            return Self::NoArguments;
        };
        match command.to_str() {
            Some("-h" | "--help") => Self::Help,
            Some("-V" | "--version") => Self::Version,
            Some("self-update" | "self_update") => Self::SelfUpdate(&args[1..]),
            Some("migrate") => Self::Migrate(&args[1..]),
            Some("serve-health") => Self::ServeHealth(&args[1..]),
            Some("status") => Self::Status(&args[1..]),
            Some("init") => Self::Init(&args[1..]),
            _ => Self::Passthrough(args),
        }
    }
}

fn resolve_home() -> Result<AosHome, ExitCode> {
    AosHome::resolve().map_err(|error| {
        eprintln!("aos: failed to resolve product home: {error}");
        ExitCode::FAILURE
    })
}

fn handle_init(args: &[OsString]) -> ExitCode {
    if has_distro_override(args) {
        eprintln!("aos init always installs Unicity CE; use `astrid init` for another distro");
        return ExitCode::FAILURE;
    }
    if has_help_flag(args) {
        print_init_help();
        return ExitCode::SUCCESS;
    }

    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };
    let args = match product_init_runtime_args(&home, args) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("aos: failed to prepare Unicity CE: {error}");
            return ExitCode::FAILURE;
        }
    };
    run_runtime(&home, args)
}

fn handle_runtime_passthrough(args: &[OsString]) -> ExitCode {
    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("aos: failed to resolve product home: {error}");
            return ExitCode::FAILURE;
        }
    };
    run_runtime(&home, args.iter())
}

#[cfg(unix)]
fn run_runtime<I, S>(home: &AosHome, args: I) -> ExitCode
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    match home.exec_runtime_with_args(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aos: failed to start bundled runtime: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn run_runtime<I, S>(home: &AosHome, args: I) -> ExitCode
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    match home.run_runtime_with_args(args) {
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

fn handle_self_update(args: &[OsString]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("Usage: aos self-update");
        return ExitCode::FAILURE;
    }

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

fn handle_health_service(args: &[OsString]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("Usage: aos serve-health");
        return ExitCode::FAILURE;
    }

    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("aos: failed to resolve product home: {error}");
            return ExitCode::FAILURE;
        }
    };

    set_runtime_home(&home);

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

fn handle_status(args: &[OsString]) -> ExitCode {
    if has_help_flag(args) {
        println!("Unicity AOS\n\nUsage:\n  aos status [--json]");
        return ExitCode::SUCCESS;
    }
    if !args.is_empty() && (args.len() != 1 || args[0] != OsStr::new("--json")) {
        eprintln!("Usage: aos status [--json]");
        return ExitCode::FAILURE;
    }

    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };
    set_runtime_home(&home);
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

    if args.first().is_some_and(|arg| arg == "--json") {
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

fn set_runtime_home(home: &AosHome) {
    // The single-threaded client resolves its local socket from this process-only override.
    unsafe {
        std::env::set_var("ASTRID_HOME", home.runtime_home());
    }
}

fn product_init_runtime_args(home: &AosHome, args: &[OsString]) -> io::Result<Vec<OsString>> {
    let mut runtime_args = vec![
        OsString::from("init"),
        OsString::from("--distro"),
        home.ensure_unicity_ce_manifest()?.into_os_string(),
    ];
    runtime_args.extend(args.iter().cloned());
    Ok(runtime_args)
}

fn has_distro_override(args: &[OsString]) -> bool {
    args.iter().any(|arg| {
        arg.as_os_str() == OsStr::new("--distro")
            || arg.to_str().is_some_and(|arg| arg.starts_with("--distro="))
    })
}

fn has_help_flag(args: &[OsString]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.to_str(), Some("-h" | "--help")))
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

fn handle_migrate_command(args: &[OsString]) -> ExitCode {
    let [subcommand, flag, source] = args else {
        eprintln!("Usage: aos migrate runtime --from <absolute-legacy-home>");
        return ExitCode::FAILURE;
    };
    if subcommand.as_os_str() != OsStr::new("runtime") || flag.as_os_str() != OsStr::new("--from") {
        eprintln!("Usage: aos migrate runtime --from <absolute-legacy-home>");
        return ExitCode::FAILURE;
    }

    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("aos: failed to resolve product home: {error}");
            return ExitCode::FAILURE;
        }
    };
    match home.migrate_runtime_from(std::path::Path::new(source)) {
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

fn print_help() {
    println!(
        "Unicity AOS\n\nUsage:\n  aos init [--yes] [--offline] [--allow-unsigned] [--accept-new-key] [--var KEY=VALUE]\n  aos status [--json]\n  aos migrate runtime --from <absolute-legacy-home>\n  aos self-update\n  aos serve-health\n  aos <runtime command> [arguments...]\n\n`aos init` installs the Unicity CE manifest bundled with this product release. `aos status` reads the typed local runtime status operation. `aos self-update` updates AOS and its pinned runtime. `aos serve-health` binds only 127.0.0.1:8765 and exposes GET /v1/runtime/health. Commands not owned by AOS pass through unchanged to the bundled Astrid Runtime. AOS roots intentionally shadow runtime roots; use `astrid <command>` when the raw runtime command is required. Runtime state is scoped to ~/.unicity-os/runtime (or UNICITY_AOS_HOME)."
    );
}

fn print_init_help() {
    println!(
        "Unicity AOS\n\nUsage:\n  aos init [--yes] [--offline] [--allow-unsigned] [--accept-new-key] [--var KEY=VALUE]\n\nInstalls Unicity CE from the manifest bundled with this product release. For a different distro, use `astrid init`."
    );
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
    use super::{RootCommand, child_exit_code, has_distro_override, product_init_runtime_args};
    use unicity_aos_bootstrap::AosHome;

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
    fn product_init_pins_unicity_ce_and_preserves_flags() {
        let root = temporary_home();
        let home = AosHome::from_root(&root);
        let init_args = vec![
            OsString::from("--yes"),
            OsString::from("--var"),
            OsString::from("model=gpt-5"),
        ];
        let args =
            product_init_runtime_args(&home, &init_args).expect("materialize product manifest");
        assert_eq!(
            [&args[0], &args[1], &args[3], &args[4], &args[5]],
            ["init", "--distro", "--yes", "--var", "model=gpt-5"]
        );
        assert_eq!(
            args[2],
            root.join("distributions/unicity-ce/Distro.toml")
                .into_os_string()
        );
        fs::remove_dir_all(root).expect("remove temporary product home");
    }

    #[test]
    fn product_init_rejects_distro_overrides() {
        assert!(has_distro_override(&[
            OsString::from("--distro"),
            OsString::from("other")
        ]));
        assert!(has_distro_override(&[OsString::from("--distro=other")]));
        assert!(!has_distro_override(&[OsString::from("--yes")]));
    }

    #[test]
    fn parser_recognizes_product_roots() {
        assert_eq!(RootCommand::parse(&[]), RootCommand::NoArguments);

        let help = [OsString::from("--help")];
        assert_eq!(RootCommand::parse(&help), RootCommand::Help);
        let version = [OsString::from("--version")];
        assert_eq!(RootCommand::parse(&version), RootCommand::Version);

        let self_update = [OsString::from("self_update"), OsString::from("unexpected")];
        assert_eq!(
            RootCommand::parse(&self_update),
            RootCommand::SelfUpdate(&self_update[1..])
        );
        let migrate = [OsString::from("migrate"), OsString::from("runtime")];
        assert_eq!(
            RootCommand::parse(&migrate),
            RootCommand::Migrate(&migrate[1..])
        );
        let serve_health = [OsString::from("serve-health"), OsString::from("preserved")];
        assert_eq!(
            RootCommand::parse(&serve_health),
            RootCommand::ServeHealth(&serve_health[1..])
        );
        let status = [OsString::from("status"), OsString::from("--json")];
        assert_eq!(
            RootCommand::parse(&status),
            RootCommand::Status(&status[1..])
        );
        let init = [OsString::from("init"), OsString::from("--yes")];
        assert_eq!(RootCommand::parse(&init), RootCommand::Init(&init[1..]));
    }

    #[test]
    fn parser_passes_every_unowned_root_through_unchanged() {
        let inherited = [OsString::from("capsule"), OsString::from("build")];
        assert_eq!(
            RootCommand::parse(&inherited),
            RootCommand::Passthrough(&inherited)
        );
        let runtime = [OsString::from("runtime"), OsString::from("status")];
        assert_eq!(
            RootCommand::parse(&runtime),
            RootCommand::Passthrough(&runtime)
        );
    }

    #[cfg(unix)]
    #[test]
    fn parser_preserves_non_utf8_runtime_roots() {
        use std::os::unix::ffi::OsStringExt;

        let inherited = [OsString::from_vec(vec![0xff, b'x'])];
        assert_eq!(
            RootCommand::parse(&inherited),
            RootCommand::Passthrough(&inherited)
        );
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
