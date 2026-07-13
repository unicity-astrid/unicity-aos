//! `unicity` — the product command surface for Unicity AOS.
//!
//! Unicity AOS is a trusted distribution built on Astrid Runtime. The product
//! binary therefore delegates runtime and operator commands directly to its
//! bundled runtime, scoped to this installation's private `ASTRID_HOME`.

use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use unicity_aos_bootstrap::AosHome;

const UNICITY_CE_DISTRO: &str = "https://raw.githubusercontent.com/unicity-aos/aos-ce/main/distros/community/unicity-ce/Distro.toml";

#[cfg(unix)]
fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some(exit_code) = handle_product_command(&args) {
        return exit_code;
    }
    let args = product_runtime_args(args);

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
    let args = product_runtime_args(args);

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
        Some("init") if has_distro_override(&args[1..]) => {
            eprintln!(
                "unicity init always installs Unicity CE; use `astrid init` for another distro"
            );
            Some(ExitCode::FAILURE)
        }
        Some("init") if has_help_flag(&args[1..]) => {
            print_init_help();
            Some(ExitCode::SUCCESS)
        }
        Some(_) => None,
    }
}

fn product_runtime_args(args: Vec<OsString>) -> Vec<OsString> {
    if args.first().is_some_and(|arg| arg == "init") {
        let mut runtime_args = vec![
            OsString::from("init"),
            OsString::from("--distro"),
            OsString::from(UNICITY_CE_DISTRO),
        ];
        runtime_args.extend(args.into_iter().skip(1));
        runtime_args
    } else {
        args
    }
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
        "Unicity AOS\n\nUsage:\n  unicity init [--yes] [--offline] [--allow-unsigned] [--accept-new-key] [--var KEY=VALUE]\n  unicity migrate runtime --from <absolute-legacy-home>\n  unicity <runtime command> [arguments...]\n\n`unicity init` installs the pinned Unicity CE distribution. Unicity delegates runtime and operator commands to its bundled Astrid Runtime. The runtime state is scoped to ~/.unicity-os/runtime (or UNICITY_AOS_HOME).\n\n`unicity self-update` is intentionally disabled; AOS updates use the product updater."
    );
}

fn print_init_help() {
    println!(
        "Unicity AOS\n\nUsage:\n  unicity init [--yes] [--offline] [--allow-unsigned] [--accept-new-key] [--var KEY=VALUE]\n\nInstalls Unicity CE from the product-pinned distribution manifest. For a different distro, use the Astrid Runtime CLI directly."
    );
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{UNICITY_CE_DISTRO, has_distro_override, product_runtime_args};

    #[test]
    fn product_init_pins_unicity_ce_and_preserves_flags() {
        let args = product_runtime_args(vec![
            OsString::from("init"),
            OsString::from("--yes"),
            OsString::from("--var"),
            OsString::from("model=gpt-5"),
        ]);
        assert_eq!(
            args,
            [
                "init",
                "--distro",
                UNICITY_CE_DISTRO,
                "--yes",
                "--var",
                "model=gpt-5"
            ]
        );
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
}
