#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![allow(missing_docs)]

//! Principal-scoped command and workspace adapter for the first AOS Realm.

mod actor;
mod host;

use aos_realm_machine::{
    Machine as Rv64Machine, MachineConfig as Rv64MachineConfig, RV64_SMOKE_PROGRAM,
    RV64_SUPERVISOR_PROGRAM, SliceOutcome,
};
use aos_realm_runtime::{
    CAT_GUEST, ECHO_GUEST, ExecutionReport, GUEST_PIPELINE_GUEST, HostFault, MINI_SHELL_GUEST,
    PWD_GUEST, ProcessConfig, ProcessOutcome, ProcessTreeReport, RealmHost, RealmIoError,
    RealmMachine, RealmMachineStatus, RunLimits, SMOKE_WRITE_GUEST, STDIN_CAT_GUEST, Signal,
    WRITE_FILE_GUEST,
};
use aos_realm_vfs::FsStatus;
use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use host::{
    AstridRealmHost, DEFAULT_CWD, REALM_NAME, ensure_layout, home_status, layout_state,
    validate_cwd,
};
use serde::{Deserialize, Serialize};

const HARD_MAX_FUEL: u64 = 100_000;
const HARD_MAX_LINUX_STEPS: u64 = 20_000_000;
const HARD_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const HARD_MEMORY_BYTES: usize = 64 * 1024;
// The enclosing Astrid component has a 64 MiB linear-memory ceiling. Keep the
// guest at 32 MiB so its RAM, embedded Image, interpreter state, and component
// stack all fit inside that independently enforced outer limit.
const HARD_LINUX_MEMORY_BYTES: usize = 32 * 1024 * 1024;
const LINUX_SLICE_STEPS: u64 = 100_000;
const LINUX_INIT_MARKER: &[u8] = b"AOS LINUX /init";
#[cfg(target_arch = "wasm32")]
const LINUX_COOPERATE_TOPIC: &str = "realm.v1.linux.cooperate";
const AOS_LINUX_IMAGE: &[u8] = include_bytes!("../linux/Image");

