//! `aos` — the product command surface for Unicity AOS.
//!
//! Unicity AOS is a distribution built on Astrid Runtime. AOS-owned commands
//! shadow matching runtime roots; every other root passes through unchanged to
//! the bundled runtime under the product-owned home and workspace layout.

use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitStatus;
use std::process::{Command, ExitCode};
use std::time::Duration;

use astrid_core::PrincipalId;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use unicity_aos_bootstrap::{AOS_WORKSPACE_STATE_DIR, AosHome};

mod hook;
mod mcp;

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
    /// Authenticated runtime principal for principal-scoped AOS commands.
    #[arg(
        long,
        value_name = "OPERATOR_PRINCIPAL",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    principal: Option<String>,
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
    Update(UpdateArgs),
    /// Distribution state is fixed to Unicity CE in this AOS release.
    Distro(DistroArgs),
    /// Deliver a host hook through the authenticated AOS event bus.
    Hook(hook::HookArgs),
    /// Expose this AOS installation to an MCP host over stdio.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Serve the loopback-only product health endpoint.
    ServeHealth,
    /// Run the bundled runtime daemon in the foreground.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Run the persistent bundled daemon in the foreground.
    Foreground(ForegroundDaemonArgs),
}

#[derive(Args)]
struct ForegroundDaemonArgs {
    /// Project workspace owned by this daemon.
    #[arg(long, value_name = "PATH")]
    workspace: Option<std::path::PathBuf>,
    /// Enable debug-level daemon logging.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve AOS tools and broker interactions over stdio.
    Serve(mcp::ServeArgs),
}

#[derive(Args)]
struct DistroArgs {
    /// Runtime distribution arguments retained only to provide a safe refusal.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    arguments: Vec<OsString>,
}

#[derive(Args)]
struct StatusArgs {
    /// Authenticated runtime principal for this status request.
    #[arg(
        id = "status-principal",
        long = "principal",
        value_name = "PRINCIPAL",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    principal: Option<String>,
    /// Print a machine-readable JSON status object.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct UpdateArgs {
    /// Follow the signed stable, dev, or nightly product channel.
    #[arg(long, value_enum, conflicts_with = "version")]
    channel: Option<UpdateChannel>,
    /// Install an exact signed AOS calendar-semver release.
    #[arg(long, value_parser = parse_aos_version, conflicts_with = "channel")]
    version: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum UpdateChannel {
    Stable,
    Dev,
    Nightly,
}

impl UpdateChannel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Dev => "dev",
            Self::Nightly => "nightly",
        }
    }
}

