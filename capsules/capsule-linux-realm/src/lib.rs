#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![allow(missing_docs)]

//! Principal-scoped command and workspace adapter for the first AOS Realm.

mod actor;
mod backend;
mod host;
mod paths;
mod resources;

use aos_realm_9p::Session as Plan9Session;
use aos_realm_machine::{
    HostRequestFailure, Machine as Rv64Machine, MachineConfig as Rv64MachineConfig,
    RV64_SMOKE_PROGRAM, RV64_SUPERVISOR_PROGRAM, SliceOutcome,
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
use backend::{DEFAULT_LINUX_BACKEND_ID, LinuxMachine, LinuxSliceOutcome};
#[cfg(test)]
use host::NativeTestWorkspace9p;
#[cfg(not(test))]
use host::{AstridHome9p, AstridWorkspace9p};
use host::{
    AstridRealmHost, DEFAULT_CWD, LINUX_HOME_9P_CHANNEL, LINUX_WORKSPACE_9P_CHANNEL, REALM_HOME,
    REALM_NAME, REALM_TMP, ensure_layout, home_status, layout_state, validate_cwd,
};
use paths::{
    MountContext, MountRole, PATH_CONTRACT_VERSION, PathConsumer, ProjectionState,
    ReferenceStability,
};
#[cfg(test)]
use resources::DEFAULT_LINUX_MEMORY_BYTES;
use resources::RealmResources;
use serde::{Deserialize, Serialize};

const HARD_MAX_FUEL: u64 = 100_000;
const HARD_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const HARD_MEMORY_BYTES: usize = 64 * 1024;
const LINUX_SLICE_STEPS: u64 = aos_realm_vcpu_protocol::MAX_SLICE_STEPS;
// `/dev/console` has the normal TTY output transformation enabled, so the
// init protocol's line feeds emerge as CRLF on the SBI debug console.
const LINUX_READY_MARKER: &[u8] = b"AOS READY\r\n";
const LINUX_PROTOCOL_PREFIX: &str = "AOS/1 ";
const LINUX_FRAME_TOKEN_BYTES: usize = 16;
const LINUX_FRAME_TOKEN_HEX_BYTES: usize = LINUX_FRAME_TOKEN_BYTES * 2;
const MAX_LINUX_COMMAND_BYTES: usize = 1024;
const MAX_LINUX_CWD_BYTES: usize = 64;
const LINUX_DEFAULT_CWD: &str = "/home/agent";
#[cfg(target_arch = "wasm32")]
const LINUX_COOPERATE_TOPIC: &str = "realm.v1.linux.cooperate";

#[cfg(not(test))]
type WorkspacePlan9Session = Plan9Session<AstridWorkspace9p>;
#[cfg(test)]
type WorkspacePlan9Session = Plan9Session<NativeTestWorkspace9p>;

#[cfg(not(test))]
type HomePlan9Session = Plan9Session<AstridHome9p>;
#[cfg(test)]
type HomePlan9Session = Plan9Session<NativeTestWorkspace9p>;

fn new_workspace_9p_session() -> Result<WorkspacePlan9Session, SysError> {
    #[cfg(not(test))]
    let filesystem = AstridWorkspace9p;
    #[cfg(test)]
    let filesystem = NativeTestWorkspace9p::new()?;
    Plan9Session::new(filesystem, "workspace")
        .map_err(|error| SysError::ApiError(error.to_string()))
}

fn new_home_9p_session() -> Result<HomePlan9Session, SysError> {
    #[cfg(not(test))]
    let filesystem = AstridHome9p;
    #[cfg(test)]
    let filesystem = NativeTestWorkspace9p::new()?;
    Plan9Session::new(filesystem, "home").map_err(|error| SysError::ApiError(error.to_string()))
}

/// One principal-owned Realm service.
///
/// The SDK singleton lives in the component's Wasmtime Store. Astrid keeps
/// that Store affined to one verified principal; the inner state repeats the
/// owner check before every operation.
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
    LinuxConsole,
    LinuxSh,
    LinuxShutdown,
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
    /// The Linux shell accepts its durable principal `/home/agent` or the
    /// invocation-scoped Astrid `/workspace`; other Realm programs use
    /// `/workspace` by default.
    pub cwd: Option<String>,
    /// Optional lower fuel ceiling. It can never raise the capsule hard limit.
    pub fuel: Option<u64>,
    /// Optional lower output ceiling. It can never raise the capsule hard limit.
    pub max_output_bytes: Option<usize>,
}

/// Agent-facing shell request that can execute only inside the Linux Realm.
///
/// This deliberately does not expose a host executable or host-process option.
/// The command is delivered to Bash in the principal-affine RV64 Linux guest;
/// the host only drives the metered virtual machine and its confined mounts.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RealmShellArgs {
    /// Exact script to execute inside the Linux Realm.
    pub command: String,
    /// Guest-visible CWD beneath `/home/agent` or `/workspace`.
    pub cwd: Option<String>,
    /// Optional lower guest-step ceiling. It cannot raise the principal limit.
    pub max_steps: Option<u64>,
    /// Optional lower output ceiling. It cannot raise the principal limit.
    pub max_output_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ExecResponse {
    realm: &'static str,
    owner_principal: String,
    program: String,
    execution_backend: &'static str,
    argv: Vec<String>,
    requested_cwd: Option<String>,
    cwd: String,
    path_context: MountContext,
    home_generation_before: u64,
    home_generation_after: u64,
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
    resources: RealmResources,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {}

const CLI_RUN_COMMAND: &str = "realm";
const CLI_RESULT_TOPIC_PREFIX: &str = "cli.v1.command.result.";
const MAX_CLI_REQUEST_ID_BYTES: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CliRunRequest {
    req_id: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug)]
enum CliAction {
    Status,
    Exec(ExecArgs),
}

#[derive(Debug)]
struct CliRunOutput {
    output: String,
    exit_code: u8,
}

#[derive(Debug, Serialize)]
struct MountStatus {
    role: MountRole,
    guest_path: &'static str,
    source: &'static str,
    declared_resource_root: &'static str,
    display_name: &'static str,
    mode: &'static str,
    durable: bool,
    reference_stability: ReferenceStability,
    nested_wasm_projection: ProjectionState,
    linux_projection: ProjectionState,
}

