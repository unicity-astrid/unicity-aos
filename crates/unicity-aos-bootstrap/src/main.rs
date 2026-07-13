//! `unicity` — the product command surface for Unicity AOS.
//!
//! Unicity AOS is a trusted distribution built on Astrid Runtime. The product
//! binary therefore delegates runtime and operator commands directly to its
//! bundled runtime, scoped to this installation's private `ASTRID_HOME`.

use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use unicity_aos_bootstrap::AosHome;

#[cfg(unix)]
fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some(exit_code) = handle_product_command(&args) {
        return exit_code;
    }

    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("unicity: failed to resolve product home: {error}");
            return ExitCode::FAILURE;
        }
    };

    match home.exec_runtime_with_args(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("unicity: failed to start bundled runtime: {error}");
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

    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("unicity: failed to resolve product home: {error}");
            return ExitCode::FAILURE;
        }
    };

    match home.run_runtime_with_args(args) {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(1, i32::from(u8::MAX)) as u8),
        Err(error) => {
            eprintln!("unicity: failed to start bundled runtime: {error}");
            ExitCode::FAILURE
        }
    }
}

fn handle_product_command(args: &[OsString]) -> Option<ExitCode> {
    match args.first().and_then(|arg| arg.to_str()) {
        None => offer_first_run_migration().or_else(|| {
            print_help();
            Some(ExitCode::SUCCESS)
        }),
        Some("-h" | "--help") => {
            print_help();
            Some(ExitCode::SUCCESS)
        }
        Some("-V" | "--version") => {
            println!("Unicity AOS {}", env!("CARGO_PKG_VERSION"));
            Some(ExitCode::SUCCESS)
        }
        Some("self-update" | "self_update") => {
            eprintln!(
                "unicity: runtime self-update is disabled; update Unicity AOS through its product updater"
            );
            Some(ExitCode::FAILURE)
        }
        Some("migrate") => Some(handle_migrate_command(&args[1..])),
        Some(_) => None,
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

    println!(
        "Found a standalone Astrid Runtime home at {}.",
        source.display()
    );
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
            "Skipped. You can import later with `unicity migrate runtime --from {}`.",
            source.display()
        );
        return Some(ExitCode::SUCCESS);
    }

    match home.migrate_runtime_from(&source) {
        Ok(unicity_aos_bootstrap::MigrationOutcome::Migrated) => {
            println!(
                "Unicity AOS: imported the standalone runtime; the source was left unchanged."
            );
            Some(ExitCode::SUCCESS)
        }
        Ok(unicity_aos_bootstrap::MigrationOutcome::AlreadyMigrated) => Some(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("unicity: runtime migration failed: {error}");
            Some(ExitCode::FAILURE)
        }
    }
}

fn handle_migrate_command(args: &[OsString]) -> ExitCode {
    let [subcommand, flag, source] = args else {
        eprintln!("Usage: unicity migrate runtime --from <absolute-legacy-home>");
        return ExitCode::FAILURE;
    };
    if subcommand.as_os_str() != OsStr::new("runtime") || flag.as_os_str() != OsStr::new("--from") {
        eprintln!("Usage: unicity migrate runtime --from <absolute-legacy-home>");
        return ExitCode::FAILURE;
    }

    let home = match AosHome::resolve() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("unicity: failed to resolve product home: {error}");
            return ExitCode::FAILURE;
        }
    };
    match home.migrate_runtime_from(std::path::Path::new(source)) {
        Ok(unicity_aos_bootstrap::MigrationOutcome::Migrated) => {
            println!(
                "Unicity AOS: imported the standalone runtime; the source was left unchanged."
            );
            ExitCode::SUCCESS
        }
        Ok(unicity_aos_bootstrap::MigrationOutcome::AlreadyMigrated) => {
            println!("Unicity AOS: this runtime migration is already complete.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("unicity: runtime migration failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "Unicity AOS\n\nUsage:\n  unicity migrate runtime --from <absolute-legacy-home>\n  unicity <runtime command> [arguments...]\n\nUnicity delegates local runtime and operator commands to its bundled Astrid Runtime.\nThe runtime state is scoped to ~/.unicity-os/runtime (or UNICITY_AOS_HOME).\n\n`unicity self-update` is intentionally disabled; AOS updates use the product updater."
    );
}