fn parse_aos_version(value: &str) -> Result<String, String> {
    let components = value.split('.').collect::<Vec<_>>();
    let canonical = |component: &str| {
        component == "0"
            || (component.as_bytes().first().is_some_and(u8::is_ascii_digit)
                && !component.starts_with('0')
                && component.bytes().all(|byte| byte.is_ascii_digit()))
    };
    if components.len() != 3
        || components[0].len() != 4
        || !components[0].bytes().all(|byte| byte.is_ascii_digit())
        || !canonical(components[1])
        || !canonical(components[2])
    {
        return Err("expected YYYY.MINOR.PATCH without leading zeroes".to_owned());
    }
    let year = components[0]
        .parse::<u16>()
        .map_err(|_| "release year is invalid".to_owned())?;
    if !(2026..=2099).contains(&year) {
        return Err("release year must be between 2026 and 2099".to_owned());
    }
    Ok(value.to_owned())
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
    #[arg(
        long,
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
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
    if runtime_stop_requested(&args) {
        return handle_runtime_stop(&args);
    }
    if product_init_requested(&args)
        && let Err(code) = prepare_product_init(&args)
    {
        return code;
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
    if runtime_stop_requested(&args) {
        return handle_runtime_stop(&args);
    }
    if product_init_requested(&args)
        && let Err(code) = prepare_product_init(&args)
    {
        return code;
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
    if let Some(root) = ambiguous_leading_principal(args) {
        eprintln!(
            "aos: ambiguous '--principal {root}': provide an operator principal before the AOS-owned command, for example `aos --principal operator {root}`"
        );
        return Some(ExitCode::from(2));
    }

    let first = args.first()?.to_str()?;
    let product_invocation = matches!(first, "-h" | "--help" | "-V" | "--version")
        || (first == "help" && help_targets_product(args))
        || is_owned_root(first)
        || leading_owned_root(args).is_some();
    if !product_invocation {
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

    if cli.principal.is_some()
        && !matches!(
            &cli.command,
            Some(
                ProductCommand::Init(_)
                    | ProductCommand::Distro(_)
                    | ProductCommand::Hook(_)
                    | ProductCommand::Mcp { .. }
                    | ProductCommand::Status(_)
            )
        )
    {
        eprintln!(
            "aos: '--principal' is supported for `aos init`, `aos status`, `aos hook`, and `aos mcp`; this AOS-owned command does not accept a runtime principal"
        );
        return Some(ExitCode::from(2));
    }

    match cli.command {
        Some(ProductCommand::Init(_)) => None,
        Some(ProductCommand::Status(args)) => {
            Some(handle_status(cli.principal, args.principal, args.json))
        }
        Some(ProductCommand::Migrate {
            command: MigrateCommand::Runtime { from },
        }) => Some(handle_migrate_runtime(&from)),
        Some(ProductCommand::Update(args)) => Some(handle_self_update(&args)),
        Some(ProductCommand::Distro(args)) => Some(refuse_distro_command(&args.arguments)),
        Some(ProductCommand::Hook(args)) => Some(handle_hook(cli.principal, args)),
        Some(ProductCommand::Mcp {
            command: McpCommand::Serve(args),
        }) => Some(mcp::handle_serve(cli.principal, args)),
        Some(ProductCommand::ServeHealth) => Some(handle_health_service()),
        Some(ProductCommand::Daemon {
            command: DaemonCommand::Foreground(args),
        }) => Some(handle_foreground_daemon(&args)),
        None => Some(print_product_help()),
    }
}

fn handle_foreground_daemon(args: &ForegroundDaemonArgs) -> ExitCode {
    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };
    #[cfg(unix)]
    {
        match home.exec_foreground_daemon(args.workspace.as_deref(), args.verbose) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("aos: failed to run foreground daemon: {error}");
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(unix))]
    {
        match home
            .foreground_daemon_command(args.workspace.as_deref(), args.verbose)
            .and_then(|mut command| command.status())
        {
            Ok(status) => child_exit_code(status),
            Err(error) => {
                eprintln!("aos: failed to run foreground daemon: {error}");
                ExitCode::FAILURE
            }
        }
    }
}

fn product_init_requested(args: &[OsString]) -> bool {
    leading_runtime_root_index(args)
        .ok()
        .flatten()
        .and_then(|index| args.get(index))
        .is_some_and(|root| root == "init")
}

fn prepare_product_init(args: &[OsString]) -> Result<(), ExitCode> {
    let cli = ProductCli::try_parse_from(
        std::iter::once(OsString::from("aos")).chain(args.iter().cloned()),
    )
    .map_err(|error| {
        eprintln!("aos: failed to reconstruct validated CE init arguments: {error}");
        ExitCode::FAILURE
    })?;
    let Some(ProductCommand::Init(init)) = cli.command else {
        eprintln!("aos: internal error: CE init preparation received another command");
        return Err(ExitCode::FAILURE);
    };

    let mut runtime_args = vec![
        OsString::from("--principal"),
        OsString::from("default"),
        OsString::from("init"),
        OsString::from("--target-principal"),
        OsString::from("default"),
    ];
    if init.verbose {
        runtime_args.push(OsString::from("--verbose"));
    }
    if init.yes {
        runtime_args.push(OsString::from("--yes"));
    }
    if init.offline {
        runtime_args.push(OsString::from("--offline"));
    }
    if init.allow_unsigned {
        runtime_args.push(OsString::from("--allow-unsigned"));
    }
    if init.accept_new_key {
        runtime_args.push(OsString::from("--accept-new-key"));
    }
    for value in init.vars {
        runtime_args.push(OsString::from("--var"));
        runtime_args.push(OsString::from(value));
    }

    let home = resolve_home()?;
    home.prepare_unicity_ce_init(runtime_args).map_err(|error| {
        eprintln!("aos: failed to prepare the bundled runtime for CE init: {error}");
        ExitCode::FAILURE
    })
}

fn help_targets_product(args: &[OsString]) -> bool {
    match args.get(1).and_then(|argument| argument.to_str()) {
        None => true,
        Some(root) => is_owned_root(root),
    }
}

fn runtime_args_for_dispatch(mut args: Vec<OsString>) -> Vec<OsString> {
    if leading_runtime_root_index(&args)
        .ok()
        .flatten()
        .and_then(|index| args.get(index))
        .is_some_and(|arg| arg == "init")
    {
        args.push(OsString::from("--grant-capsules"));
    }
    args
}

fn runtime_stop_requested(args: &[OsString]) -> bool {
    match leading_runtime_root_index(args) {
        Ok(Some(index)) => args.get(index).is_some_and(|root| root == "stop"),
        Ok(None) => false,
        Err(()) => fallback_runtime_root(args).is_some_and(|root| root == "stop"),
    }
}

fn fallback_runtime_root(args: &[OsString]) -> Option<&str> {
    args.iter().filter_map(|arg| arg.to_str()).find(|arg| {
        matches!(
            *arg,
            "chat"
                | "run"
                | "agent"
                | "group"
                | "caps"
                | "quota"
                | "invite"
                | "keypair"
                | "pair-device"
                | "secret"
                | "voucher"
                | "trust"
                | "audit"
                | "budget"
                | "session"
                | "capsule"
                | "mcp"
                | "distro"
                | "init"
                | "config"
                | "gc"
                | "start"
                | "status"
                | "stop"
                | "restart"
                | "logs"
                | "ps"
                | "top"
                | "who"
                | "doctor"
                | "setup"
                | "version"
                | "completions"
                | "update"
                | "self-update"
                | "self_update"
                | "help"
        )
    })
}

fn handle_runtime_stop(args: &[OsString]) -> ExitCode {
    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };
    let output = match home
        .runtime_command_with_args(args)
        .and_then(|mut command| command.output())
    {
        Ok(output) => output,
        Err(error) => {
            eprintln!("aos: failed to run bundled runtime stop: {error}");
            return ExitCode::FAILURE;
        }
    };

    if output.status.success() {
        return emit_runtime_output(&output)
            .map_or_else(runtime_output_error, |()| ExitCode::SUCCESS);
    }

    if expected_shutdown_disconnect(&output) && wait_for_confirmed_stop(&home) {
        if let Err(error) = std::io::stdout().write_all(&output.stdout) {
            return runtime_output_error(error);
        }
        if output.stdout.is_empty() {
            println!("Unicity AOS stopped.");
        }
        return ExitCode::SUCCESS;
    }

    match emit_runtime_output(&output) {
        Ok(()) => child_exit_code(output.status),
        Err(error) => runtime_output_error(error),
    }
}