#[derive(Debug, Serialize)]
struct ConsumerCwdStatus {
    consumer: PathConsumer,
    cwd: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    realm: &'static str,
    owner_principal: String,
    state: &'static str,
    /// Kept as the nested-WASM default for compatibility. New consumers should
    /// select from `cwd_defaults` by execution consumer.
    default_cwd: &'static str,
    cwd_defaults: [ConsumerCwdStatus; 3],
    home: &'static str,
    home_storage: &'static str,
    home_format: u32,
    home_generation: u64,
    home_files: usize,
    home_manifest: Option<String>,
    mounts: Vec<MountStatus>,
    path_contract_version: u32,
    physical_host_paths_visible: bool,
    commands: [&'static str; 14],
    workspace_commit: &'static str,
    host_process: bool,
    actor_state: &'static str,
    realm_boot_sequence: u64,
    commands_completed: u64,
    process_records: usize,
    pipe_objects: usize,
    reserved_pipe_bytes: usize,
    next_process_id: Option<u64>,
    linux_lifecycle: &'static str,
    linux_backend: &'static str,
    linux_cold_start: &'static str,
    linux_state: &'static str,
    linux_residency: &'static str,
    linux_vcpus: u32,
    linux_ram_bytes: Option<usize>,
    linux_ram_persistent: bool,
    linux_ram_durability: &'static str,
    linux_storage_persistent: bool,
    linux_rootfs_persistent: bool,
    linux_home_persistent: bool,
    linux_boot_executions_this_actor_boot: u64,
    linux_commands_completed_this_actor_boot: u64,
    linux_clean_shutdowns: u64,
    linux_guest_steps_this_actor_boot: u64,
    linux_last_outcome: Option<&'static str>,
    linux_last_exit_status: Option<i32>,
    linux_guest_accounting_scope: &'static str,
    linux_outer_wasm_metering: &'static str,
    component_residency: &'static str,
    component_state_durability: &'static str,
    resources: ResourceStatus,
}

#[derive(Debug, Serialize)]
struct ResourceStatus {
    configured: RealmResources,
    active: Option<RealmResources>,
    effective: Option<RealmResources>,
    reconfigure_on_next_exec: bool,
    configuration_scope: &'static str,
    outer_enforcement: &'static str,
    allocation_behavior: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct ActorSnapshot {
    state: &'static str,
    boot_sequence: u64,
    commands_completed: u64,
    machine: RealmMachineStatus,
    linux: LinuxSnapshot,
    resources: Option<RealmResources>,
}

#[derive(Clone, Copy, Debug)]
struct LinuxSnapshot {
    state: &'static str,
    boot_executions: u64,
    clean_shutdowns: u64,
    guest_steps: u64,
    last_outcome: Option<&'static str>,
    last_exit_status: Option<i32>,
    commands_completed: u64,
    ram_bytes: Option<usize>,
    vcpus: Option<u32>,
}

impl LinuxSnapshot {
    const fn cold() -> Self {
        Self {
            state: "cold",
            boot_executions: 0,
            clean_shutdowns: 0,
            guest_steps: 0,
            last_outcome: None,
            last_exit_status: None,
            commands_completed: 0,
            ram_bytes: None,
            vcpus: None,
        }
    }
}

impl ActorSnapshot {
    fn idle() -> Self {
        Self {
            state: "idle",
            boot_sequence: 0,
            commands_completed: 0,
            machine: RealmMachine::default().status(),
            linux: LinuxSnapshot::cold(),
            resources: None,
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
    Linux(LinuxAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinuxAction {
    Boot,
    Console,
    Shell,
    Shutdown,
}

impl SelectedExecution {
    const fn backend(self) -> &'static str {
        match self {
            Self::Single(_) | Self::EchoPipeline | Self::GuestPipeline | Self::MiniShell => {
                "nested-core-wasm"
            }
            Self::Rv64(_) => "aos-rv64-interpreter",
            Self::Linux(_) => "aos-rv64-linux",
        }
    }

    const fn hard_fuel(self, resources: RealmResources) -> u64 {
        match self {
            Self::Linux(_) => resources.effective_max_steps(),
            _ => HARD_MAX_FUEL,
        }
    }

    const fn memory_bytes(self, resources: RealmResources) -> usize {
        match self {
            Self::Linux(_) => resources.linux_memory_bytes,
            _ => HARD_MEMORY_BYTES,
        }
    }

    const fn output_bytes(self, resources: RealmResources) -> usize {
        match self {
            Self::Linux(_) => resources.linux_max_output_bytes,
            _ => HARD_MAX_OUTPUT_BYTES,
        }
    }

    const fn path_consumer(self) -> PathConsumer {
        match self {
            Self::Single(_) | Self::EchoPipeline | Self::GuestPipeline | Self::MiniShell => {
                PathConsumer::NestedCoreWasm
            }
            Self::Rv64(_) => PathConsumer::BareRv64,
            Self::Linux(_) => PathConsumer::LinuxGuest,
        }
    }
}

#[capsule]
impl LinuxRealm {
    /// Run one signed command in the caller's principal-scoped AOS Realm.
    ///
    /// Nested core-WASM programs can use the confined `/workspace`, durable
    /// `/home/agent`, and ephemeral `/tmp` Astrid projections. Linux receives
    /// the same durable principal home over one 9P channel and the current
    /// invocation's Astrid `/workspace` over another. The response describes
    /// the selected consumer, projection state, and home generation boundary.
    #[astrid::tool("linux_realm_exec", mutable)]
    pub fn exec(&self, args: ExecArgs) -> Result<String, SysError> {
        let principal = caller_principal()?;
        actor::execute_resident(&principal, args, RealmResources::load()?)
    }

    /// Run a foreground shell command inside the caller's Linux Realm.
    ///
    /// This is the normal shell surface for agents. It never invokes a host
    /// shell and the capsule declares no `host_process` capability. Files built
    /// in `/workspace` therefore remain data on the host and execute only in the
    /// guest unless a separate, explicitly privileged boundary promotes them.
    #[astrid::tool("realm_shell", mutable)]
    pub fn shell(&self, args: RealmShellArgs) -> Result<String, SysError> {
        let principal = caller_principal()?;
        actor::execute_resident(
            &principal,
            realm_shell_exec_args(args),
            RealmResources::load()?,
        )
    }

    /// Discover the per-consumer CWD and mount projections without exposing
    /// physical host paths. Call this before selecting a path for a Realm job.
    #[astrid::tool("linux_realm_status")]
    pub fn status(&self, args: StatusArgs) -> Result<String, SysError> {
        let principal = caller_principal()?;
        actor::status_resident(&principal, args, RealmResources::load()?)
    }

    /// Serve the provider-scoped `astrid capsule ... realm` command without
    /// requiring an MCP broker. The CLI remains an authenticated Astrid
    /// uplink; this is not a host shell escape hatch.
    #[astrid::interceptor("cli_run_linux_realm")]
    fn cli_run(&self, request: CliRunRequest) -> Result<(), SysError> {
        if !is_valid_cli_request_id(&request.req_id) {
            log::warn("linux-realm: rejected malformed CLI request id");
            return Ok(());
        }

        let req_id = request.req_id.clone();
        let topic = format!("{CLI_RESULT_TOPIC_PREFIX}{req_id}");
        let result = run_cli_request(request);
        let payload = match result {
            Ok(result) => serde_json::json!({
                "req_id": req_id,
                "exit_code": result.exit_code,
                "output": ensure_trailing_newline(result.output),
            }),
            Err(error) => serde_json::json!({
                "req_id": req_id,
                "exit_code": 1,
                "output": "",
                "error": error.to_string(),
            }),
        };
        ipc::publish_json(&topic, &payload)
    }
}

fn run_cli_request(request: CliRunRequest) -> Result<CliRunOutput, SysError> {
    if request.command != CLI_RUN_COMMAND {
        return Err(SysError::ApiError(format!(
            "unsupported CLI command `{}`; expected `{CLI_RUN_COMMAND}`",
            request.command
        )));
    }
    let action = parse_cli_action(&request.args).map_err(SysError::ApiError)?;
    let principal = caller_principal()?;
    let (output, exit_code) = match action {
        CliAction::Status => (
            actor::status_resident(&principal, StatusArgs::default(), RealmResources::load()?)?,
            0,
        ),
        CliAction::Exec(args) => {
            let output = actor::execute_resident(&principal, args, RealmResources::load()?)?;
            let parsed = parse_realm_json(&output)?;
            let exit_code = cli_exit_code(&parsed);
            return Ok(CliRunOutput {
                output: pretty_json_value(&parsed)?,
                exit_code,
            });
        }
    };
    Ok(CliRunOutput {
        output: pretty_json(&output)?,
        exit_code,
    })
}

fn parse_cli_action(args: &[String]) -> Result<CliAction, String> {
    let Some(action) = args.first().map(String::as_str) else {
        return Err(cli_usage());
    };
    match action {
        "status" if args.len() == 1 => Ok(CliAction::Status),
        "boot" if args.len() == 1 => Ok(CliAction::Exec(cli_exec("linux-boot", vec![], None))),
        "shutdown" if args.len() == 1 => {
            Ok(CliAction::Exec(cli_exec("linux-shutdown", vec![], None)))
        }
        "sh" => {
            let (cwd, operands) = parse_cli_cwd(&args[1..])?;
            if operands.len() != 1 {
                return Err(format!(
                    "realm sh requires exactly one quoted script\n{}",
                    cli_usage()
                ));
            }
            Ok(CliAction::Exec(cli_exec(
                "linux-sh",
                vec![operands[0].clone()],
                cwd,
            )))
        }
        "exec" => {
            let (cwd, operands) = parse_cli_cwd(&args[1..])?;
            let Some((command, command_args)) = operands.split_first() else {
                return Err(format!("realm exec requires a command\n{}", cli_usage()));
            };
            Ok(CliAction::Exec(cli_exec(
                command,
                command_args.to_vec(),
                cwd,
            )))
        }
        "help" | "--help" | "-h" => Err(cli_usage()),
        _ => Err(format!(
            "unknown or malformed realm action `{action}`\n{}",
            cli_usage()
        )),
    }
}

fn parse_cli_cwd(args: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    if args.first().map(String::as_str) != Some("--cwd") {
        return Ok((None, args.to_vec()));
    }
    let Some(cwd) = args.get(1) else {
        return Err("--cwd requires an absolute guest path".to_string());
    };
    Ok((Some(cwd.clone()), args[2..].to_vec()))
}

fn cli_exec(command: &str, args: Vec<String>, cwd: Option<String>) -> ExecArgs {
    ExecArgs {
        program: None,
        command: Some(command.to_string()),
        args,
        cwd,
        fuel: None,
        max_output_bytes: None,
    }
}

fn realm_shell_exec_args(args: RealmShellArgs) -> ExecArgs {
    ExecArgs {
        program: Some(RealmProgram::LinuxSh),
        command: None,
        args: vec![args.command],
        cwd: args.cwd,
        fuel: args.max_steps,
        max_output_bytes: args.max_output_bytes,
    }
}

fn cli_usage() -> String {
    "usage: realm status | boot | sh [--cwd PATH] '<script>' | shutdown | exec [--cwd PATH] <command> [args...]".to_string()
}

fn is_valid_cli_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CLI_REQUEST_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f') || byte == b'-')
}

fn pretty_json(value: &str) -> Result<String, SysError> {
    pretty_json_value(&parse_realm_json(value)?)
}

fn parse_realm_json(value: &str) -> Result<serde_json::Value, SysError> {
    serde_json::from_str(value)
        .map_err(|error| SysError::ApiError(format!("Realm returned malformed JSON: {error}")))
}

fn pretty_json_value(value: &serde_json::Value) -> Result<String, SysError> {
    serde_json::to_string_pretty(value)
        .map_err(|error| SysError::ApiError(format!("failed to render Realm result: {error}")))
}

fn cli_exit_code(response: &serde_json::Value) -> u8 {
    let outcome = response
        .get("outcome")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let status = response
        .get("exit_status")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| u8::try_from(value).ok());
    if matches!(outcome, "ready" | "completed" | "stopped" | "exited") {
        status.unwrap_or(0)
    } else {
        status.filter(|status| *status != 0).unwrap_or(1)
    }
}

fn ensure_trailing_newline(mut value: String) -> String {
    if !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

fn caller_principal() -> Result<String, SysError> {
    astrid_sdk::runtime::caller()?
        .principal
        .filter(|principal| !principal.is_empty())
        .ok_or_else(|| SysError::ApiError("AOS Realm requires a stamped principal".to_string()))
}

#[cfg(test)]
fn run_command(
    args: ExecArgs,
    principal: String,
    realm_host: Box<dyn RealmHost>,
) -> Result<ExecResponse, SysError> {
    let mut machine = RealmMachine::default();
    run_command_in_machine(args, principal, realm_host, &mut machine, 0, 0)
}

fn run_command_in_machine(
    args: ExecArgs,
    principal: String,
    realm_host: Box<dyn RealmHost>,
    machine: &mut RealmMachine,
    boot_sequence: u64,
    home_generation: u64,
) -> Result<ExecResponse, SysError> {
    let selected = select_program(&args)?;
    let requested_cwd = args.cwd.clone();
    let cwd = args.cwd.clone().unwrap_or_else(|| DEFAULT_CWD.to_string());
    let resources = RealmResources::default();
    let hard_fuel = selected.execution.hard_fuel(resources);
    let output_ceiling = selected.execution.output_bytes(resources);
    let limits = RunLimits {
        fuel: args.fuel.unwrap_or(hard_fuel).min(hard_fuel),
        memory_bytes: selected.execution.memory_bytes(resources),
        output_bytes: args
            .max_output_bytes
            .unwrap_or(output_ceiling)
            .min(output_ceiling),
    };
    let (report, mut process_ids) = execute_selected(&selected, &cwd, limits, realm_host, machine)?;
    process_ids.sort_unstable();
    let machine_status = machine.status();
    let (outcome, exit_status, fault) = outcome_fields(&report.outcome);
    let path_context = MountContext::for_execution(
        selected.execution.path_consumer(),
        &cwd,
        Some(home_generation),
        boot_sequence,
    )
    .map_err(|error| {
        SysError::ApiError(format!(
            "failed to describe Realm path context: {}",
            io_error_name(error)
        ))
    })?;
    Ok(ExecResponse {
        realm: REALM_NAME,
        owner_principal: principal,
        program: selected.name.to_string(),
        execution_backend: selected.execution.backend(),
        argv: selected.argv,
        requested_cwd,
        cwd,
        path_context,
        home_generation_before: home_generation,
        home_generation_after: home_generation,
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
        resources,
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
        SelectedExecution::Linux(_) => Err(SysError::ApiError(
            "Linux commands require a principal-affine resident Realm".to_string(),
        )),
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
        SliceOutcome::HostRequest(request) => ProcessOutcome::Trapped(format!(
            "bare RV64 execution unexpectedly requested 9P host service {}",
            request.id.get()
        )),
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

#[derive(Debug)]
struct LinuxInvocationReport {
    backend: &'static str,
    outcome: &'static str,
    exit_status: Option<i32>,
    fault: Option<String>,
    stdout: Vec<u8>,
    fuel_consumed: u64,
    suspensions: u64,
    booted: bool,
    command_completed: bool,
    clean_shutdown: bool,
}

#[derive(Debug)]
enum LinuxDriveOutcome {
    Ready,
    Command(i32),
    Halted(i32),
    FuelExhausted,
    OutputLimit,
    Trapped(String),
}

#[derive(Debug)]
struct LinuxDriveReport {
    outcome: LinuxDriveOutcome,
    stdout: Vec<u8>,
    fuel_consumed: u64,
    suspensions: u64,
}

fn execute_linux_resident(
    machine: &mut Option<LinuxMachine>,
    home_9p: &mut Option<HomePlan9Session>,
    workspace_9p: &mut Option<WorkspacePlan9Session>,
    action: LinuxAction,
    command: Option<&str>,
    cwd: &str,
    limits: LinuxInvocationLimits,
) -> Result<LinuxInvocationReport, SysError> {
    let frame_token =
        if action == LinuxAction::Boot || (action == LinuxAction::Shutdown && machine.is_none()) {
            None
        } else {
            Some(linux_frame_token()?)
        };
    #[cfg(target_arch = "wasm32")]
    {
        let cooperate = ipc::subscribe(LINUX_COOPERATE_TOPIC)?;
        return execute_linux_resident_cooperatively(
            LinuxResidentState::new(machine, home_9p, workspace_9p),
            action,
            command,
            cwd,
            frame_token.as_deref(),
            limits,
            || {
                // recv(0) is the kernel-recognized run-loop yield primitive. A
                // private never-published topic makes this a scheduling boundary
                // without admitting input or consuming another service queue.
                let _ = cooperate.recv(0)?;
                Ok(())
            },
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    execute_linux_resident_cooperatively(
        LinuxResidentState::new(machine, home_9p, workspace_9p),
        action,
        command,
        cwd,
        frame_token.as_deref(),
        limits,
        || Ok(()),
    )
}

struct LinuxInvocationLimits {
    run: RunLimits,
    max_file_bytes: u64,
    max_processes: u32,
    max_open_files: u32,
    vcpus: u32,
}

impl From<RunLimits> for LinuxInvocationLimits {
    fn from(run: RunLimits) -> Self {
        Self {
            run,
            max_file_bytes: resources::DEFAULT_LINUX_MAX_FILE_BYTES,
            max_processes: resources::DEFAULT_LINUX_MAX_PROCESSES,
            max_open_files: resources::DEFAULT_LINUX_MAX_OPEN_FILES,
            // Direct reference tests use one logical guest CPU. Principal-
            // configured execution passes its explicit or admitted auto value.
            vcpus: 1,
        }
    }
}

struct LinuxResidentState<'a> {
    machine: &'a mut Option<LinuxMachine>,
    home_9p: &'a mut Option<HomePlan9Session>,
    workspace_9p: &'a mut Option<WorkspacePlan9Session>,
}

struct LinuxPlan9State<'a> {
    home: &'a mut Option<HomePlan9Session>,
    workspace: &'a mut Option<WorkspacePlan9Session>,
}

impl<'a> LinuxResidentState<'a> {
    fn new(
        machine: &'a mut Option<LinuxMachine>,
        home_9p: &'a mut Option<HomePlan9Session>,
        workspace_9p: &'a mut Option<WorkspacePlan9Session>,
    ) -> Self {
        Self {
            machine,
            home_9p,
            workspace_9p,
        }
    }
}

fn execute_linux_resident_cooperatively(
    state: LinuxResidentState<'_>,
    action: LinuxAction,
    command: Option<&str>,
    cwd: &str,
    frame_token: Option<&str>,
    limits: impl Into<LinuxInvocationLimits>,
    mut cooperate: impl FnMut() -> Result<(), SysError>,
) -> Result<LinuxInvocationReport, SysError> {
    let LinuxInvocationLimits {
        run: limits,
        max_file_bytes,
        max_processes,
        max_open_files,
        vcpus,
    } = limits.into();
    let LinuxResidentState {
        machine,
        home_9p,
        workspace_9p,
    } = state;
    let backend = machine
        .as_ref()
        .map_or(DEFAULT_LINUX_BACKEND_ID, LinuxMachine::backend_id);
    if matches!(action, LinuxAction::Console | LinuxAction::Shell) && command.is_none() {
        return Err(SysError::ApiError(
            "Linux execution requires a validated command".to_string(),
        ));
    }
    if action == LinuxAction::Shutdown && machine.is_none() {
        *workspace_9p = None;
        return Ok(LinuxInvocationReport {
            backend,
            outcome: "stopped",
            exit_status: Some(0),
            fault: None,
            stdout: Vec::new(),
            fuel_consumed: 0,
            suspensions: 0,
            booted: false,
            command_completed: false,
            clean_shutdown: false,
        });
    }

    let mut stdout = Vec::new();
    let mut fuel_consumed = 0_u64;
    let mut suspensions = 0_u64;
    let booted = machine.is_none();

    if booted {
        *workspace_9p = None;
        let resident = LinuxMachine::new(
            Rv64MachineConfig {
                ram_bytes: limits.memory_bytes,
                // Console chunks are drained after every scheduling slice. The
                // invocation-wide output ceiling is enforced below.
                max_console_bytes: HARD_MAX_OUTPUT_BYTES,
            },
            vcpus,
        )
        .map_err(SysError::ApiError)?;
        *machine = Some(resident);

        let report = match drive_linux_until(
            machine.as_mut().expect("machine inserted"),
            LinuxPlan9State {
                home: home_9p,
                workspace: workspace_9p,
            },
            limits.fuel,
            limits.output_bytes,
            LinuxAction::Boot,
            None,
            &mut cooperate,
        ) {
            Ok(report) => report,
            Err(error) => {
                *machine = None;
                *workspace_9p = None;
                return Err(error);
            }
        };
        fuel_consumed = fuel_consumed.saturating_add(report.fuel_consumed);
        suspensions = suspensions.saturating_add(report.suspensions);
        stdout.extend_from_slice(&report.stdout);
        if !matches!(report.outcome, LinuxDriveOutcome::Ready) {
            *machine = None;
            *workspace_9p = None;
            return Ok(linux_failure_report(
                backend,
                report.outcome,
                stdout,
                fuel_consumed,
                suspensions,
                true,
            ));
        }
    }

    if action == LinuxAction::Boot {
        return Ok(LinuxInvocationReport {
            backend,
            outcome: "ready",
            exit_status: None,
            fault: None,
            stdout,
            fuel_consumed,
            suspensions,
            booted,
            command_completed: false,
            clean_shutdown: false,
        });
    }

    let command = match action {
        LinuxAction::Console => command.expect("validated before boot"),
        LinuxAction::Shell => command.expect("validated before boot"),
        LinuxAction::Shutdown => "shutdown",
        LinuxAction::Boot => unreachable!("handled above"),
    };
    let frame_token = frame_token.ok_or_else(|| {
        SysError::ApiError("Linux command frame is missing its correlation token".to_string())
    })?;
    validate_linux_frame_token(frame_token)?;
    let framed_command = if action == LinuxAction::Shell {
        linux_shell_command_frame(max_file_bytes, max_processes, max_open_files, cwd, command)
    } else {
        command.to_string()
    };
    let input = format!("{LINUX_PROTOCOL_PREFIX}{frame_token} {framed_command}\n");
    machine
        .as_mut()
        .expect("booted or pre-existing machine")
        .push_console_input(input.as_bytes())
        .map_err(SysError::ApiError)?;

    let remaining_fuel = limits.fuel.saturating_sub(fuel_consumed);
    let remaining_output = limits.output_bytes.saturating_sub(stdout.len());
    let report = match drive_linux_until(
        machine.as_mut().expect("machine remains resident"),
        LinuxPlan9State {
            home: home_9p,
            workspace: workspace_9p,
        },
        remaining_fuel,
        remaining_output,
        action,
        Some(frame_token),
        &mut cooperate,
    ) {
        Ok(report) => report,
        Err(error) => {
            *machine = None;
            *workspace_9p = None;
            return Err(error);
        }
    };
    fuel_consumed = fuel_consumed.saturating_add(report.fuel_consumed);
    suspensions = suspensions.saturating_add(report.suspensions);
    stdout.extend_from_slice(&report.stdout);

    match report.outcome {
        LinuxDriveOutcome::Command(status) => Ok(LinuxInvocationReport {
            backend,
            outcome: "completed",
            exit_status: Some(status),
            fault: None,
            stdout,
            fuel_consumed,
            suspensions,
            booted,
            command_completed: true,
            clean_shutdown: false,
        }),
        LinuxDriveOutcome::Halted(status) if action == LinuxAction::Shutdown && status == 0 => {
            *machine = None;
            *workspace_9p = None;
            Ok(LinuxInvocationReport {
                backend,
                outcome: "stopped",
                exit_status: Some(0),
                fault: None,
                stdout,
                fuel_consumed,
                suspensions,
                booted,
                command_completed: true,
                clean_shutdown: true,
            })
        }
        outcome => {
            *machine = None;
            *workspace_9p = None;
            Ok(linux_failure_report(
                backend,
                outcome,
                stdout,
                fuel_consumed,
                suspensions,
                booted,
            ))
        }
    }
}

fn linux_shell_command_frame(
    max_file_bytes: u64,
    max_processes: u32,
    max_open_files: u32,
    cwd: &str,
    command: &str,
) -> String {
    if max_processes == 0 && max_open_files == 0 {
        // Preserve the original private frame for existing pinned images. New
        // PID 1 generations accept both forms; an explicit process ceiling is
        // the unambiguous opt-in to the extended frame below.
        format!("sh {max_file_bytes} {} {cwd} {command}", cwd.len())
    } else if max_open_files == 0 {
        format!(
            "sh {max_file_bytes} {max_processes} {} {cwd} {command}",
            cwd.len()
        )
    } else {
        format!(
            "sh {max_file_bytes} {max_processes} {max_open_files} {} {cwd} {command}",
            cwd.len()
        )
    }
}

fn drive_linux_until(
    machine: &mut LinuxMachine,
    plan9: LinuxPlan9State<'_>,
    fuel: u64,
    output_bytes: usize,
    target: LinuxAction,
    frame_token: Option<&str>,
    cooperate: &mut impl FnMut() -> Result<(), SysError>,
) -> Result<LinuxDriveReport, SysError> {
    let mut stdout = Vec::new();
    let mut fuel_consumed = 0_u64;
    let mut suspensions = 0_u64;
    loop {
        let remaining = fuel.saturating_sub(fuel_consumed);
        if remaining == 0 {
            return Ok(LinuxDriveReport {
                outcome: LinuxDriveOutcome::FuelExhausted,
                stdout,
                fuel_consumed,
                suspensions,
            });
        }
        let report = machine
            .run_slice(remaining.min(LINUX_SLICE_STEPS))
            .map_err(SysError::ApiError)?;
        fuel_consumed = fuel_consumed.saturating_add(report.steps_executed);
        if stdout.len().saturating_add(report.console.len()) > output_bytes {
            return Ok(LinuxDriveReport {
                outcome: LinuxDriveOutcome::OutputLimit,
                stdout,
                fuel_consumed,
                suspensions,
            });
        }
        stdout.extend_from_slice(&report.console);

        if matches!(report.outcome, LinuxSliceOutcome::Yielded) {
            suspensions = suspensions.saturating_add(1);
            cooperate()?;
        }

        let marker_outcome = match target {
            LinuxAction::Boot if contains_bytes(&stdout, LINUX_READY_MARKER) => {
                Some(LinuxDriveOutcome::Ready)
            }
            LinuxAction::Console | LinuxAction::Shell => frame_token
                .and_then(|token| linux_command_status(&stdout, token))
                .map(LinuxDriveOutcome::Command),
            _ => None,
        };
        if let Some(outcome) = marker_outcome {
            return Ok(LinuxDriveReport {
                outcome,
                stdout,
                fuel_consumed,
                suspensions,
            });
        }

        match report.outcome {
            LinuxSliceOutcome::Yielded => {}
            LinuxSliceOutcome::Halted { passed, code } => {
                let status = if passed {
                    0
                } else {
                    i32::try_from(code).unwrap_or(i32::MAX)
                };
                return Ok(LinuxDriveReport {
                    outcome: LinuxDriveOutcome::Halted(status),
                    stdout,
                    fuel_consumed,
                    suspensions,
                });
            }
            LinuxSliceOutcome::HostRequest(request) => {
                if request.channel == LINUX_HOME_9P_CHANNEL {
                    if plan9.home.is_none() {
                        *plan9.home = Some(new_home_9p_session()?);
                    }
                    let response = plan9
                        .home
                        .as_mut()
                        .expect("home session inserted")
                        .serve(&request.message);
                    log_plan9_failure("home", &request.message, &response);
                    if response.len() > request.max_response_bytes {
                        machine
                            .fail_9p_request(request.id, HostRequestFailure::Failed)
                            .map_err(SysError::ApiError)?;
                        return Err(SysError::ApiError(
                            "home 9P response exceeds the machine's admitted maximum".to_string(),
                        ));
                    }
                    machine
                        .complete_9p_request(request.id, &response)
                        .map_err(|error| SysError::ApiError(error.to_string()))?;
                } else if request.channel == LINUX_WORKSPACE_9P_CHANNEL {
                    if plan9.workspace.is_none() {
                        *plan9.workspace = Some(new_workspace_9p_session()?);
                    }
                    let response = plan9
                        .workspace
                        .as_mut()
                        .expect("workspace session inserted")
                        .serve(&request.message);
                    log_plan9_failure("workspace", &request.message, &response);
                    if response.len() > request.max_response_bytes {
                        machine
                            .fail_9p_request(request.id, HostRequestFailure::Failed)
                            .map_err(SysError::ApiError)?;
                        return Err(SysError::ApiError(
                            "workspace 9P response exceeds the machine's admitted maximum"
                                .to_string(),
                        ));
                    }
                    machine
                        .complete_9p_request(request.id, &response)
                        .map_err(|error| SysError::ApiError(error.to_string()))?;
                } else {
                    machine
                        .fail_9p_request(request.id, HostRequestFailure::Denied)
                        .map_err(|error| SysError::ApiError(error.to_string()))?;
                }
                suspensions = suspensions.saturating_add(1);
                cooperate()?;
            }
            LinuxSliceOutcome::Trapped(trap) => {
                return Ok(LinuxDriveReport {
                    outcome: LinuxDriveOutcome::Trapped(trap),
                    stdout,
                    fuel_consumed,
                    suspensions,
                });
            }
        }
    }
}

fn log_plan9_failure(channel: &str, request: &[u8], response: &[u8]) {
    // 9P2000.L Rlerror is a seven-byte header followed by a little-endian
    // errno. Keep diagnostics path-free: the operation number, channel, and
    // stable errno are enough to identify an incomplete adapter boundary.
    if response.get(4) != Some(&7) || response.len() < 11 {
        return;
    }
    let Some(errno_bytes) = response.get(7..11).and_then(|bytes| bytes.try_into().ok()) else {
        return;
    };
    let operation = request.get(4).copied().unwrap_or_default();
    let errno = u32::from_le_bytes(errno_bytes);
    let detail = if operation == 26 {
        request
            .get(11..15)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
            .map(|valid| format!(" valid={valid:#x}"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let message = format!(
        "Linux 9P request failed: channel={channel} operation={operation} errno={errno}{detail}"
    );
    #[cfg(target_arch = "wasm32")]
    astrid_sdk::log::warn(message);
    // Native Linux-machine tests exercise the same parser and 9P response
    // path without an Astrid host. Never cross a component import from those
    // tests merely to emit diagnostics.
    #[cfg(not(target_arch = "wasm32"))]
    let _ = message;
}

fn linux_failure_report(
    backend: &'static str,
    outcome: LinuxDriveOutcome,
    stdout: Vec<u8>,
    fuel_consumed: u64,
    suspensions: u64,
    booted: bool,
) -> LinuxInvocationReport {
    let (outcome, exit_status, fault) = match outcome {
        LinuxDriveOutcome::FuelExhausted => ("fuel-exhausted", None, None),
        LinuxDriveOutcome::OutputLimit => ("host-fault", None, Some("output-limit".to_string())),
        LinuxDriveOutcome::Trapped(message) => ("trapped", None, Some(message)),
        LinuxDriveOutcome::Halted(status) => (
            "halted",
            Some(if status == 0 { 1 } else { status }),
            Some("Linux halted before reaching the requested supervisor boundary".to_string()),
        ),
        LinuxDriveOutcome::Ready | LinuxDriveOutcome::Command(_) => (
            "trapped",
            None,
            Some("invalid Linux supervisor transition".to_string()),
        ),
    };
    LinuxInvocationReport {
        backend,
        outcome,
        exit_status,
        fault,
        stdout,
        fuel_consumed,
        suspensions,
        booted,
        command_completed: false,
        clean_shutdown: false,
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|candidate| candidate == needle)
}

fn linux_frame_token() -> Result<String, SysError> {
    let bytes = astrid_sdk::runtime::random_bytes(LINUX_FRAME_TOKEN_BYTES)?;
    if bytes.len() != LINUX_FRAME_TOKEN_BYTES {
        return Err(SysError::ApiError(format!(
            "runtime returned {} bytes for a {LINUX_FRAME_TOKEN_BYTES}-byte Linux frame token",
            bytes.len()
        )));
    }
    let mut token = String::with_capacity(LINUX_FRAME_TOKEN_HEX_BYTES);
    for byte in bytes {
        token.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        token.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    Ok(token)
}

fn validate_linux_frame_token(token: &str) -> Result<(), SysError> {
    if token.len() == LINUX_FRAME_TOKEN_HEX_BYTES
        && token
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        Ok(())
    } else {
        Err(SysError::ApiError(
            "Linux frame token must be 32 lowercase hexadecimal characters".to_string(),
        ))
    }
}

fn linux_command_status(stdout: &[u8], frame_token: &str) -> Option<i32> {
    validate_linux_frame_token(frame_token).ok()?;
    let ready_at = find_last_bytes(stdout, LINUX_READY_MARKER)?;
    let result_prefix = format!("AOS END {frame_token} ");
    stdout[..ready_at]
        .windows(result_prefix.len())
        .enumerate()
        .filter_map(|(marker_at, candidate)| {
            if candidate != result_prefix.as_bytes() {
                return None;
            }
            let status_start = marker_at + result_prefix.len();
            let suffix = &stdout[status_start..ready_at];
            let status_end = suffix.windows(2).position(|bytes| bytes == b"\r\n")?;
            let status_bytes = &suffix[..status_end];
            if status_bytes.is_empty()
                || status_bytes.len() > 3
                || !status_bytes.iter().all(u8::is_ascii_digit)
            {
                return None;
            }
            let status = std::str::from_utf8(status_bytes)
                .ok()?
                .parse::<i32>()
                .ok()?;
            (status <= 255).then_some((marker_at, status))
        })
        .max_by_key(|(marker_at, _)| *marker_at)
        .map(|(_, status)| status)
}

fn find_last_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|candidate| candidate == needle)
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
            "linux-console" => RealmProgram::LinuxConsole,
            "linux-sh" => RealmProgram::LinuxSh,
            "linux-shutdown" => RealmProgram::LinuxShutdown,
            "write-file" => RealmProgram::WriteFile,
            "cat" => RealmProgram::Cat,
            _ => {
                return Err(SysError::ApiError(format!(
                    "unsupported realm command `{command}`; supported: pwd, echo, pipe-echo, guest-pipe-echo, realm-sh, rv64-smoke, rv64-supervisor, linux-boot, linux-console, linux-sh, linux-shutdown, write-file, cat, smoke-write"
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
                SelectedExecution::Linux(LinuxAction::Boot),
                vec!["linux-boot".to_string()],
            )
        }
        RealmProgram::LinuxConsole => {
            if args.args.is_empty() {
                return Err(SysError::ApiError(
                    "linux-console expects a command token".to_string(),
                ));
            }
            let command = args.args.join(" ");
            if command.is_empty()
                || command.len() > MAX_LINUX_COMMAND_BYTES
                || command
                    .as_bytes()
                    .iter()
                    .any(|byte| *byte == 0 || *byte == b'\n' || *byte == b'\r')
                || !(command == "ping" || command == "counter" || command.starts_with("echo "))
            {
                return Err(SysError::ApiError(
                    "linux-console accepts one bounded ping, counter, or echo command".to_string(),
                ));
            }
            (
                "linux-console",
                SelectedExecution::Linux(LinuxAction::Console),
                vec!["linux-console".to_string(), command],
            )
        }
        RealmProgram::LinuxSh => {
            require_arity("linux-sh", &args.args, 1)?;
            let script = args.args[0].clone();
            if script.is_empty()
                || script.len() > MAX_LINUX_COMMAND_BYTES.saturating_sub(3)
                || script
                    .as_bytes()
                    .iter()
                    .any(|byte| *byte == 0 || *byte == b'\n' || *byte == b'\r')
            {
                return Err(SysError::ApiError(
                    "linux-sh script is empty, too large, or contains a line break".to_string(),
                ));
            }
            (
                "linux-sh",
                SelectedExecution::Linux(LinuxAction::Shell),
                vec!["linux-sh".to_string(), script],
            )
        }
        RealmProgram::LinuxShutdown => {
            require_arity("linux-shutdown", &args.args, 0)?;
            (
                "linux-shutdown",
                SelectedExecution::Linux(LinuxAction::Shutdown),
                vec!["linux-shutdown".to_string()],
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

fn linux_effective_cwd(
    action: LinuxAction,
    requested_cwd: Option<&str>,
) -> Result<String, SysError> {
    if action != LinuxAction::Shell {
        if requested_cwd.is_some() {
            return Err(SysError::ApiError(
                "cwd applies to linux-sh, not Linux lifecycle or diagnostic actions".to_string(),
            ));
        }
        return Ok(LINUX_DEFAULT_CWD.to_string());
    }

    let Some(requested) = requested_cwd else {
        return Ok(LINUX_DEFAULT_CWD.to_string());
    };
    if !requested.starts_with('/') {
        return Err(SysError::ApiError(format!(
            "linux-sh cwd `{requested}` must be an absolute guest path"
        )));
    }
    let normalized = host::canonical_guest_path("/", requested)
        .map_err(|error| SysError::ApiError(format!("invalid linux-sh cwd: {error}")))?;
    if normalized.len() > MAX_LINUX_CWD_BYTES {
        return Err(SysError::ApiError(format!(
            "linux-sh cwd exceeds the {MAX_LINUX_CWD_BYTES}-byte guest protocol limit"
        )));
    }
    if normalized == LINUX_DEFAULT_CWD
        || normalized.starts_with("/home/agent/")
        || normalized == "/workspace"
        || normalized.starts_with("/workspace/")
    {
        Ok(normalized)
    } else {
        Err(SysError::ApiError(format!(
            "linux-sh cwd `{requested}` is outside the admitted /home/agent and /workspace roots"
        )))
    }
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
    configured_resources: RealmResources,
) -> StatusResponse {
    let active_resources = actor.resources;
    let effective_resources = active_resources.map(|resources| {
        let resources = actor
            .linux
            .ram_bytes
            .map_or(resources, |bytes| resources.with_linux_memory_bytes(bytes));
        actor
            .linux
            .vcpus
            .map_or(resources, |count| resources.with_linux_vcpus(count))
    });
    StatusResponse {
        realm: REALM_NAME,
        owner_principal: principal,
        state,
        default_cwd: DEFAULT_CWD,
        cwd_defaults: [
            ConsumerCwdStatus {
                consumer: PathConsumer::NestedCoreWasm,
                cwd: Some(DEFAULT_CWD),
            },
            ConsumerCwdStatus {
                consumer: PathConsumer::LinuxGuest,
                cwd: Some(LINUX_DEFAULT_CWD),
            },
            ConsumerCwdStatus {
                consumer: PathConsumer::BareRv64,
                cwd: None,
            },
        ],
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
                role: MountRole::AgentHome,
                guest_path: "/home/agent",
                source: "principal-home",
                declared_resource_root: REALM_HOME,
                display_name: "Agent Home",
                mode: "rw",
                durable: true,
                reference_stability: ReferenceStability::PrincipalGeneration,
                nested_wasm_projection: ProjectionState::Mounted,
                linux_projection: ProjectionState::Mounted,
            },
            MountStatus {
                role: MountRole::Workspace,
                guest_path: "/workspace",
                source: "invocation-cwd",
                declared_resource_root: "cwd://",
                display_name: "Workspace",
                mode: "rw",
                durable: false,
                reference_stability: ReferenceStability::Invocation,
                nested_wasm_projection: ProjectionState::Mounted,
                linux_projection: ProjectionState::Mounted,
            },
            MountStatus {
                role: MountRole::Temporary,
                guest_path: "/tmp",
                source: "principal-tmp",
                declared_resource_root: REALM_TMP,
                display_name: "Temporary Files",
                mode: "rw",
                durable: false,
                reference_stability: ReferenceStability::RealmBoot,
                nested_wasm_projection: ProjectionState::Mounted,
                linux_projection: ProjectionState::GuestRamOnly,
            },
        ],
        path_contract_version: PATH_CONTRACT_VERSION,
        physical_host_paths_visible: false,
        commands: [
            "pwd",
            "echo",
            "pipe-echo",
            "guest-pipe-echo",
            "realm-sh",
            "rv64-smoke",
            "rv64-supervisor",
            "linux-boot",
            "linux-console",
            "linux-sh",
            "linux-shutdown",
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
        linux_lifecycle: "lazy-principal-resident",
        linux_backend: DEFAULT_LINUX_BACKEND_ID,
        linux_cold_start: "full-boot-then-principal-resident",
        linux_state: actor.linux.state,
        linux_residency: "principal-affine-store",
        linux_vcpus: actor
            .linux
            .vcpus
            .unwrap_or(configured_resources.linux_vcpus),
        linux_ram_bytes: actor.linux.ram_bytes,
        linux_ram_persistent: actor.linux.state == "running",
        linux_ram_durability: "evictable-cache",
        linux_storage_persistent: true,
        linux_rootfs_persistent: false,
        linux_home_persistent: true,
        linux_boot_executions_this_actor_boot: actor.linux.boot_executions,
        linux_commands_completed_this_actor_boot: actor.linux.commands_completed,
        linux_clean_shutdowns: actor.linux.clean_shutdowns,
        linux_guest_steps_this_actor_boot: actor.linux.guest_steps,
        linux_last_outcome: actor.linux.last_outcome,
        linux_last_exit_status: actor.linux.last_exit_status,
        linux_guest_accounting_scope: "verified-principal+actor-boot",
        linux_outer_wasm_metering: "principal-affine-invocation",
        component_residency: "principal-affine-store",
        component_state_durability: "evictable-cache+durable-home",
        resources: ResourceStatus {
            configured: configured_resources,
            active: active_resources,
            effective: effective_resources,
            reconfigure_on_next_exec: active_resources
                .is_some_and(|active| active != configured_resources),
            configuration_scope: "principal-capsule-config",
            outer_enforcement: "astrid-principal-profile+wasmtime-store-limiter",
            allocation_behavior: "host-and-principal-admitted-auto-or-explicit",
        },
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

    const EXTERNAL_DEVELOPER_TOOLCHAIN_PROBE: &str = concat!(
        "set -eu; ",
        "rm -rf /workspace/aos-dev-probe; mkdir /workspace/aos-dev-probe; cd /workspace/aos-dev-probe; ",
        "bash --version; git --version; python3 --version; clang --version; clang++ --version; make --version; cmake --version; ninja --version; ",
        "rustc --version; cargo --version; rustup --version; rustup show active-toolchain; astrid-build --version; ",
        "python3 -c 'print(\"PYTHON_OK\")'; ",
        "printf '#include <stdio.h>\\nint main(void){puts(\"C_OK\");}\\n' > hello.c; cc hello.c -o hello-c; ./hello-c; ",
        "printf '#include <iostream>\\nint main(){std::cout << \"CXX_OK\\\\n\";}\\n' > hello.cc; c++ hello.cc -o hello-cxx; ./hello-cxx; ",
        "printf 'all:\\n\\t@echo MAKE_OK\\n' > Makefile; make; ",
        "printf 'cmake_minimum_required(VERSION 3.15)\\nproject(probe C)\\nadd_executable(cmake-probe hello.c)\\n' > CMakeLists.txt; ",
        "cmake -S . -B cmake-build -G Ninja; cmake --build cmake-build; ./cmake-build/cmake-probe; ",
        "mkdir rust-probe; printf '[package]\\nname=\"realm-probe\"\\nversion=\"0.1.0\"\\nedition=\"2024\"\\n\\n[dependencies]\\n' > rust-probe/Cargo.toml; ",
        "mkdir rust-probe/src; printf 'fn main(){println!(\"RUST_OK\");}\\n' > rust-probe/src/main.rs; ",
        "cargo build --manifest-path rust-probe/Cargo.toml --release --offline; ./rust-probe/target/release/realm-probe; ",
        "printf '#[unsafe(no_mangle)] pub extern \"C\" fn realm_wasm_probe() -> i32 { 42 }\\n' > rust-probe/src/lib.rs; ",
        "rustc --crate-type=cdylib --target=wasm32-unknown-unknown -O rust-probe/src/lib.rs -o rust-probe/realm-probe.wasm; ",
        "test \"$(od -An -tx1 -N4 rust-probe/realm-probe.wasm | tr -d ' \\n')\" = 0061736d; echo WASM_OK; ",
        "git init -q; git config user.name Agent; git config user.email agent@aos.invalid; git add .; git commit -qm probe; git rev-parse --verify HEAD; ",
        "echo TOOLCHAIN_OK",
    );

    #[test]
    fn shell_frame_preserves_bootstrap_compatibility_and_opts_into_resource_limits() {
        assert_eq!(
            linux_shell_command_frame(0, 0, 0, "/workspace", "cargo test"),
            "sh 0 10 /workspace cargo test"
        );
        assert_eq!(
            linux_shell_command_frame(4096, 2048, 0, "/home/agent", "rustup show"),
            "sh 4096 2048 11 /home/agent rustup show"
        );
        assert_eq!(
            linux_shell_command_frame(4096, 0, 65_536, "/workspace", "cargo build"),
            "sh 4096 0 65536 10 /workspace cargo build"
        );
    }

    #[test]
    fn cli_actions_are_structured_and_preserve_one_quoted_script() {
        let action = parse_cli_action(&[
            "sh".to_string(),
            "--cwd".to_string(),
            "/workspace/project".to_string(),
            "set -e; cargo test".to_string(),
        ])
        .expect("valid shell action");
        let CliAction::Exec(args) = action else {
            panic!("shell action must execute the Realm");
        };
        assert_eq!(args.command.as_deref(), Some("linux-sh"));
        assert_eq!(args.cwd.as_deref(), Some("/workspace/project"));
        assert_eq!(args.args, ["set -e; cargo test"]);

        let action = parse_cli_action(&[
            "exec".to_string(),
            "--cwd".to_string(),
            "/home/agent".to_string(),
            "write-file".to_string(),
            "proof.txt".to_string(),
            "durable".to_string(),
        ])
        .expect("valid generic action");
        let CliAction::Exec(args) = action else {
            panic!("generic action must execute the Realm");
        };
        assert_eq!(args.command.as_deref(), Some("write-file"));
        assert_eq!(args.cwd.as_deref(), Some("/home/agent"));
        assert_eq!(args.args, ["proof.txt", "durable"]);
    }

    #[test]
    fn agent_realm_shell_can_only_select_the_linux_guest_shell() {
        let args = realm_shell_exec_args(RealmShellArgs {
            command: "cc hello.c -o hello && ./hello".to_string(),
            cwd: Some("/workspace/project".to_string()),
            max_steps: Some(50_000_000),
            max_output_bytes: Some(16_384),
        });

        assert!(matches!(args.program, Some(RealmProgram::LinuxSh)));
        assert!(args.command.is_none(), "no executable selector is accepted");
        assert_eq!(args.args, ["cc hello.c -o hello && ./hello"]);
        assert_eq!(args.cwd.as_deref(), Some("/workspace/project"));
        assert_eq!(args.fuel, Some(50_000_000));
        assert_eq!(args.max_output_bytes, Some(16_384));
    }

    #[test]
    fn cli_actions_reject_ambiguous_or_incomplete_shell_input() {
        let error = parse_cli_action(&["sh".to_string(), "echo".to_string(), "split".to_string()])
            .expect_err("a shell script must remain one CLI argument");
        assert!(error.contains("one quoted script"));

        let error = parse_cli_action(&["sh".to_string(), "--cwd".to_string()])
            .expect_err("cwd needs a value");
        assert!(error.contains("--cwd requires"));
    }

    #[test]
    fn cli_request_ids_cannot_steer_result_topics() {
        assert!(is_valid_cli_request_id("0123456789abcdef0123456789abcdef"));
        assert!(is_valid_cli_request_id(
            "01234567-89ab-cdef-0123-456789abcdef"
        ));
        for invalid in ["", "ABC", "../../escape", "segment.result", "wild*"] {
            assert!(!is_valid_cli_request_id(invalid), "accepted {invalid:?}");
        }
    }

    #[test]
    fn cli_exit_status_cannot_report_a_failed_boot_as_success() {
        assert_eq!(
            cli_exit_code(&serde_json::json!({"outcome": "ready", "exit_status": null})),
            0
        );
        assert_eq!(
            cli_exit_code(&serde_json::json!({"outcome": "completed", "exit_status": 7})),
            7
        );
        assert_eq!(
            cli_exit_code(&serde_json::json!({"outcome": "halted", "exit_status": 0})),
            1
        );
        assert_eq!(
            cli_exit_code(&serde_json::json!({"outcome": "host-fault", "exit_status": null})),
            1
        );
    }

    #[test]
    fn an_unrequested_linux_halt_is_a_failure_even_when_sbi_reports_success() {
        let report = linux_failure_report(
            DEFAULT_LINUX_BACKEND_ID,
            LinuxDriveOutcome::Halted(0),
            b"AOS STORAGE FAILED\r\n".to_vec(),
            42,
            3,
            true,
        );
        assert_eq!(report.outcome, "halted");
        assert_eq!(report.exit_status, Some(1));
        assert!(report.fault.is_some());
        assert!(!report.command_completed);
    }

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
        assert_eq!(
            response.requested_cwd.as_deref(),
            Some("/workspace/project")
        );
        assert_eq!(response.cwd, "/workspace/project");
        let json = serde_json::to_string(&response).expect("response serializes");
        assert!(json.contains("\"consumer\":\"nested-core-wasm\""));
        assert!(json.contains("\"resource_uri\":\"cwd://project\""));
        assert!(json.contains("\"display_path\":\"Workspace/project\""));
        assert!(json.contains("\"reference_stability\":\"invocation\""));
    }

    #[test]
    fn linux_cwd_admits_only_the_two_real_shell_roots() {
        assert_eq!(
            linux_effective_cwd(LinuxAction::Shell, None).expect("default Linux cwd"),
            LINUX_DEFAULT_CWD
        );
        assert_eq!(
            linux_effective_cwd(LinuxAction::Shell, Some(LINUX_DEFAULT_CWD))
                .expect("explicit Linux home"),
            LINUX_DEFAULT_CWD
        );

        assert_eq!(
            linux_effective_cwd(LinuxAction::Shell, Some("/workspace"))
                .expect("workspace is an invocation-scoped Linux mount"),
            "/workspace"
        );
        assert_eq!(
            linux_effective_cwd(LinuxAction::Shell, Some("/workspace/src"))
                .expect("workspace children remain inside the admitted mount"),
            "/workspace/src"
        );

        let outside_error = linux_effective_cwd(LinuxAction::Shell, Some("/etc"))
            .expect_err("an unadmitted Linux cwd cannot be selected");
        assert!(outside_error.to_string().contains("outside the admitted"));

        let oversized = format!("/workspace/{}", "x".repeat(MAX_LINUX_CWD_BYTES));
        let oversized_error = linux_effective_cwd(LinuxAction::Shell, Some(&oversized))
            .expect_err("PID 1 cannot admit a CWD larger than its frame limit");
        assert!(oversized_error.to_string().contains("64-byte"));

        let lifecycle_error = linux_effective_cwd(LinuxAction::Console, Some(LINUX_DEFAULT_CWD))
            .expect_err("diagnostic actions do not accept a cwd");
        assert!(
            lifecycle_error
                .to_string()
                .contains("cwd applies to linux-sh")
        );
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
    fn linux_boot_and_console_preserve_userspace_across_invocations() {
        let limits = RunLimits {
            fuel: RealmResources::default().effective_max_steps(),
            memory_bytes: DEFAULT_LINUX_MEMORY_BYTES,
            output_bytes: HARD_MAX_OUTPUT_BYTES,
        };
        let mut machine = None;
        let mut home_9p = None;
        let mut workspace_9p = None;
        let boot = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Boot,
            None,
            LINUX_DEFAULT_CWD,
            None,
            limits,
            || Ok(()),
        )
        .expect("embedded Linux boot succeeds");

        assert_eq!(
            boot.outcome,
            "ready",
            "Linux boot output:\n{}",
            String::from_utf8_lossy(&boot.stdout)
        );
        assert!(machine.is_some());
        let stdout = String::from_utf8_lossy(&boot.stdout);
        assert!(stdout.contains("AOS LINUX /init"));
        assert!(contains_bytes(&boot.stdout, LINUX_READY_MARKER));
        assert!(boot.fuel_consumed > LINUX_SLICE_STEPS);
        assert!(boot.suspensions > 1);

        let first = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Console,
            Some("counter"),
            LINUX_DEFAULT_CWD,
            Some("0123456789abcdef0123456789abcdef"),
            limits,
            || Ok(()),
        )
        .expect("first resident command");
        let second = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Console,
            Some("counter"),
            LINUX_DEFAULT_CWD,
            Some("fedcba9876543210fedcba9876543210"),
            limits,
            || Ok(()),
        )
        .expect("second resident command");

        assert_eq!(first.outcome, "completed");
        assert_eq!(first.exit_status, Some(0));
        assert!(String::from_utf8_lossy(&first.stdout).contains("counter=1"));
        assert_eq!(second.outcome, "completed");
        assert!(String::from_utf8_lossy(&second.stdout).contains("counter=2"));
        assert!(machine.is_some(), "Linux RAM remains resident");

        let shell = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some(
                "set -e; test ! -r /dev/console; test ! -r /proc/1/fd/0; test ! -r /proc/self/fd/1; test ! -r /proc/self/fd/2; test \"$(find /dev -type b -o -type c | wc -l)\" -eq 5; id -u; uname -m; pwd; mkdir -p .config/aos; printf persisted > .config/aos/proof.tmp; mv .config/aos/proof.tmp .config/aos/proof",
            ),
            LINUX_DEFAULT_CWD,
            Some("11223344556677889900aabbccddeeff"),
            limits,
            || Ok(()),
        )
        .expect("BusyBox ash command");
        let shell_stdout = String::from_utf8_lossy(&shell.stdout);
        assert_eq!(shell.outcome, "completed", "shell output:\n{shell_stdout}");
        assert_eq!(shell.exit_status, Some(0));
        assert!(shell_stdout.contains("1000\r\n"));
        assert!(shell_stdout.contains("riscv64\r\n"));
        assert!(shell_stdout.contains("/home/agent\r\n"));

        let persisted = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some("cat .config/aos/proof"),
            LINUX_DEFAULT_CWD,
            Some("22334455667788990011aabbccddeeff"),
            limits,
            || Ok(()),
        )
        .expect("resident shell reads its prior file");
        assert_eq!(persisted.exit_status, Some(0));
        assert!(String::from_utf8_lossy(&persisted.stdout).contains("persisted"));

        let file_limited = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some("rm -f .config/aos/limited; truncate -s 5 .config/aos/limited"),
            LINUX_DEFAULT_CWD,
            Some("2a2a4455667788990011aabbccddeeff"),
            LinuxInvocationLimits {
                run: limits,
                max_file_bytes: 4,
                max_processes: 0,
                max_open_files: 0,
                vcpus: 1,
            },
            || Ok(()),
        )
        .expect("guest enforces the configured per-file ceiling");
        assert_eq!(file_limited.exit_status, Some(153));

        let file_limit_cleanup = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some("rm -f .config/aos/limited"),
            LINUX_DEFAULT_CWD,
            Some("2b2b4455667788990011aabbccddeeff"),
            limits,
            || Ok(()),
        )
        .expect("default envelope removes only the inner file ceiling");
        assert_eq!(file_limit_cleanup.exit_status, Some(0));

        let workspace_write = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some(
                "set -e; test \"$(pwd)\" = /workspace; mkdir bridge; printf written-by-linux > proof; mv proof bridge/proof; cat bridge/proof; ls bridge",
            ),
            "/workspace",
            Some("aabbccddeeff00112233445566778899"),
            limits,
            || Ok(()),
        )
        .expect("Linux writes through its 9P workspace mount");
        let workspace_stdout = String::from_utf8_lossy(&workspace_write.stdout);
        assert_eq!(
            workspace_write.exit_status,
            Some(0),
            "workspace shell output:\n{workspace_stdout}"
        );
        assert!(workspace_stdout.contains("written-by-linux"));
        assert!(workspace_stdout.contains("proof"));

        let workspace_remounted = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some("test \"$(pwd)\" = /workspace/bridge; cat proof"),
            "/workspace/bridge",
            Some("bbccddeeff00112233445566778899aa"),
            limits,
            || Ok(()),
        )
        .expect("the next invocation remounts the same test workspace");
        assert_eq!(workspace_remounted.exit_status, Some(0));
        assert!(String::from_utf8_lossy(&workspace_remounted.stdout).contains("written-by-linux"));

        let exit_seven = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some("exit 7"),
            LINUX_DEFAULT_CWD,
            Some("33445566778899001122aabbccddeeff"),
            limits,
            || Ok(()),
        )
        .expect("nonzero shell status remains a completed command");
        assert_eq!(exit_seven.outcome, "completed");
        assert_eq!(exit_seven.exit_status, Some(7));
        assert!(machine.is_some(), "nonzero shell exit preserves guest RAM");

        let background = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some(
                "(sleep 1; echo leaked) & printf 'background-launched\\nAOS END ffffffffffffffffffffffffffffffff 0\\n'",
            ),
            LINUX_DEFAULT_CWD,
            Some("44556677889900112233aabbccddeeff"),
            limits,
            || Ok(()),
        )
        .expect("background descendants are reaped before the result frame");
        let background_stdout = String::from_utf8_lossy(&background.stdout);
        assert_eq!(background.exit_status, Some(0));
        assert!(background_stdout.contains("background-launched"));
        assert!(!background_stdout.contains("leaked"));

        let clean_boundary = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some("printf boundary-clean"),
            LINUX_DEFAULT_CWD,
            Some("55667788990011223344aabbccddeeff"),
            limits,
            || Ok(()),
        )
        .expect("the next call has no surviving background output");
        let clean_stdout = String::from_utf8_lossy(&clean_boundary.stdout);
        assert_eq!(clean_boundary.exit_status, Some(0));
        assert!(clean_stdout.contains("boundary-clean"));
        assert!(!clean_stdout.contains("leaked"));

