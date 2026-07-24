use aos_realm_machine::{
    CheckpointBinding, CheckpointDigest, MAX_HARTS, Machine, MachineCheckpoint, MachineConfig,
    MachineMetrics, SliceOutcome,
};
use serde::Serialize;
use std::{env, fs, process::ExitCode, time::Instant};

const RAM_BYTES: usize = 1024 * 1024 * 1024;
const CHECKPOINT_HART_COUNT: usize = 2;
const CONSOLE_BYTES: usize = 64 * 1024;
const SLICE_STEPS: u64 = 10_000_000;
const MAX_STEPS: u64 = 2_000_000_000;
const BENCHMARK_WALL_SECONDS: u64 = 1_784_143_900;
const HOME_9P_CHANNEL: u32 = 1;
const SYSTEM_BLOCK_CHANNEL: u32 = 3;
const INIT_MARKER: &[u8] = b"AOS LINUX /init";
const DEFAULT_SAMPLES: u32 = 10;
const DEFAULT_WARMUPS: u32 = 2;

#[derive(Serialize)]
struct Sample {
    schema: &'static str,
    kind: &'static str,
    engine: &'static str,
    scenario: &'static str,
    iteration: u32,
    duration_ns: u64,
    guest_steps: u64,
    guest_instructions_retired: u64,
    ram_bytes: usize,
    hart_count: usize,
    checkpoint_bytes: usize,
    instruction_fetches: u64,
    translations: u64,
    translation_cache_hits: u64,
    translation_cache_misses: u64,
    translation_cache_flushes: u64,
    sv39_walks: u64,
    page_table_entries_read: u64,
    page_table_entries_written: u64,
}

struct Measurement {
    duration_ns: u64,
    guest_steps: u64,
    guest_instructions_retired: u64,
    metrics: MachineMetrics,
}

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
    let image_path = args.next().ok_or_else(usage)?;
    let system_path = args.next().ok_or_else(usage)?;
    let checkpoint_path = args.next().ok_or_else(usage)?;
    let samples = parse_count(args.next(), DEFAULT_SAMPLES, "SAMPLES")?;
    let warmups = parse_count(args.next(), DEFAULT_WARMUPS, "WARMUPS")?;
    let hart_count = parse_count(args.next(), CHECKPOINT_HART_COUNT as u32, "HART_COUNT")? as usize;
    if hart_count > MAX_HARTS {
        return Err(format!(
            "HART_COUNT must not exceed the machine maximum of {MAX_HARTS}"
        ));
    }
    if args.next().is_some() {
        return Err(usage());
    }

    let image = fs::read(&image_path)
        .map_err(|error| format!("could not read Linux image {image_path:?}: {error}"))?;
    let system = fs::read(&system_path)
        .map_err(|error| format!("could not read system image {system_path:?}: {error}"))?;
    let checkpoint = fs::read(&checkpoint_path)
        .map_err(|error| format!("could not read checkpoint {checkpoint_path:?}: {error}"))?;
    let binding = CheckpointBinding::new(
        CheckpointDigest::hash(&image),
        CheckpointDigest::hash(&system),
    );

    // Validate every immutable input before warmup. Timed checkpoint samples
    // still repeat the complete integrity and binding checks used in production.
    if hart_count == CHECKPOINT_HART_COUNT {
        let admitted = MachineCheckpoint::decode(&checkpoint, binding)
            .map_err(|error| format!("checkpoint failed admission: {error}"))?;
        validate_checkpoint(&admitted)?;
    }

    for _ in 0..warmups {
        let _ = cold_to_principal_bind(&image, &system, hart_count)?;
        if hart_count == CHECKPOINT_HART_COUNT {
            let _ = checkpoint_to_bindable(&checkpoint, binding)?;
        }
    }

    for iteration in 0..samples {
        let (init, principal_bind) = cold_to_principal_bind(&image, &system, hart_count)?;
        emit(measured_sample(
            "cold-to-init",
            iteration,
            hart_count,
            checkpoint.len(),
            init,
        ))?;
        emit(measured_sample(
            "cold-to-principal-bind",
            iteration,
            hart_count,
            checkpoint.len(),
            principal_bind,
        ))?;

        if hart_count == CHECKPOINT_HART_COUNT {
            let duration_ns = checkpoint_to_bindable(&checkpoint, binding)?;
            emit(Sample {
                schema: "aos-linux-realm-benchmark/v1",
                kind: "sample",
                engine: "aos-rv64-reference-native",
                scenario: "checkpoint-to-bindable",
                iteration,
                duration_ns,
                guest_steps: 0,
                guest_instructions_retired: 0,
                ram_bytes: RAM_BYTES,
                hart_count,
                checkpoint_bytes: checkpoint.len(),
                instruction_fetches: 0,
                translations: 0,
                translation_cache_hits: 0,
                translation_cache_misses: 0,
                translation_cache_flushes: 0,
                sv39_walks: 0,
                page_table_entries_read: 0,
                page_table_entries_written: 0,
            })?;
        }
    }
    Ok(())
}

fn measured_sample(
    scenario: &'static str,
    iteration: u32,
    hart_count: usize,
    checkpoint_bytes: usize,
    measurement: Measurement,
) -> Sample {
    Sample {
        schema: "aos-linux-realm-benchmark/v1",
        kind: "sample",
        engine: "aos-rv64-reference-native",
        scenario,
        iteration,
        duration_ns: measurement.duration_ns,
        guest_steps: measurement.guest_steps,
        guest_instructions_retired: measurement.guest_instructions_retired,
        ram_bytes: RAM_BYTES,
        hart_count,
        checkpoint_bytes,
        instruction_fetches: measurement.metrics.instruction_fetches,
        translations: measurement.metrics.translations(),
        translation_cache_hits: measurement.metrics.translation_cache_hits,
        translation_cache_misses: measurement.metrics.translation_cache_misses,
        translation_cache_flushes: measurement.metrics.translation_cache_flushes,
        sv39_walks: measurement.metrics.sv39_walks,
        page_table_entries_read: measurement.metrics.page_table_entries_read,
        page_table_entries_written: measurement.metrics.page_table_entries_written,
    }
}