fn expected_shutdown_disconnect(output: &std::process::Output) -> bool {
    if output.status.code() != Some(1) {
        return false;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("connection lost waiting on astrid.v1.response.shutdown.")
        && stderr.contains("connection closed before astrid.v1.response.shutdown.")
}

fn wait_for_confirmed_stop(home: &AosHome) -> bool {
    const ATTEMPTS: usize = 100;
    const INTERVAL: Duration = Duration::from_millis(50);

    for attempt in 0..ATTEMPTS {
        if unicity_aos_bootstrap::status::confirm_stopped(home)
            .is_ok_and(|status| status.state == "stopped")
        {
            return true;
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(INTERVAL);
        }
    }
    false
}

fn emit_runtime_output(output: &std::process::Output) -> io::Result<()> {
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    Ok(())
}

fn runtime_output_error(error: io::Error) -> ExitCode {
    eprintln!("aos: failed to write bundled runtime output: {error}");
    ExitCode::FAILURE
}

fn is_owned_root(value: &str) -> bool {
    matches!(
        value,
        "init"
            | "status"
            | "migrate"
            | "update"
            | "self-update"
            | "self_update"
            | "distro"
            | "hook"
            | "mcp"
            | "daemon"
            | "serve-health"
    )
}

fn ambiguous_leading_principal(args: &[OsString]) -> Option<&str> {
    if args.first()?.to_str()? != "--principal" {
        return None;
    }
    let value = args.get(1)?.to_str().filter(|value| is_owned_root(value))?;
    let later_command = leading_runtime_root_index(args.get(2..).unwrap_or_default())
        .ok()
        .flatten()
        .is_some();
    (!later_command).then_some(value)
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
        if arg.starts_with("-p") && arg.len() > 2 {
            index += 1;
            continue;
        }
        return Err(());
    }
    Ok(None)
}