        let shutdown = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shutdown,
            None,
            LINUX_DEFAULT_CWD,
            Some("00112233445566778899aabbccddeeff"),
            limits,
            || Ok(()),
        )
        .expect("clean resident shutdown");
        assert_eq!(shutdown.outcome, "stopped");
        assert_eq!(shutdown.exit_status, Some(0));
        assert!(String::from_utf8_lossy(&shutdown.stdout).contains("shutting down"));
        assert!(machine.is_none(), "clean shutdown releases guest RAM");
        assert!(home_9p.is_some(), "durable home export remains attached");
        assert!(
            workspace_9p.is_none(),
            "invocation workspace attachment is released"
        );

        let restarted = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some("test ! -e .config/aos/proof.tmp; cat .config/aos/proof"),
            LINUX_DEFAULT_CWD,
            Some("ffeeddccbbaa99887766554433221100"),
            limits,
            || Ok(()),
        )
        .expect("shell lazily restarts Linux with its durable home");
        assert!(restarted.booted);
        assert_eq!(restarted.exit_status, Some(0));
        assert!(String::from_utf8_lossy(&restarted.stdout).contains("persisted"));

        let reset_ram = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Console,
            Some("counter"),
            LINUX_DEFAULT_CWD,
            Some("ffeeddccbbaa99887766554433221101"),
            limits,
            || Ok(()),
        )
        .expect("console state starts from the new guest boot");
        // The cold-boot readback shell was command 1 in the new PID 1; this
        // diagnostic is command 2 rather than continuing the old guest count.
        assert!(String::from_utf8_lossy(&reset_ram.stdout).contains("counter=2"));
    }

    #[test]
    #[ignore = "boots the external pinned developer image under the full RV64 interpreter"]
    fn external_developer_image_executes_the_complete_base_toolchain() {
        assert!(EXTERNAL_DEVELOPER_TOOLCHAIN_PROBE.len() <= MAX_LINUX_COMMAND_BYTES);
        let image_path = std::env::var_os("AOS_REALM_TEST_LINUX_IMAGE")
            .expect("set AOS_REALM_TEST_LINUX_IMAGE to the recorded Linux Image");
        let image = std::fs::read(&image_path).expect("read external developer image");
        let limits = RunLimits {
            fuel: u64::MAX,
            memory_bytes: 3 * 1024 * 1024 * 1024,
            output_bytes: HARD_MAX_OUTPUT_BYTES,
        };
        let mut machine = Some(
            LinuxMachine::new_reference_with_image(
                Rv64MachineConfig {
                    ram_bytes: limits.memory_bytes,
                    max_console_bytes: HARD_MAX_OUTPUT_BYTES,
                },
                1,
                &image,
            )
            .expect("admit external developer image"),
        );
        let mut home_9p = None;
        let mut workspace_9p = None;
        let boot = drive_linux_until(
            machine.as_mut().expect("machine inserted"),
            LinuxPlan9State {
                home: &mut home_9p,
                workspace: &mut workspace_9p,
            },
            limits.fuel,
            limits.output_bytes,
            LinuxAction::Boot,
            None,
            &mut || Ok(()),
        )
        .expect("drive external image to readiness");
        assert!(
            matches!(boot.outcome, LinuxDriveOutcome::Ready),
            "external image boot output:\n{}",
            String::from_utf8_lossy(&boot.stdout)
        );
        eprintln!(
            "external developer image ready after {} steps and {} host suspensions",
            boot.fuel_consumed, boot.suspensions
        );

        let result = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shell,
            Some(EXTERNAL_DEVELOPER_TOOLCHAIN_PROBE),
            "/workspace",
            Some("decafbaddecafbaddecafbaddecafbad"),
            limits,
            || Ok(()),
        )
        .expect("execute complete developer toolchain probe");
        let stdout = String::from_utf8_lossy(&result.stdout);
        eprintln!("external developer toolchain output:\n{stdout}");
        assert_eq!(result.outcome, "completed", "toolchain output:\n{stdout}");
        assert_eq!(result.exit_status, Some(0), "toolchain output:\n{stdout}");
        for marker in [
            "GNU bash, version 5.2.37",
            "git version 2.54.0",
            "Python 3.14.6",
            "clang version 22.1.7",
            "GNU Make 4.4.1",
            "cmake version 4.3.2",
            "1.13.2",
            "rustc 1.97.1",
            "cargo 1.97.1",
            "rustup 1.29.0",
            "aos-system",
            "astrid-build 0.10.4",
            "PYTHON_OK",
            "C_OK",
            "CXX_OK",
            "MAKE_OK",
            "RUST_OK",
            "WASM_OK",
            "TOOLCHAIN_OK",
        ] {
            assert!(stdout.contains(marker), "missing {marker:?} in:\n{stdout}");
        }
    }

    #[test]
    fn bounded_cold_boot_cooperates_at_every_slice_boundary() {
        let mut cooperative_yields = 0_u64;
        let mut machine = None;
        let mut home_9p = None;
        let mut workspace_9p = None;
        let report = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Boot,
            None,
            LINUX_DEFAULT_CWD,
            None,
            RunLimits {
                fuel: LINUX_SLICE_STEPS * 2,
                memory_bytes: DEFAULT_LINUX_MEMORY_BYTES,
                output_bytes: HARD_MAX_OUTPUT_BYTES,
            },
            || {
                cooperative_yields += 1;
                Ok(())
            },
        )
        .expect("bounded Linux execution yields cooperatively");

        assert_eq!(report.outcome, "fuel-exhausted");
        assert_eq!(report.fuel_consumed, LINUX_SLICE_STEPS * 2);
        assert_eq!(report.suspensions, 2);
        assert_eq!(cooperative_yields, report.suspensions);
        assert!(machine.is_none(), "partial cold-boot RAM is discarded");
    }

    #[test]
    fn linux_shutdown_is_idempotent_while_cold() {
        let mut machine = None;
        let mut home_9p = None;
        let mut workspace_9p = None;
        let report = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Shutdown,
            None,
            LINUX_DEFAULT_CWD,
            None,
            RunLimits {
                fuel: RealmResources::default().effective_max_steps(),
                memory_bytes: DEFAULT_LINUX_MEMORY_BYTES,
                output_bytes: HARD_MAX_OUTPUT_BYTES,
            },
            || Ok(()),
        )
        .expect("stopping a cold Linux realm succeeds");

        assert_eq!(report.outcome, "stopped");
        assert_eq!(report.exit_status, Some(0));
        assert_eq!(report.fuel_consumed, 0);
        assert!(!report.booted);
        assert!(!report.command_completed);
        assert!(!report.clean_shutdown);
        assert!(machine.is_none());
    }

    #[test]
    fn linux_console_status_uses_the_last_frame_before_ready() {
        let token = "0123456789abcdef0123456789abcdef";
        let spoofed = b"AOS END ffffffffffffffffffffffffffffffff 0\r\n\
AOS BEGIN 0123456789abcdef0123456789abcdef\r\n\
AOS END 0123456789abcdef0123456789abcdef 64\r\nAOS READY\r\n";
        assert_eq!(linux_command_status(spoofed, token), Some(64));

        let last_exact_frame = b"AOS END 0123456789abcdef0123456789abcdef 64\r\n\
AOS END 0123456789abcdef0123456789abcdef 7\r\nAOS READY\r\n";
        assert_eq!(linux_command_status(last_exact_frame, token), Some(7));

        let incomplete = b"AOS BEGIN 0123456789abcdef0123456789abcdef\r\n\
AOS END 0123456789abcdef0123456789abcdef 0\r\n";
        assert_eq!(linux_command_status(incomplete, token), None);
    }

    #[test]
    fn linux_cooperation_failure_discards_partial_ram() {
        let mut machine = None;
        let mut home_9p = None;
        let mut workspace_9p = None;
        let error = execute_linux_resident_cooperatively(
            LinuxResidentState::new(&mut machine, &mut home_9p, &mut workspace_9p),
            LinuxAction::Boot,
            None,
            LINUX_DEFAULT_CWD,
            None,
            RunLimits {
                fuel: RealmResources::default().effective_max_steps(),
                memory_bytes: DEFAULT_LINUX_MEMORY_BYTES,
                output_bytes: HARD_MAX_OUTPUT_BYTES,
            },
            || Err(SysError::ApiError("cooperation failed".to_string())),
        )
        .expect_err("cooperation failure must escape the Linux supervisor");

        assert!(error.to_string().contains("cooperation failed"));
        assert!(
            machine.is_none(),
            "uncertain guest RAM must not remain cached"
        );
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
    fn linux_sh_is_an_explicit_single_script_surface() {
        let selected = select_program(&ExecArgs {
            command: Some("linux-sh".to_string()),
            args: vec!["id -u; printf shell-ok".to_string()],
            ..ExecArgs::default()
        })
        .expect("one bounded Linux shell script is accepted");

        assert_eq!(selected.name, "linux-sh");
        assert!(matches!(
            selected.execution,
            SelectedExecution::Linux(LinuxAction::Shell)
        ));
        assert_eq!(selected.argv, ["linux-sh", "id -u; printf shell-ok"]);

        for args in [
            Vec::new(),
            vec!["echo one".to_string(), "echo two".to_string()],
            vec!["echo one\necho two".to_string()],
            vec!["x".repeat(MAX_LINUX_COMMAND_BYTES)],
        ] {
            let error = select_program(&ExecArgs {
                command: Some("linux-sh".to_string()),
                args,
                ..ExecArgs::default()
            })
            .expect_err("ambiguous or unframeable Linux shell input is rejected");
            assert!(error.to_string().contains("linux-sh"));
        }

        let bypass = select_program(&ExecArgs {
            command: Some("linux-console".to_string()),
            args: vec!["sh".to_string(), "id -u".to_string()],
            ..ExecArgs::default()
        })
        .expect_err("the diagnostic console cannot bypass the linux-sh surface");
        assert!(bypass.to_string().contains("ping, counter, or echo"));
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
                linux: LinuxSnapshot {
                    state: "cold",
                    boot_executions: 2,
                    clean_shutdowns: 1,
                    guest_steps: 25_000_000,
                    last_outcome: Some("exited"),
                    last_exit_status: Some(0),
                    commands_completed: 3,
                    ram_bytes: None,
                    vcpus: None,
                },
                resources: Some(RealmResources::default()),
            },
            RealmResources::default(),
        ))
        .expect("status serializes");

        assert!(json.contains("/workspace"));
        assert!(json.contains("/home/agent"));
        assert!(json.contains("kv-cas-head+content-addressed-file-blobs"));
        assert!(json.contains("\"home_generation\":7"));
        assert!(json.contains("\"realm_boot_sequence\":9"));
        assert!(json.contains("\"commands_completed\":4"));
        assert!(json.contains("\"linux_lifecycle\":\"lazy-principal-resident\""));
        assert!(json.contains("\"linux_backend\":\"aos-rv64-interpreter\""));
        assert!(json.contains("\"linux_cold_start\":\"full-boot-then-principal-resident\""));
        assert!(json.contains("\"linux-sh\""));
        assert!(json.contains("\"linux_state\":\"cold\""));
        assert!(json.contains("\"linux_boot_executions_this_actor_boot\":2"));
        assert!(json.contains("\"linux_commands_completed_this_actor_boot\":3"));
        assert!(json.contains("\"linux_guest_steps_this_actor_boot\":25000000"));
        assert!(json.contains("\"linux_outer_wasm_metering\":\"principal-affine-invocation\""));
        assert!(json.contains("\"component_residency\":\"principal-affine-store\""));
        assert!(json.contains("\"configuration_scope\":\"principal-capsule-config\""));
        assert!(
            json.contains(
                "\"outer_enforcement\":\"astrid-principal-profile+wasmtime-store-limiter\""
            )
        );
        assert!(json.contains("\"reconfigure_on_next_exec\":false"));
        assert!(json.contains("\"path_contract_version\":1"));
        assert!(json.contains("\"physical_host_paths_visible\":false"));
        assert!(json.contains(
            "\"cwd_defaults\":[{\"consumer\":\"nested-core-wasm\",\"cwd\":\"/workspace\"},{\"consumer\":\"linux-guest\",\"cwd\":\"/home/agent\"},{\"consumer\":\"bare-rv64\",\"cwd\":null}]"
        ));
        assert!(json.contains("\"linux_projection\":\"mounted\""));
        assert!(json.contains("\"linux_projection\":\"guest-ram-only\""));
        assert!(json.contains("\"linux_storage_persistent\":true"));
        assert!(json.contains("\"linux_rootfs_persistent\":false"));
        assert!(json.contains("\"linux_home_persistent\":true"));
        assert!(json.contains("outer-astrid-promotion-required"));
        assert!(!json.contains("/Users/"));
        assert!(!json.contains(".astrid/home"));
    }

    #[test]
    fn status_reports_resource_reconfiguration_without_mutating_the_actor() {
        let active = RealmResources::default();
        let configured = RealmResources {
            linux_memory_bytes: 64 * 1024 * 1024,
            ..active
        };
        let response = status_response(
            "alice".to_string(),
            "ready",
            FsStatus {
                format: aos_realm_vfs::FORMAT_VERSION,
                generation: 0,
                files: 0,
                manifest: None,
            },
            ActorSnapshot {
                state: "running",
                boot_sequence: 1,
                commands_completed: 0,
                machine: RealmMachine::default().status(),
                linux: LinuxSnapshot::cold(),
                resources: Some(active),
            },
            configured,
        );

        assert_eq!(response.resources.active, Some(active));
        assert_eq!(response.resources.configured, configured);
        assert!(response.resources.reconfigure_on_next_exec);
    }

    #[test]
    fn status_distinguishes_auto_configuration_from_effective_guest_ram() {
        let configured = RealmResources::default();
        let response = status_response(
            "alice".to_string(),
            "ready",
            FsStatus {
                format: aos_realm_vfs::FORMAT_VERSION,
                generation: 0,
                files: 0,
                manifest: None,
            },
            ActorSnapshot {
                state: "running",
                boot_sequence: 1,
                commands_completed: 0,
                machine: RealmMachine::default().status(),
                linux: LinuxSnapshot {
                    state: "running",
                    ram_bytes: Some(1024 * 1024 * 1024),
                    vcpus: Some(12),
                    ..LinuxSnapshot::cold()
                },
                resources: Some(configured),
            },
            configured,
        );

        assert_eq!(response.linux_ram_bytes, Some(1024 * 1024 * 1024));
        assert_eq!(response.linux_vcpus, 12);
        assert_eq!(response.resources.active, Some(configured));
        assert_eq!(
            response
                .resources
                .effective
                .expect("running Realm has an effective envelope")
                .linux_memory_bytes,
            1024 * 1024 * 1024
        );
        assert_eq!(
            response
                .resources
                .effective
                .expect("running Realm has an effective envelope")
                .linux_vcpus,
            12
        );
        assert!(!response.resources.reconfigure_on_next_exec);
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

        assert_eq!(
            manifest["package"]["astrid-version"].as_str(),
            Some(">=0.10.2")
        );
        assert_eq!(
            manifest["package"]["metadata"]["astrid-runtime"]["component-residency"].as_str(),
            Some("principal")
        );
        assert_eq!(
            manifest["env"]["linux_memory_bytes"]["default"].as_str(),
            Some("0")
        );
        assert_eq!(
            manifest["env"]["linux_max_steps"]["default"].as_str(),
            Some("0")
        );
        assert_eq!(
            manifest["env"]["linux_max_file_bytes"]["default"].as_str(),
            Some("0")
        );
        assert_eq!(
            manifest["env"]["linux_max_processes"]["default"].as_str(),
            Some("0")
        );
        assert_eq!(
            manifest["env"]["linux_max_open_files"]["default"].as_str(),
            Some("0")
        );
        assert!(capabilities.contains_key("fs_read"));
        assert!(capabilities.contains_key("fs_write"));
        assert_eq!(
            manifest["subscribe"]["tool.v1.execute.realm_shell"]["handler"].as_str(),
            Some("tool_execute_realm_shell")
        );
    }

    #[test]
    fn signed_worker_manifest_lock_and_bytes_are_one_identity() {
        let manifest: toml::Value = include_str!("../Capsule.toml")
            .parse()
            .expect("Capsule.toml parses");
        let component = manifest["component"]
            .as_array()
            .expect("components are an array")
            .iter()
            .find(|component| component["id"].as_str() == Some("linux-vcpu"))
            .expect("Linux vCPU component");
        let bytes = include_bytes!("../assets/linux-vcpu.wasm");
        let actual_hash = format!("blake3:{}", blake3::hash(bytes).to_hex());
        assert_eq!(component["hash"].as_str(), Some(actual_hash.as_str()));

        let lock = include_str!("../assets/linux-vcpu.lock");
        let field = |name: &str| {
            lock.lines()
                .find_map(|line| line.strip_prefix(&format!("{name}=")))
                .unwrap_or_else(|| panic!("worker lock is missing {name}"))
        };
        assert_eq!(field("protocol"), "aos-linux-vcpu-1");
        assert_eq!(field("worker_bytes"), bytes.len().to_string());
        assert_eq!(format!("blake3:{}", field("worker_blake3")), actual_hash);
    }
}