#[derive(Default)]
pub struct LinuxRealm;

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RealmProgram {
    SmokeWrite,
    Pwd,
    Echo,
    PipeEcho,
    GuestPipeEcho,
    RealmSh,
    Rv64Smoke,
    Rv64Supervisor,
    LinuxBoot,
    WriteFile,
    Cat,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecArgs {
    /// Signed program selection. Omit to use `command`, or omit both for `pwd`.
    pub program: Option<RealmProgram>,
    /// Exact command name. This is never evaluated by a host shell.
    pub command: Option<String>,
    /// Structured command arguments. The outer adapter never tokenizes strings;
    /// `realm-sh` alone recognizes separate `|` and `>` operator tokens.
    #[serde(default)]
    pub args: Vec<String>,
    /// Guest-visible CWD beneath `/workspace`, `/home/agent`, or `/tmp`.
    pub cwd: Option<String>,
    /// Optional lower fuel ceiling. It can never raise the capsule hard limit.
    pub fuel: Option<u64>,
    /// Optional lower output ceiling. It can never raise the capsule hard limit.
    pub max_output_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ExecResponse {
    realm: &'static str,
    owner_principal: String,
    program: String,
    execution_backend: &'static str,
    argv: Vec<String>,
    cwd: String,
    outcome: &'static str,
    exit_status: Option<i32>,
    fault: Option<String>,
    stdout: String,
    stderr: String,
    fuel_consumed: u64,
    memory_limit_bytes: usize,
    suspensions: u64,
    processes: usize,
    realm_boot_sequence: u64,
    process_ids: Vec<u64>,
    next_process_id: Option<u64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {}

#[derive(Debug, Serialize)]
struct MountStatus {
    guest_path: &'static str,
    source: &'static str,
    mode: &'static str,
    durable: bool,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    realm: &'static str,
    owner_principal: String,
    state: &'static str,
    default_cwd: &'static str,
    home: &'static str,
    home_storage: &'static str,
    home_format: u32,
    home_generation: u64,
    home_files: usize,
    home_manifest: Option<String>,
    mounts: Vec<MountStatus>,
    commands: [&'static str; 11],
    workspace_commit: &'static str,
    host_process: bool,
    actor_state: &'static str,
    realm_boot_sequence: u64,
    commands_completed: u64,
    process_records: usize,
    pipe_objects: usize,
    reserved_pipe_bytes: usize,
    next_process_id: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct ActorSnapshot {
    state: &'static str,
    boot_sequence: u64,
    commands_completed: u64,
    machine: RealmMachineStatus,
}

impl ActorSnapshot {
    fn idle() -> Self {
        Self {
            state: "idle",
            boot_sequence: 0,
            commands_completed: 0,
            machine: RealmMachine::default().status(),
        }
    }

    fn compatibility() -> Self {
        Self {
            state: "compatibility-entrypoint",
            boot_sequence: 0,
            commands_completed: 0,
            machine: RealmMachine::default().status(),
        }
    }
}

#[derive(Debug)]
struct SelectedProgram {
    name: &'static str,
    execution: SelectedExecution,
    argv: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
enum SelectedExecution {
    Single(&'static [u8]),
    EchoPipeline,
    GuestPipeline,
    MiniShell,
    Rv64(&'static [u8]),
    Linux,
}

impl SelectedExecution {
    const fn backend(self) -> &'static str {
        match self {
            Self::Single(_) | Self::EchoPipeline | Self::GuestPipeline | Self::MiniShell => {
                "nested-core-wasm"
            }
            Self::Rv64(_) => "aos-rv64-interpreter",
            Self::Linux => "aos-rv64-linux",
        }
    }

    const fn hard_fuel(self) -> u64 {
        match self {
            Self::Linux => HARD_MAX_LINUX_STEPS,
            _ => HARD_MAX_FUEL,
        }
    }

    const fn memory_bytes(self) -> usize {
        match self {
            Self::Linux => HARD_LINUX_MEMORY_BYTES,
            _ => HARD_MEMORY_BYTES,
        }
    }
}

#[capsule]
impl LinuxRealm {
    /// Own the principal-isolated Realm machines for this capsule boot.
    #[astrid::run]
    fn run(&self) -> Result<(), SysError> {
        actor::run_actor_loop()
    }

    /// Run one signed command in the caller's principal-scoped AOS Realm.
    ///
    /// `/workspace` maps to the invocation's confined Astrid copy-on-write
    /// `cwd://` mount; its changes require an outer Astrid promotion.
    /// `/home/agent` maps to durable principal-owned realm storage. Commands are
    /// nested core WebAssembly modules and cannot invoke a host shell or process.
    #[astrid::tool("linux_realm_exec", mutable)]
    pub fn exec(&self, args: ExecArgs) -> Result<String, SysError> {
        let principal = caller_principal()?;
        ensure_layout()?;
        let cwd = args.cwd.as_deref().unwrap_or(DEFAULT_CWD);
        validate_cwd(cwd)?;
        let response = run_command(args, principal, Box::<AstridRealmHost>::default())?;
        serde_json::to_string(&response).map_err(|error| SysError::ApiError(error.to_string()))
    }

    /// Inspect the initialized realm without exposing physical host paths.
    #[astrid::tool("linux_realm_status")]
    pub fn status(&self, _args: StatusArgs) -> Result<String, SysError> {
        let principal = caller_principal()?;
        let response = status_response(
            principal,
            layout_state()?,
            home_status()?,
            ActorSnapshot::compatibility(),
        );
        serde_json::to_string(&response).map_err(|error| SysError::ApiError(error.to_string()))
    }
}

fn caller_principal() -> Result<String, SysError> {
    astrid_sdk::runtime::caller()?
        .principal
        .filter(|principal| !principal.is_empty())
        .ok_or_else(|| SysError::ApiError("AOS Realm requires a stamped principal".to_string()))
}

fn run_command(
    args: ExecArgs,
    principal: String,
    realm_host: Box<dyn RealmHost>,
) -> Result<ExecResponse, SysError> {
    let mut machine = RealmMachine::default();
    run_command_in_machine(args, principal, realm_host, &mut machine, 0)
}

fn run_command_in_machine(
    args: ExecArgs,
    principal: String,
    realm_host: Box<dyn RealmHost>,
    machine: &mut RealmMachine,
    boot_sequence: u64,
) -> Result<ExecResponse, SysError> {
    let selected = select_program(&args)?;
    let cwd = args.cwd.clone().unwrap_or_else(|| DEFAULT_CWD.to_string());
    let hard_fuel = selected.execution.hard_fuel();
    let limits = RunLimits {
        fuel: args.fuel.unwrap_or(hard_fuel).min(hard_fuel),
        memory_bytes: selected.execution.memory_bytes(),
        output_bytes: args
            .max_output_bytes
            .unwrap_or(HARD_MAX_OUTPUT_BYTES)
            .min(HARD_MAX_OUTPUT_BYTES),
    };
    let (report, mut process_ids) = execute_selected(&selected, &cwd, limits, realm_host, machine)?;
    process_ids.sort_unstable();
    let machine_status = machine.status();
    let (outcome, exit_status, fault) = outcome_fields(&report.outcome);
    Ok(ExecResponse {
        realm: REALM_NAME,
        owner_principal: principal,
        program: selected.name.to_string(),
        execution_backend: selected.execution.backend(),
        argv: selected.argv,
        cwd,
        outcome,
        exit_status,
        fault,
        stdout: String::from_utf8_lossy(&report.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&report.stderr).into_owned(),
        fuel_consumed: report.fuel_consumed,
        memory_limit_bytes: report.memory_limit_bytes,
        suspensions: report.suspensions,
        processes: process_ids.len(),
        realm_boot_sequence: boot_sequence,
        process_ids,
        next_process_id: machine_status.next_process_id.map(|process| process.get()),
    })
}

fn execute_selected(
    selected: &SelectedProgram,
    cwd: &str,
    limits: RunLimits,
    realm_host: Box<dyn RealmHost>,
    machine: &mut RealmMachine,
) -> Result<(ExecutionReport, Vec<u64>), SysError> {
    match selected.execution {
        SelectedExecution::Single(guest) => machine
            .execute_process(
                guest,
                ProcessConfig {
                    argv: selected.argv.clone(),
                    environment: Vec::new(),
                    cwd: cwd.to_string(),
                },
                limits,
                realm_host,
            )
            .map(|report| (report.execution, vec![report.process_id.get()]))
            .map_err(|error| SysError::ApiError(error.to_string())),
        SelectedExecution::EchoPipeline => {
            let pipeline = machine
                .execute_pipeline(
                    ECHO_GUEST,
                    ProcessConfig {
                        argv: selected.argv.clone(),
                        environment: Vec::new(),
                        cwd: cwd.to_string(),
                    },
                    STDIN_CAT_GUEST,
                    ProcessConfig {
                        argv: vec!["stdin-cat".to_string()],
                        environment: Vec::new(),
                        cwd: cwd.to_string(),
                    },
                    limits,
                    4,
                )
                .map_err(|error| SysError::ApiError(error.to_string()))?;
            let process_ids = vec![
                pipeline.producer.process_id.get(),
                pipeline.consumer.process_id.get(),
            ];
            Ok((combine_pipeline(pipeline), process_ids))
        }
        SelectedExecution::GuestPipeline => {
            let tree = machine
                .execute_process_tree(
                    GUEST_PIPELINE_GUEST,
                    ProcessConfig {
                        argv: selected.argv.clone(),
                        environment: Vec::new(),
                        cwd: cwd.to_string(),
                    },
                    limits,
                    realm_host,
                    2,
                )
                .map_err(|error| SysError::ApiError(error.to_string()))?;
            let mut process_ids = vec![tree.root.process_id.get()];
            process_ids.extend(tree.children.iter().map(|child| child.process_id.get()));
            Ok((combine_process_tree(tree), process_ids))
        }
        SelectedExecution::MiniShell => {
            let tree = machine
                .execute_process_tree(
                    MINI_SHELL_GUEST,
                    ProcessConfig {
                        argv: selected.argv.clone(),
                        environment: Vec::new(),
                        cwd: cwd.to_string(),
                    },
                    limits,
                    realm_host,
                    2,
                )
                .map_err(|error| SysError::ApiError(error.to_string()))?;
            let mut process_ids = vec![tree.root.process_id.get()];
            process_ids.extend(tree.children.iter().map(|child| child.process_id.get()));
            Ok((combine_process_tree(tree), process_ids))
        }
        SelectedExecution::Rv64(program) => {
            execute_rv64(program, limits).map(|report| (report, vec![]))
        }
        SelectedExecution::Linux => execute_linux(limits).map(|report| (report, vec![])),
    }
}

fn execute_rv64(program: &[u8], limits: RunLimits) -> Result<ExecutionReport, SysError> {
    let mut machine = Rv64Machine::new(Rv64MachineConfig {
        ram_bytes: HARD_MEMORY_BYTES,
        max_console_bytes: limits.output_bytes,
    })
    .map_err(|error| SysError::ApiError(error.to_string()))?;
    machine
        .load_program(program)
        .map_err(|error| SysError::ApiError(error.to_string()))?;
    let report = machine.run_slice(limits.fuel);
    let outcome = match report.outcome {
        SliceOutcome::Yielded => ProcessOutcome::FuelExhausted,
        SliceOutcome::Halted(status) if status.passed => ProcessOutcome::Exited(0),
        SliceOutcome::Halted(status) => {
            ProcessOutcome::Exited(i32::try_from(status.code).unwrap_or(i32::MAX))
        }
        SliceOutcome::Trapped(trap) => ProcessOutcome::Trapped(trap.to_string()),
    };
    Ok(ExecutionReport {
        outcome,
        stdout: report.console,
        stderr: Vec::new(),
        fuel_consumed: report.total_steps_executed,
        memory_limit_bytes: HARD_MEMORY_BYTES,
        suspensions: 0,
    })
}

fn execute_linux(limits: RunLimits) -> Result<ExecutionReport, SysError> {
    #[cfg(target_arch = "wasm32")]
    {
        let cooperate = ipc::subscribe(LINUX_COOPERATE_TOPIC)?;
        return execute_linux_cooperatively(limits, || {
            // recv(0) is the kernel-recognized run-loop yield primitive. A
            // private never-published topic makes this a scheduling boundary
            // without admitting input or consuming another actor queue.
            let _ = cooperate.recv(0)?;
            Ok(())
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    execute_linux_cooperatively(limits, || Ok(()))
}

fn execute_linux_cooperatively(
    limits: RunLimits,
    mut cooperate: impl FnMut() -> Result<(), SysError>,
) -> Result<ExecutionReport, SysError> {
    let mut machine = Rv64Machine::new(Rv64MachineConfig {
        ram_bytes: limits.memory_bytes,
        max_console_bytes: limits.output_bytes,
    })
    .map_err(|error| SysError::ApiError(error.to_string()))?;
    machine
        .boot_linux(
            AOS_LINUX_IMAGE,
            &[],
            "earlycon=sbi console=hvc0 init=/init panic=-1",
        )
        .map_err(|error| SysError::ApiError(error.to_string()))?;

    let mut stdout = Vec::new();
    let mut fuel_consumed = 0;
    let mut suspensions = 0;
    let outcome = loop {
        let remaining = limits.fuel.saturating_sub(fuel_consumed);
        if remaining == 0 {
            break ProcessOutcome::FuelExhausted;
        }
        let report = machine.run_slice(remaining.min(LINUX_SLICE_STEPS));
        fuel_consumed = report.total_steps_executed;
        let instructions_retired = report.total_instructions_retired;
        stdout.extend_from_slice(&report.console);
        match report.outcome {
            SliceOutcome::Yielded => {
                suspensions += 1;
                cooperate()?;
            }
            SliceOutcome::Halted(status) if !status.passed => {
                break ProcessOutcome::Exited(i32::try_from(status.code).unwrap_or(i32::MAX));
            }
            SliceOutcome::Halted(_) => {
                if stdout
                    .windows(LINUX_INIT_MARKER.len())
                    .any(|bytes| bytes == LINUX_INIT_MARKER)
                {
                    break ProcessOutcome::Exited(0);
                }
                break ProcessOutcome::Trapped(format!(
                    "Linux halted before the AOS /init marker after {instructions_retired} retired instructions"
                ));
            }
            SliceOutcome::Trapped(trap) => break ProcessOutcome::Trapped(trap.to_string()),
        }
    };

    Ok(ExecutionReport {
        outcome,
        stdout,
        stderr: Vec::new(),
        fuel_consumed,
        memory_limit_bytes: limits.memory_bytes,
        suspensions,
    })
}

fn combine_process_tree(tree: ProcessTreeReport) -> ExecutionReport {
    let mut reports = Vec::with_capacity(tree.children.len().saturating_add(1));
    reports.push(tree.root.execution);
    reports.extend(tree.children.into_iter().map(|child| child.execution));
    let outcome = reports
        .iter()
        .find(|report| report.outcome != ProcessOutcome::Exited(0))
        .map(|report| report.outcome.clone())
        .unwrap_or(ProcessOutcome::Exited(0));
    reports.into_iter().fold(
        ExecutionReport {
            outcome,
            stdout: Vec::new(),
            stderr: Vec::new(),
            fuel_consumed: 0,
            memory_limit_bytes: 0,
            suspensions: 0,
        },
        |mut combined, report| {
            combined.stdout.extend_from_slice(&report.stdout);
            combined.stderr.extend_from_slice(&report.stderr);
            combined.fuel_consumed = combined.fuel_consumed.saturating_add(report.fuel_consumed);
            combined.memory_limit_bytes = combined
                .memory_limit_bytes
                .saturating_add(report.memory_limit_bytes);
            combined.suspensions = combined.suspensions.saturating_add(report.suspensions);
            combined
        },
    )
}

fn combine_pipeline(pipeline: aos_realm_runtime::PipelineReport) -> ExecutionReport {
    let outcome = match &pipeline.producer.execution.outcome {
        ProcessOutcome::Exited(0) => pipeline.consumer.execution.outcome.clone(),
        other => other.clone(),
    };
    let mut stderr = pipeline.producer.execution.stderr;
    stderr.extend_from_slice(&pipeline.consumer.execution.stderr);
    ExecutionReport {
        outcome,
        stdout: pipeline.consumer.execution.stdout,
        stderr,
        fuel_consumed: pipeline
            .producer
            .execution
            .fuel_consumed
            .saturating_add(pipeline.consumer.execution.fuel_consumed),
        memory_limit_bytes: pipeline
            .producer
            .execution
            .memory_limit_bytes
            .saturating_add(pipeline.consumer.execution.memory_limit_bytes),
        suspensions: pipeline
            .producer
            .execution
            .suspensions
            .saturating_add(pipeline.consumer.execution.suspensions),
    }
}

fn select_program(args: &ExecArgs) -> Result<SelectedProgram, SysError> {
    if args.program.is_some() && args.command.is_some() {
        return Err(SysError::ApiError(
            "choose either program or command, not both".to_string(),
        ));
    }
    let program = if let Some(program) = args.program {
        program
    } else if let Some(command) = args.command.as_deref() {
        match command {
            "smoke-write" => RealmProgram::SmokeWrite,
            "pwd" => RealmProgram::Pwd,
            "echo" => RealmProgram::Echo,
            "pipe-echo" => RealmProgram::PipeEcho,
            "guest-pipe-echo" => RealmProgram::GuestPipeEcho,
            "realm-sh" => RealmProgram::RealmSh,
            "rv64-smoke" => RealmProgram::Rv64Smoke,
            "rv64-supervisor" => RealmProgram::Rv64Supervisor,
            "linux-boot" => RealmProgram::LinuxBoot,
            "write-file" => RealmProgram::WriteFile,
            "cat" => RealmProgram::Cat,
            _ => {
                return Err(SysError::ApiError(format!(
                    "unsupported realm command `{command}`; supported: pwd, echo, pipe-echo, guest-pipe-echo, realm-sh, rv64-smoke, rv64-supervisor, linux-boot, write-file, cat, smoke-write"
                )));
            }
        }
    } else {
        RealmProgram::Pwd
    };

    let (name, execution, argv) = match program {
        RealmProgram::SmokeWrite => {
            require_arity("smoke-write", &args.args, 0)?;
            (
                "smoke-write",
                SelectedExecution::Single(SMOKE_WRITE_GUEST),
                vec!["smoke-write".to_string()],
            )
        }
        RealmProgram::Pwd => {
            require_arity("pwd", &args.args, 0)?;
            (
                "pwd",
                SelectedExecution::Single(PWD_GUEST),
                vec!["pwd".to_string()],
            )
        }
        RealmProgram::Echo => (
            "echo",
            SelectedExecution::Single(ECHO_GUEST),
            vec!["echo".to_string(), args.args.join(" ")],
        ),
        RealmProgram::PipeEcho => {
            require_arity("pipe-echo", &args.args, 1)?;
            (
                "pipe-echo",
                SelectedExecution::EchoPipeline,
                vec!["echo".to_string(), args.args[0].clone()],
            )
        }
        RealmProgram::GuestPipeEcho => {
            require_arity("guest-pipe-echo", &args.args, 1)?;
            (
                "guest-pipe-echo",
                SelectedExecution::GuestPipeline,
                vec!["guest-pipeline".to_string(), args.args[0].clone()],
            )
        }
        RealmProgram::RealmSh => {
            let mut argv = vec!["realm-sh".to_string()];
            argv.extend(args.args.iter().cloned());
            ("realm-sh", SelectedExecution::MiniShell, argv)
        }
        RealmProgram::Rv64Smoke => {
            require_arity("rv64-smoke", &args.args, 0)?;
            (
                "rv64-smoke",
                SelectedExecution::Rv64(&RV64_SMOKE_PROGRAM),
                vec!["rv64-smoke".to_string()],
            )
        }
        RealmProgram::Rv64Supervisor => {
            require_arity("rv64-supervisor", &args.args, 0)?;
            (
                "rv64-supervisor",
                SelectedExecution::Rv64(&RV64_SUPERVISOR_PROGRAM),
                vec!["rv64-supervisor".to_string()],
            )
        }
        RealmProgram::LinuxBoot => {
            require_arity("linux-boot", &args.args, 0)?;
            (
                "linux-boot",
                SelectedExecution::Linux,
                vec!["linux-boot".to_string()],
            )
        }
        RealmProgram::WriteFile => {
            require_arity("write-file", &args.args, 2)?;
            let mut argv = vec!["write-file".to_string()];
            argv.extend(args.args.iter().cloned());
            (
                "write-file",
                SelectedExecution::Single(WRITE_FILE_GUEST),
                argv,
            )
        }
        RealmProgram::Cat => {
            require_arity("cat", &args.args, 1)?;
            (
                "cat",
                SelectedExecution::Single(CAT_GUEST),
                vec!["cat".to_string(), args.args[0].clone()],
            )
        }
    };
    Ok(SelectedProgram {
        name,
        execution,
        argv,
    })
}

fn require_arity(command: &str, args: &[String], expected: usize) -> Result<(), SysError> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(SysError::ApiError(format!(
            "{command} expects {expected} argument(s), received {}",
            args.len()
        )))
    }
}

fn status_response(
    principal: String,
    state: &'static str,
    home_status: FsStatus,
    actor: ActorSnapshot,
) -> StatusResponse {
    StatusResponse {
        realm: REALM_NAME,
        owner_principal: principal,
        state,
        default_cwd: DEFAULT_CWD,
        home: "/home/agent",
        home_storage: "kv-cas-head+content-addressed-file-blobs",
        home_format: home_status.format,
        home_generation: home_status.generation,
        home_files: home_status.files,
        home_manifest: home_status
            .manifest
            .map(|digest| digest.as_str().to_string()),
        mounts: vec![
            MountStatus {
                guest_path: "/home/agent",
                source: "principal-home",
                mode: "rw",
                durable: true,
            },
            MountStatus {
                guest_path: "/workspace",
                source: "invocation-cwd",
                mode: "rw",
                durable: false,
            },
            MountStatus {
                guest_path: "/tmp",
                source: "principal-tmp",
                mode: "rw",
                durable: false,
            },
        ],
        commands: [
            "pwd",
            "echo",
            "pipe-echo",
            "guest-pipe-echo",
            "realm-sh",
            "rv64-smoke",
            "rv64-supervisor",
            "linux-boot",
            "write-file",
            "cat",
            "smoke-write",
        ],
        workspace_commit: "outer-astrid-promotion-required",
        host_process: false,
        actor_state: actor.state,
        realm_boot_sequence: actor.boot_sequence,
        commands_completed: actor.commands_completed,
        process_records: actor.machine.process_records,
        pipe_objects: actor.machine.pipe_objects,
        reserved_pipe_bytes: actor.machine.reserved_pipe_bytes,
        next_process_id: actor.machine.next_process_id.map(|process| process.get()),
    }
}

fn outcome_fields(outcome: &ProcessOutcome) -> (&'static str, Option<i32>, Option<String>) {
    match outcome {
        ProcessOutcome::Exited(status) => ("exited", Some(*status), None),
        ProcessOutcome::Signaled(signal) => ("signaled", None, Some(signal_name(*signal))),
        ProcessOutcome::FuelExhausted => {
            ("fuel-exhausted", None, Some("fuel exhausted".to_string()))
        }
        ProcessOutcome::HostFault(fault) => ("host-fault", None, Some(host_fault_name(*fault))),
        ProcessOutcome::Trapped(message) => ("trapped", None, Some(message.clone())),
    }
}

fn signal_name(signal: Signal) -> String {
    match signal {
        Signal::Interrupt => "interrupt",
        Signal::Terminate => "terminate",
        Signal::Kill => "kill",
        Signal::Pipe => "pipe",
    }
    .to_string()
}

fn host_fault_name(fault: HostFault) -> String {
    match fault {
        HostFault::MissingMemory => "missing-memory".to_string(),
        HostFault::InvalidPointer => "invalid-pointer".to_string(),
        HostFault::UnknownDescriptor(_) => "unknown-descriptor".to_string(),
        HostFault::OutputLimit => "output-limit".to_string(),
        HostFault::MissingArgument => "missing-argument".to_string(),
        HostFault::BufferTooSmall => "buffer-too-small".to_string(),
        HostFault::InvalidUtf8 => "invalid-utf8".to_string(),
        HostFault::InvalidArgument => "invalid-argument".to_string(),
        HostFault::Io(error) => format!("io-{}", io_error_name(error)),
        HostFault::BrokenPipe => "broken-pipe".to_string(),
    }
}

fn io_error_name(error: RealmIoError) -> &'static str {
    match error {
        RealmIoError::NotFound => "not-found",
        RealmIoError::Denied => "denied",
        RealmIoError::InvalidPath => "invalid-path",
        RealmIoError::IsDirectory => "is-directory",
        RealmIoError::NotDirectory => "not-directory",
        RealmIoError::TooLarge => "too-large",
        RealmIoError::Unsupported => "unsupported",
        RealmIoError::Io => "failure",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestHost;

    impl RealmHost for TestHost {
        fn open(
            &mut self,
            _cwd: &str,
            _path: &str,
            _mode: aos_realm_runtime::OpenMode,
        ) -> Result<Box<dyn aos_realm_runtime::RealmFile>, RealmIoError> {
            Err(RealmIoError::Denied)
        }
    }

    #[test]
    fn command_runs_as_a_nested_guest_with_explicit_cwd() {
        let response = run_command(
            ExecArgs {
                command: Some("pwd".to_string()),
                cwd: Some("/workspace/project".to_string()),
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("realm command succeeds");

        assert_eq!(response.owner_principal, "alice");
        assert_eq!(response.execution_backend, "nested-core-wasm");
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.exit_status, Some(0));
        assert_eq!(response.stdout, "/workspace/project\n");
    }

    #[test]
    fn rv64_probe_runs_real_guest_instructions_through_virtual_uart() {
        let response = run_command(
            ExecArgs {
                command: Some("rv64-smoke".to_string()),
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("RV64 probe succeeds");

        assert_eq!(response.execution_backend, "aos-rv64-interpreter");
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.exit_status, Some(0));
        assert_eq!(response.stdout, "AOS RV64\n");
        assert_eq!(response.fuel_consumed, 23);
        assert_eq!(response.memory_limit_bytes, HARD_MEMORY_BYTES);
        assert_eq!(response.processes, 0);
        assert!(response.process_ids.is_empty());
    }

    #[test]
    fn rv64_supervisor_probe_crosses_privilege_and_delegated_trap_path() {
        let response = run_command(
            ExecArgs {
                command: Some("rv64-supervisor".to_string()),
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("RV64 supervisor probe succeeds");

        assert_eq!(response.program, "rv64-supervisor");
        assert_eq!(response.execution_backend, "aos-rv64-interpreter");
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.exit_status, Some(0));
        assert_eq!(response.stdout, "STR\n");
        assert_eq!(response.fuel_consumed, 31);
        assert_eq!(response.memory_limit_bytes, HARD_MEMORY_BYTES);
        assert_eq!(response.processes, 0);
        assert!(response.process_ids.is_empty());
    }

    #[test]
    fn linux_boot_reaches_aos_init_and_powers_down_inside_the_rv64_machine() {
        let response = run_command(
            ExecArgs {
                command: Some("linux-boot".to_string()),
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("embedded Linux boot succeeds");

        assert_eq!(response.program, "linux-boot");
        assert_eq!(response.execution_backend, "aos-rv64-linux");
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.exit_status, Some(0));
        assert!(response.stdout.contains("Linux version 6.18.39"));
        assert!(
            response
                .stdout
                .contains("Machine model: AOS RV64 virtual machine v0")
        );
        assert!(response.stdout.contains("Run /init as init process"));
        assert!(response.stdout.contains("AOS LINUX /init"));
        assert!(response.stdout.contains("reboot: Power down"));
        assert_eq!(response.memory_limit_bytes, HARD_LINUX_MEMORY_BYTES);
        assert!(response.fuel_consumed < HARD_MAX_LINUX_STEPS);
        assert!(response.suspensions > 100);
        assert_eq!(response.processes, 0);
    }

    #[test]
    fn linux_runner_cooperates_after_every_yielded_slice() {
        let mut cooperative_yields = 0_u64;
        let report = execute_linux_cooperatively(
            RunLimits {
                fuel: LINUX_SLICE_STEPS * 2,
                memory_bytes: HARD_LINUX_MEMORY_BYTES,
                output_bytes: HARD_MAX_OUTPUT_BYTES,
            },
            || {
                cooperative_yields += 1;
                Ok(())
            },
        )
        .expect("bounded Linux execution yields cooperatively");

        assert_eq!(report.outcome, ProcessOutcome::FuelExhausted);
        assert_eq!(report.fuel_consumed, LINUX_SLICE_STEPS * 2);
        assert_eq!(report.suspensions, 2);
        assert_eq!(cooperative_yields, report.suspensions);
    }

    #[test]
    fn rv64_probe_obeys_outer_fuel_and_output_limits() {
        let fuel = run_command(
            ExecArgs {
                command: Some("rv64-smoke".to_string()),
                fuel: Some(2),
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("bounded RV64 probe returns accounting");
        assert_eq!(fuel.outcome, "fuel-exhausted");
        assert_eq!(fuel.fuel_consumed, 2);
        assert!(fuel.stdout.is_empty());

        let output = run_command(
            ExecArgs {
                command: Some("rv64-smoke".to_string()),
                max_output_bytes: Some(0),
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("output-limited RV64 probe returns a trap");
        assert_eq!(output.outcome, "trapped");
        assert_eq!(output.fuel_consumed, 2);
        assert!(output.stdout.is_empty());
        assert!(
            output
                .fault
                .as_deref()
                .is_some_and(|fault| fault.contains("console output"))
        );
    }

    #[test]
    fn caller_can_only_reduce_fuel() {
        let response = run_command(
            ExecArgs {
                program: Some(RealmProgram::SmokeWrite),
                fuel: Some(u64::MAX),
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("realm command succeeds");

        assert!(response.fuel_consumed <= HARD_MAX_FUEL);
    }

    #[test]
    fn command_is_not_a_shell_command_line() {
        let error = select_program(&ExecArgs {
            command: Some("pwd && whoami".to_string()),
            ..ExecArgs::default()
        })
        .expect_err("shell syntax must not be interpreted");

        assert!(error.to_string().contains("unsupported realm command"));
    }

    #[test]
    fn pipe_echo_runs_two_resumable_processes_with_exact_output() {
        let response = run_command(
            ExecArgs {
                command: Some("pipe-echo".to_string()),
                args: vec!["hello through a four-byte pipe".to_string()],
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("pipeline command succeeds");

        assert_eq!(response.processes, 2);
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.stdout, "hello through a four-byte pipe\n");
        assert!(response.suspensions >= 2);
    }

    #[test]
    fn guest_pipe_echo_creates_and_waits_for_its_own_children() {
        let response = run_command(
            ExecArgs {
                command: Some("guest-pipe-echo".to_string()),
                args: vec!["guest-selected process topology".to_string()],
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("guest-created pipeline succeeds");

        assert_eq!(response.processes, 3);
        assert_eq!(response.process_ids, vec![1, 2, 3]);
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.stdout, "guest-selected process topology\n");
        assert!(response.suspensions >= 2);
        assert!(response.fuel_consumed <= HARD_MAX_FUEL);
    }

    #[test]
    fn realm_shell_builds_a_direct_signed_job_from_structured_tokens() {
        let response = run_command(
            ExecArgs {
                command: Some("realm-sh".to_string()),
                args: vec!["echo".to_string(), "hello from the guest shell".to_string()],
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("guest shell command succeeds");

        assert_eq!(
            response.argv,
            ["realm-sh", "echo", "hello from the guest shell"]
        );
        assert_eq!(response.processes, 2);
        assert_eq!(response.process_ids, vec![1, 2]);
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.stdout, "hello from the guest shell\n");
    }

    #[test]
    fn realm_shell_connects_a_foreground_pipeline_inside_the_guest() {
        let response = run_command(
            ExecArgs {
                command: Some("realm-sh".to_string()),
                args: vec![
                    "echo".to_string(),
                    "record based pipeline".to_string(),
                    "|".to_string(),
                    "cat".to_string(),
                ],
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("guest shell pipeline succeeds");

        assert_eq!(response.processes, 3);
        assert_eq!(response.process_ids, vec![1, 2, 3]);
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.stdout, "record based pipeline\n");
        assert!(response.suspensions >= 4);
    }

    #[test]
    fn realm_shell_passes_a_bounded_environment_to_a_signed_child() {
        let response = run_command(
            ExecArgs {
                command: Some("realm-sh".to_string()),
                args: vec!["env".to_string(), "ASTRID_REALM=ready".to_string()],
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("guest shell environment command succeeds");

        assert_eq!(response.processes, 2);
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.stdout, "ASTRID_REALM=ready\n");
    }

    #[test]
    fn realm_shell_rejects_text_command_lines_as_unsupported_grammar() {
        let response = run_command(
            ExecArgs {
                command: Some("realm-sh".to_string()),
                args: vec!["echo hello | cat".to_string()],
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("guest shell returns a process result");

        assert_eq!(response.processes, 1);
        assert_eq!(response.outcome, "exited");
        assert_eq!(response.exit_status, Some(64));
        assert!(response.stdout.is_empty());
    }

    #[test]
    fn pipe_echo_splits_the_callers_total_fuel_and_output_budgets() {
        let response = run_command(
            ExecArgs {
                command: Some("pipe-echo".to_string()),
                args: vec!["output larger than one byte".to_string()],
                fuel: Some(1_000),
                max_output_bytes: Some(1),
                ..ExecArgs::default()
            },
            "alice".to_string(),
            Box::<TestHost>::default(),
        )
        .expect("bounded pipeline returns accounting");

        assert!(response.fuel_consumed <= 1_000);
        assert!(response.stdout.len() + response.stderr.len() <= 1);
        assert_eq!(response.processes, 2);
    }

    #[test]
    fn forged_principal_field_is_not_part_of_the_input_contract() {
        let error =
            serde_json::from_str::<ExecArgs>(r#"{"command":"pwd","principal":"someone-else"}"#)
                .expect_err("unknown principal field must fail");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn status_exposes_guest_mounts_without_physical_paths() {
        let json = serde_json::to_string(&status_response(
            "alice".to_string(),
            "ready",
            FsStatus {
                format: aos_realm_vfs::FORMAT_VERSION,
                generation: 7,
                files: 3,
                manifest: None,
            },
            ActorSnapshot {
                state: "running",
                boot_sequence: 9,
                commands_completed: 4,
                machine: RealmMachine::default().status(),
            },
        ))
        .expect("status serializes");

        assert!(json.contains("/workspace"));
        assert!(json.contains("/home/agent"));
        assert!(json.contains("kv-cas-head+content-addressed-file-blobs"));
        assert!(json.contains("\"home_generation\":7"));
        assert!(json.contains("\"realm_boot_sequence\":9"));
        assert!(json.contains("\"commands_completed\":4"));
        assert!(json.contains("outer-astrid-promotion-required"));
        assert!(!json.contains("/Users/"));
        assert!(!json.contains(".astrid/home"));
    }

    #[test]
    fn actual_capsule_manifest_has_scoped_fs_and_no_host_process_authority() {
        let manifest: toml::Value = include_str!("../Capsule.toml")
            .parse()
            .expect("Capsule.toml parses");
        let capabilities = manifest["capabilities"]
            .as_table()
            .expect("capabilities is a table");

        assert!(!capabilities.contains_key("host_process"));
        assert!(capabilities.contains_key("kv"));
        assert!(capabilities.contains_key("fs_read"));
        assert!(capabilities.contains_key("fs_write"));
    }
}