fn handle_self_update(args: &UpdateArgs) -> ExitCode {
    if std::env::var_os("UNICITY_AOS_INSTALL_METHOD").as_deref() == Some(OsStr::new("homebrew")) {
        if args.version.is_some()
            || matches!(
                args.channel,
                Some(UpdateChannel::Dev | UpdateChannel::Nightly)
            )
        {
            eprintln!("aos: Homebrew installations follow only the signed stable channel");
            return ExitCode::from(2);
        }
        return command_exit_code(
            Command::new("brew")
                .args(["upgrade", "unicity-aos/tap/aos"])
                .status(),
            "run Homebrew upgrade",
        );
    }

    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("aos: resolve product home for update: {error}");
            return ExitCode::FAILURE;
        }
    };
    let installer = home.root().join("libexec/install.sh");
    match std::fs::symlink_metadata(&installer) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            eprintln!(
                "aos: trusted installed updater is not a regular file: {}",
                installer.display()
            );
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!(
                "aos: trusted installed updater is unavailable at {}: {error}",
                installer.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let mut command = Command::new("sh");
    command.arg(installer);
    if let Some(version) = &args.version {
        command.args(["--version", version]);
    } else {
        command.args([
            "--channel",
            args.channel.unwrap_or(UpdateChannel::Stable).as_str(),
        ]);
    }
    command.args(["--yes", "--no-migrate-prompt"]);
    command_exit_code(command.status(), "run the installed signed AOS updater")
}

