//! `unicity` — the product command surface for Unicity AOS.
//!
//! Unicity AOS is a trusted distribution built on Astrid Runtime. The product
//! binary therefore delegates runtime and operator commands directly to its
//! bundled runtime, scoped to this installation's private `ASTRID_HOME`.

use std::ffi::OsString;
use std::process::ExitCode;

use unicity_aos_bootstrap::AosHome;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if matches!(
        args.first().and_then(|arg| arg.to_str()),
        Some("-h" | "--help")
    ) {
        print_help();
        return ExitCode::SUCCESS;
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

fn print_help() {
    println!(
        "Unicity AOS\n\nUsage:\n  unicity <runtime command> [arguments...]\n\nUnicity delegates local runtime and operator commands to its bundled Astrid Runtime.\nThe runtime state is scoped to ~/.unicity-os/runtime (or UNICITY_AOS_HOME)."
    );
}
