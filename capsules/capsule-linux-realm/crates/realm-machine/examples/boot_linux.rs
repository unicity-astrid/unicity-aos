use aos_realm_machine::{Machine, MachineConfig, SliceOutcome};
use std::{env, fs, io::Write, process::ExitCode};

const RAM_BYTES: usize = 32 * 1024 * 1024;
const CONSOLE_BYTES: usize = 4 * 1024 * 1024;
const SLICE_STEPS: u64 = 100_000;
const DEFAULT_MAX_STEPS: u64 = 250_000_000;
const INIT_MARKER: &[u8] = b"AOS LINUX /init";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let image_path = args
        .next()
        .ok_or_else(|| "usage: boot_linux IMAGE [MAX_STEPS]".to_string())?;
    let max_steps = args
        .next()
        .map(|value| {
            value
                .to_str()
                .ok_or_else(|| "MAX_STEPS must be UTF-8".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("invalid MAX_STEPS: {error}"))
        })
        .transpose()?
        .unwrap_or(DEFAULT_MAX_STEPS);
    if args.next().is_some() {
        return Err("usage: boot_linux IMAGE [MAX_STEPS]".to_string());
    }

    let image =
        fs::read(&image_path).map_err(|error| format!("could not read {image_path:?}: {error}"))?;
    let mut machine = Machine::new(MachineConfig {
        ram_bytes: RAM_BYTES,
        max_console_bytes: CONSOLE_BYTES,
    })
    .map_err(|error| format!("could not admit Linux machine: {error}"))?;
    machine
        .boot_linux(&image, &[], "earlycon=sbi console=hvc0 init=/init panic=-1")
        .map_err(|error| format!("could not admit Linux image: {error}"))?;

    let mut serial = Vec::new();
    let mut total_steps = 0;
    while total_steps < max_steps {
        let remaining = max_steps.saturating_sub(total_steps);
        let report = machine.run_slice(remaining.min(SLICE_STEPS));
        total_steps = report.total_steps_executed;
        std::io::stdout()
            .write_all(&report.console)
            .map_err(|error| format!("could not write serial output: {error}"))?;
        serial.extend_from_slice(&report.console);
        match report.outcome {
            SliceOutcome::Yielded => {}
            SliceOutcome::Halted(status) => {
                if !serial
                    .windows(INIT_MARKER.len())
                    .any(|bytes| bytes == INIT_MARKER)
                {
                    return Err(format!(
                        "Linux halted before /init marker after {} steps (status {status:?})",
                        report.total_steps_executed
                    ));
                }
                if !status.passed {
                    return Err(format!(
                        "Linux /init halted with failure status {status:?} after {} steps",
                        report.total_steps_executed
                    ));
                }
                eprintln!(
                    "AOS Linux boot passed: {} steps, {} retired instructions",
                    report.total_steps_executed, report.total_instructions_retired
                );
                return Ok(());
            }
            SliceOutcome::HostRequest(request) => {
                return Err(format!(
                    "Linux requested unwired 9P host service {} at pc {:#x} after {} steps",
                    request.id.get(),
                    machine.pc(),
                    report.total_steps_executed
                ));
            }
            SliceOutcome::Trapped(trap) => {
                return Err(format!(
                    "Linux crossed the machine boundary at pc {:#x} after {} steps: {trap}",
                    machine.pc(),
                    report.total_steps_executed
                ));
            }
        }
    }

    Err(format!(
        "Linux did not halt within {max_steps} admitted steps; pc={:#x}, privilege={:?}",
        machine.pc(),
        machine.privilege()
    ))
}