fn refuse_distro_command(_arguments: &[OsString]) -> ExitCode {
    eprintln!(
        "aos: Unicity CE owns the distribution state for this AOS installation; `aos distro` cannot apply or replace it"
    );
    eprintln!("AOS does not expose raw distribution mutation beneath another command namespace.");
    ExitCode::from(2)
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

fn handle_hook(principal: Option<String>, args: hook::HookArgs) -> ExitCode {
    let home = match resolve_home() {
        Ok(home) => home,
        Err(code) => return code,
    };
    set_runtime_environment(&home);
    let principal = principal.unwrap_or_else(|| "default".to_owned());
    match hook::handle(principal, args) {
        Ok(Some(context)) => {
            print!("{context}");
            if let Err(error) = io::stdout().flush() {
                eprintln!("aos: failed to write hook response: {error}");
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aos: hook delivery failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn status_principal(
    leading_principal: Option<String>,
    trailing_principal: Option<String>,
) -> Result<PrincipalId, String> {
    let principal = match (leading_principal, trailing_principal) {
        (Some(_), Some(_)) => {
            return Err(
                "'--principal' was provided both before and after `status`; provide it once"
                    .to_owned(),
            );
        }
        (Some(principal), None) | (None, Some(principal)) => Some(principal),
        (None, None) => None,
    };
    principal.map_or_else(
        || Ok(PrincipalId::default()),
        |principal| {
            PrincipalId::new(principal)
                .map_err(|error| format!("invalid status principal: {error}"))
        },
    )
}

fn handle_status(
    leading_principal: Option<String>,
    command_principal: Option<String>,
    json: bool,
) -> ExitCode {
    let principal = match status_principal(leading_principal, command_principal) {
        Ok(principal) => principal,
        Err(error) => {
            eprintln!("aos: {error}");
            return ExitCode::from(2);
        }
    };
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
    let status = match runtime.block_on(unicity_aos_bootstrap::status::read_for_principal(
        &home, principal,
    )) {
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
    use clap::Parser;
    use std::ffi::OsString;

    use super::{
        DaemonCommand, ProductCli, ProductCommand, child_exit_code, handle_product_command,
        help_targets_product, is_owned_root, leading_owned_root, runtime_args_for_dispatch,
        runtime_stop_requested, status_principal,
    };

    #[test]
    fn product_cli_parses_owned_init_surface() {
        let cli = ProductCli::try_parse_from([
            "aos",
            "--principal",
            "operator",
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
        assert_eq!(cli.principal.as_deref(), Some("operator"));
        assert_eq!(init.target_principal.as_deref(), Some("alice"));
        assert!(init.verbose);
        assert!(init.yes);
        assert!(init.offline);
        assert!(init.allow_unsigned);
        assert!(init.accept_new_key);
        assert_eq!(init.vars, ["model=gpt-5"]);
    }

    #[test]
    fn product_cli_parses_persistent_foreground_daemon() {
        let cli = ProductCli::try_parse_from([
            "aos",
            "daemon",
            "foreground",
            "--workspace",
            "/workspace",
            "--verbose",
        ])
        .expect("parse foreground daemon");
        let Some(ProductCommand::Daemon {
            command: DaemonCommand::Foreground(args),
        }) = cli.command
        else {
            panic!("expected foreground daemon command");
        };
        assert_eq!(
            args.workspace.as_deref(),
            Some(std::path::Path::new("/workspace"))
        );
        assert!(args.verbose);
    }

    #[test]
    fn product_cli_parses_and_validates_status_principal() {
        let cli = ProductCli::try_parse_from(["aos", "--principal", "alice", "status"])
            .expect("parse principal-scoped product status");
        assert_eq!(cli.principal.as_deref(), Some("alice"));
        let Some(ProductCommand::Status(status)) = cli.command else {
            panic!("expected status");
        };
        assert!(status.principal.is_none());

        let cli = ProductCli::try_parse_from(["aos", "status", "--principal", "bob"])
            .expect("parse status-local principal");
        assert!(cli.principal.is_none());
        let Some(ProductCommand::Status(status)) = cli.command else {
            panic!("expected status");
        };
        assert_eq!(status.principal.as_deref(), Some("bob"));

        assert_eq!(
            status_principal(Some("alice".to_owned()), None)
                .expect("valid explicit principal")
                .as_str(),
            "alice"
        );
        assert_eq!(
            status_principal(None, Some("bob".to_owned()))
                .expect("valid status-local principal")
                .as_str(),
            "bob"
        );
        assert_eq!(
            status_principal(None, None)
                .expect("omitted principal keeps compatibility default")
                .as_str(),
            "default"
        );
        assert!(status_principal(None, Some("not/a/principal".to_owned())).is_err());
        assert!(status_principal(Some("alice".to_owned()), Some("bob".to_owned())).is_err());
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
            runtime_args_for_dispatch(vec![
                OsString::from("--principal"),
                OsString::from("operator"),
                OsString::from("init"),
                OsString::from("--target-principal"),
                OsString::from("alice"),
            ]),
            [
                OsString::from("--principal"),
                OsString::from("operator"),
                OsString::from("init"),
                OsString::from("--target-principal"),
                OsString::from("alice"),
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
    fn runtime_stop_keeps_the_inherited_argument_surface() {
        assert!(runtime_stop_requested(&[OsString::from("stop")]));
        assert!(runtime_stop_requested(&[
            OsString::from("--principal"),
            OsString::from("operator"),
            OsString::from("stop"),
        ]));
        assert!(!runtime_stop_requested(&[
            OsString::from("capsule"),
            OsString::from("stop"),
        ]));
        assert!(runtime_stop_requested(&[
            OsString::from("--future-runtime-global"),
            OsString::from("future-value"),
            OsString::from("stop"),
        ]));
        assert!(!runtime_stop_requested(&[
            OsString::from("--future-runtime-global"),
            OsString::from("future-value"),
            OsString::from("capsule"),
            OsString::from("stop"),
        ]));
    }

    #[test]
    fn help_is_owned_only_for_the_product_root_or_product_commands() {
        assert!(help_targets_product(&[OsString::from("help")]));
        for root in [
            "init",
            "status",
            "migrate",
            "update",
            "distro",
            "daemon",
            "serve-health",
        ] {
            let args = [OsString::from("help"), OsString::from(root)];
            assert!(help_targets_product(&args));
            assert!(handle_product_command(&args).is_some());
        }
        for root in ["doctor", "capsule", "completion"] {
            let args = [OsString::from("help"), OsString::from(root)];
            assert!(!help_targets_product(&args));
            assert!(handle_product_command(&args).is_none());
            assert_eq!(runtime_args_for_dispatch(args.to_vec()), args);
        }
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
            .is_none()
        );
        assert!(
            handle_product_command(&[
                OsString::from("--principal"),
                OsString::from("init"),
                OsString::from("status"),
            ])
            .is_some()
        );
        assert!(
            handle_product_command(&[OsString::from("--principal"), OsString::from("init")])
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
            assert!(matches!(cli.command, Some(ProductCommand::Update(_))));
        }
        let cli =
            ProductCli::try_parse_from(["aos", "status", "--json"]).expect("parse product status");
        let Some(ProductCommand::Status(status)) = cli.command else {
            panic!("expected product status command");
        };
        assert!(status.json);
    }

    #[test]
    fn runtime_command_contract_matches_the_product_router() {
        let contract: toml::Value = include_str!("../../../release/runtime-command-surface.toml")
            .parse()
            .expect("parse runtime command surface");
        let roots = contract["roots"].as_table().expect("root classifications");

        for root in roots["product-owned"]
            .as_array()
            .expect("product-owned roots")
        {
            assert!(is_owned_root(root.as_str().expect("runtime root")));
        }
        for bucket in ["inherited", "hidden-inherited"] {
            for root in roots[bucket].as_array().expect("inherited roots") {
                assert!(!is_owned_root(root.as_str().expect("runtime root")));
            }
        }
        assert_eq!(
            roots["shared"].as_array().expect("shared roots"),
            &[toml::Value::String("help".to_owned())]
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
}