fn usage() -> String {
    "usage: benchmark_linux IMAGE SYSTEM_SQUASHFS CHECKPOINT [SAMPLES] [WARMUPS] [HART_COUNT]"
        .to_string()
}

fn parse_count(value: Option<std::ffi::OsString>, default: u32, name: &str) -> Result<u32, String> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = value
        .to_str()
        .ok_or_else(|| format!("{name} must be UTF-8"))?
        .parse::<u32>()
        .map_err(|error| format!("invalid {name}: {error}"))?;
    if parsed == 0 {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn cold_to_principal_bind(
    image: &[u8],
    system: &[u8],
    hart_count: usize,
) -> Result<(Measurement, Measurement), String> {
    let started = Instant::now();
    let mut machine = Machine::new_with_harts(
        MachineConfig {
            ram_bytes: RAM_BYTES,
            max_console_bytes: CONSOLE_BYTES,
        },
        hart_count,
    )
    .map_err(|error| format!("could not admit Linux machine: {error}"))?;
    let bootargs = format!(
        "earlycon=sbi console=hvc0 init=/init panic=-1 aos.wall_time={BENCHMARK_WALL_SECONDS} aos.system_bytes={}",
        system.len()
    );
    machine
        .boot_linux(image, &[], &bootargs)
        .map_err(|error| format!("could not admit Linux image: {error}"))?;

    let mut console = Vec::new();
    let mut init = None;
    loop {
        let report = machine.run_slice(SLICE_STEPS);
        console.extend_from_slice(&report.console);
        if init.is_none() && contains(&console, INIT_MARKER) {
            init = Some(Measurement {
                duration_ns: elapsed_ns(started)?,
                guest_steps: report.total_steps_executed,
                guest_instructions_retired: report.total_instructions_retired,
                metrics: machine.metrics(),
            });
        }
        match report.outcome {
            SliceOutcome::Yielded if report.total_steps_executed < MAX_STEPS => {}
            SliceOutcome::HostRequest(request) if request.channel == SYSTEM_BLOCK_CHANNEL => {
                let offset = request
                    .message
                    .as_slice()
                    .try_into()
                    .map(u64::from_le_bytes)
                    .map_err(|_| "system block request has an invalid offset".to_string())?;
                let offset = usize::try_from(offset)
                    .map_err(|_| "system block offset is not addressable".to_string())?;
                let end = offset
                    .checked_add(request.max_response_bytes)
                    .filter(|end| *end <= system.len())
                    .ok_or_else(|| "system block request exceeds the image".to_string())?;
                machine
                    .complete_9p_request(request.id, &system[offset..end])
                    .map_err(|error| format!("could not complete system block read: {error}"))?;
            }
            SliceOutcome::HostRequest(request) => {
                if request.channel != HOME_9P_CHANNEL {
                    return Err(format!(
                        "first Linux host request used channel {}, expected {HOME_9P_CHANNEL}",
                        request.channel
                    ));
                }
                let init = init.ok_or_else(|| {
                    "Linux reached principal bind without the /init marker".to_string()
                })?;
                return Ok((
                    init,
                    Measurement {
                        duration_ns: elapsed_ns(started)?,
                        guest_steps: report.total_steps_executed,
                        guest_instructions_retired: report.total_instructions_retired,
                        metrics: machine.metrics(),
                    },
                ));
            }
            outcome => {
                return Err(format!(
                    "Linux reached {outcome:?} before principal bind after {} steps",
                    report.total_steps_executed
                ));
            }
        }
    }
}

fn checkpoint_to_bindable(bytes: &[u8], binding: CheckpointBinding) -> Result<u64, String> {
    let started = Instant::now();
    let checkpoint = MachineCheckpoint::decode(bytes, binding)
        .map_err(|error| format!("checkpoint failed admission: {error}"))?;
    validate_checkpoint(&checkpoint)?;
    let mut machine = checkpoint.into_machine();
    let duration_ns = elapsed_ns(started)?;
    let report = machine.run_slice(1);
    if !matches!(report.outcome, SliceOutcome::HostRequest(_)) || report.steps_executed != 0 {
        return Err("restored checkpoint was not stopped at its principal bind".to_string());
    }
    Ok(duration_ns)
}

fn validate_checkpoint(checkpoint: &MachineCheckpoint) -> Result<(), String> {
    if checkpoint.ram_bytes() != RAM_BYTES {
        return Err(format!(
            "checkpoint RAM is {}, expected {RAM_BYTES}",
            checkpoint.ram_bytes()
        ));
    }
    if checkpoint.hart_count() != CHECKPOINT_HART_COUNT {
        return Err(format!(
            "checkpoint has {} harts, expected {CHECKPOINT_HART_COUNT}",
            checkpoint.hart_count()
        ));
    }
    if checkpoint.pending_host_request().channel != HOME_9P_CHANNEL {
        return Err("checkpoint does not stop at the home provider bind".to_string());
    }
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn elapsed_ns(started: Instant) -> Result<u64, String> {
    u64::try_from(started.elapsed().as_nanos())
        .map_err(|_| "benchmark duration did not fit u64 nanoseconds".to_string())
}

fn emit(sample: Sample) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string(&sample)
            .map_err(|error| format!("could not encode benchmark sample: {error}"))?
    );
    Ok(())
}
