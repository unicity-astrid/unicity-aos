//! Stateful RV64 machine worker behind the Astrid generic-compute ABI.
//!
//! This core-Wasm module deliberately imports only shared memory and immutable
//! asset reads from `astrid_compute`. The controller owns every host effect and
//! exchanges bounded commands, slice reports, console bytes, and 9P messages
//! through the descriptor region in that shared memory.

#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![allow(
    unsafe_code,
    reason = "the compute ABI passes a validated shared-memory range"
)]

use aos_realm_machine::{
    CheckpointBinding, CheckpointDigest, HaltStatus, HostRequestFailure, HostRequestId, Machine,
    MachineCheckpoint, MachineConfig, SliceOutcome,
};
use aos_realm_vcpu_protocol::{
    self as protocol, Operation, Outcome, RequestFailure, Status, field,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const ABI_VERSION: i32 = 1;
#[cfg(target_arch = "wasm32")]
const LINUX_KERNEL_ASSET_INDEX: i32 = 0;
#[cfg(target_arch = "wasm32")]
const LINUX_SYSTEM_ASSET_INDEX: i32 = 1;
#[cfg(target_arch = "wasm32")]
const LINUX_CHECKPOINT_ASSET_INDEX: i32 = 2;
const LINUX_SYSTEM_BLOCK_CHANNEL: u32 = 3;
const LINUX_SYSTEM_SECTOR_BYTES: usize = 512;
// Keep finite admission ceilings with ample room for patch releases. The exact
// attached objects are hash-bound and worker memory remains charged to the
// principal by generic compute.
#[cfg(any(target_arch = "wasm32", test))]
const MAX_LINUX_KERNEL_BYTES: usize = 512 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
const MAX_LINUX_CHECKPOINT_BYTES: usize = 512 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
const MAX_LINUX_SYSTEM_BYTES: usize = 2 * 1024 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
const MAX_ASSET_READ_BYTES: usize = 64 * 1024;
#[cfg(target_arch = "wasm32")]
const ASSET_OK: i32 = 0;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "astrid_compute")]
unsafe extern "C" {
    #[link_name = "asset_count"]
    fn host_asset_count() -> i32;
    #[link_name = "asset_size"]
    fn host_asset_size(index: i32) -> i64;
    #[link_name = "asset_read"]
    fn host_asset_read(index: i32, offset: i64, destination: i64, length: i64) -> i32;
}

struct WorkerState {
    machine: Machine,
    pending_request: Option<HostRequestId>,
}

static STATE: Mutex<Option<WorkerState>> = Mutex::new(None);
static PARALLEL_PROBE_ARRIVALS: AtomicU64 = AtomicU64::new(0);

/// Generic-compute worker ABI version.
#[unsafe(no_mangle)]
pub extern "C" fn astrid_compute_abi_version() -> i32 {
    ABI_VERSION
}

/// Total linear-memory stack arena reserved by the signed worker linker.
#[unsafe(no_mangle)]
pub extern "C" fn astrid_compute_stack_reserve_bytes() -> i32 {
    protocol::WORKER_STACK_RESERVE_BYTES as i32
}

/// One worker's private stack stride inside the reserved arena.
#[unsafe(no_mangle)]
pub extern "C" fn astrid_compute_stack_stride_bytes() -> i32 {
    protocol::WORKER_STACK_STRIDE_BYTES as i32
}

/// Execute one bounded vCPU control operation.
///
/// The runtime has already validated the descriptor range against shared
/// memory. This function validates the protocol envelope again before touching
/// it and returns a small transport status; detailed failures are encoded in
/// the response header.
#[unsafe(no_mangle)]
pub extern "C" fn astrid_compute_run(
    worker_index: i32,
    descriptor_offset: i64,
    descriptor_length: i64,
    descriptor_tag: i64,
) -> i32 {
    let Ok(worker_index) = usize::try_from(worker_index) else {
        return -1;
    };
    let Ok(offset) = usize::try_from(descriptor_offset) else {
        return -1;
    };
    let Ok(length) = usize::try_from(descriptor_length) else {
        return -1;
    };
    let Some(expected_offset) = protocol::control_offset(worker_index) else {
        return -1;
    };
    if u64::try_from(offset).ok() != Some(expected_offset)
        || !(protocol::HEADER_BYTES..=protocol::CONTROL_BYTES).contains(&length)
    {
        return -1;
    }

    // SAFETY: Astrid validates `offset + length` against the imported shared
    // memory before dispatch. wasm32 pointers are offsets into that same memory,
    // and this invocation is the sole owner of the descriptor region.
    let bytes = unsafe { std::slice::from_raw_parts_mut(offset as *mut u8, length) };
    dispatch_worker(worker_index as u32, bytes, descriptor_tag)
}

#[cfg(test)]
fn dispatch(bytes: &mut [u8], descriptor_tag: i64) -> i32 {
    dispatch_worker(0, bytes, descriptor_tag)
}

fn dispatch_worker(worker_index: u32, bytes: &mut [u8], descriptor_tag: i64) -> i32 {
    if protocol::read_u32(bytes, field::MAGIC) != Some(protocol::MAGIC)
        || protocol::read_u32(bytes, field::VERSION) != Some(protocol::VERSION)
    {
        write_failure(
            bytes,
            Status::Invalid,
            "invalid Linux vCPU protocol envelope",
        );
        return 0;
    }
    clear_response(bytes);
    let operation = protocol::read_u32(bytes, field::OPERATION)
        .and_then(|value| Operation::try_from(value).ok());
    if operation.is_some_and(|operation| descriptor_tag != i64::from(operation as u32)) {
        write_failure(
            bytes,
            Status::Invalid,
            "descriptor tag does not match Linux vCPU operation",
        );
        return 0;
    }
    let result = match operation {
        Some(Operation::ParallelProbe) => parallel_probe(worker_index, bytes),
        Some(Operation::RunHartSlice) => run_slice(Some(worker_index), bytes),
        Some(_) if worker_index != 0 => {
            Err(invalid("machine control operations require worker zero"))
        }
        Some(Operation::InitCold) => initialize(bytes),
        Some(Operation::InitCheckpoint) => initialize_checkpoint(bytes),
        Some(Operation::RunSlice) => run_slice(None, bytes),
        Some(Operation::PushConsole) => push_console(bytes),
        Some(Operation::Complete9p) => complete_9p(bytes),
        Some(Operation::Fail9p) => fail_9p(bytes),
        Some(Operation::Reset) if input(bytes).is_ok_and(<[u8]>::is_empty) => {
            *STATE.lock().expect("worker state lock") = None;
            Ok(())
        }
        Some(Operation::Reset) => Err(invalid("reset does not accept an input payload")),
        None => Err((Status::Invalid, "unknown Linux vCPU operation".to_string())),
    };
    if let Err((status, error)) = result {
        write_failure(bytes, status, &error);
    }
    0
}

type WorkerResult = Result<(), (Status, String)>;

fn parallel_probe(worker_index: u32, bytes: &mut [u8]) -> WorkerResult {
    if !input(bytes)?.is_empty() {
        return Err(invalid("parallel probe does not accept an input payload"));
    }
    let workers = protocol::read_u32(bytes, field::HART_COUNT).unwrap_or_default();
    if !(2..=protocol::MAX_WORKER_STACKS as u32).contains(&workers) || worker_index >= workers {
        return Err(invalid(
            "parallel probe worker count or identity is outside the admitted group",
        ));
    }

    let expected = if workers == 64 {
        u64::MAX
    } else {
        (1_u64 << workers) - 1
    };
    let own_bit = 1_u64 << worker_index;
    let previous = PARALLEL_PROBE_ARRIVALS.fetch_or(own_bit, Ordering::AcqRel);
    if previous & own_bit != 0 {
        return Err(invalid("parallel probe worker identity arrived twice"));
    }

    // The bounded barrier is deliberate: a serial worker implementation
    // cannot satisfy it and therefore cannot accidentally pass this proof.
    let mut observed = previous | own_bit;
    for _ in 0..10_000_000_u64 {
        observed = PARALLEL_PROBE_ARRIVALS.load(Ordering::Acquire);
        if observed & expected == expected {
            break;
        }
        std::hint::spin_loop();
    }
    if observed & expected != expected {
        return Err(machine_error(
            "parallel probe did not observe concurrent workers",
        ));
    }

    protocol::write_u32(bytes, field::HART_COUNT, worker_index);
    Ok(())
}

fn initialize(bytes: &mut [u8]) -> WorkerResult {
    if STATE.lock().expect("worker state lock").is_some() {
        return Err(invalid("Linux vCPU is already initialized"));
    }
    let boot_input = input(bytes)?;
    if boot_input.len() != protocol::COLD_BOOT_INPUT_BYTES {
        return Err(invalid("cold boot input must contain wall-clock seconds"));
    }
    let wall_time_seconds = u64::from_le_bytes(
        boot_input
            .try_into()
            .map_err(|_| invalid("cold boot wall clock is malformed"))?,
    );
    if wall_time_seconds == 0 || wall_time_seconds > i64::MAX as u64 {
        return Err(invalid("cold boot wall clock is outside the Linux range"));
    }
    let ram_bytes =
        usize::try_from(protocol::read_u64(bytes, field::RAM_BYTES).unwrap_or_default())
            .map_err(|_| invalid("RAM size is not addressable"))?;
    let max_console_bytes =
        usize::try_from(protocol::read_u64(bytes, field::MAX_CONSOLE_BYTES).unwrap_or_default())
            .map_err(|_| invalid("console size is not addressable"))?;
    let hart_count =
        usize::try_from(protocol::read_u32(bytes, field::HART_COUNT).unwrap_or_default())
            .map_err(|_| invalid("hart count is not addressable"))?;
    if !(1..=aos_realm_machine::MAX_HARTS).contains(&hart_count) {
        return Err(invalid("hart count must be between 1 and 64"));
    }
    let mut machine = Machine::new_with_harts(
        MachineConfig {
            ram_bytes,
            max_console_bytes,
        },
        hart_count,
    )
    .map_err(|error| machine_error(error.to_string()))?;
    let linux_image = load_linux_image()?;
    let system_bytes = system_asset_size()?;
    let bootargs = format!(
        "earlycon=sbi console=hvc0 init=/init panic=-1 aos.wall_time={wall_time_seconds} aos.system_bytes={system_bytes}"
    );
    machine
        .boot_linux(&linux_image, &[], &bootargs)
        .map_err(|error| machine_error(error.to_string()))?;
    *STATE.lock().expect("worker state lock") = Some(WorkerState {
        machine,
        pending_request: None,
    });
    Ok(())
}

fn initialize_checkpoint(bytes: &mut [u8]) -> WorkerResult {
    if STATE.lock().expect("worker state lock").is_some() {
        return Err(invalid("Linux vCPU is already initialized"));
    }
    let checkpoint_input = input(bytes)?;
    if checkpoint_input.len() != protocol::CHECKPOINT_BINDING_BYTES {
        return Err(invalid(
            "checkpoint init must contain the kernel and system digests",
        ));
    }
    let linux_image = checkpoint_input[..32]
        .try_into()
        .map(CheckpointDigest::new)
        .map_err(|_| invalid("checkpoint kernel digest is malformed"))?;
    let immutable_system = checkpoint_input[32..]
        .try_into()
        .map(CheckpointDigest::new)
        .map_err(|_| invalid("checkpoint system digest is malformed"))?;
    let binding = CheckpointBinding::new(linux_image, immutable_system);
    let checkpoint_bytes = load_checkpoint()?;
    let checkpoint = MachineCheckpoint::decode(&checkpoint_bytes, binding)
        .map_err(|error| machine_error(format!("checkpoint admission failed: {error}")))?;
    let ram_bytes =
        usize::try_from(protocol::read_u64(bytes, field::RAM_BYTES).unwrap_or_default())
            .map_err(|_| invalid("RAM size is not addressable"))?;
    let max_console_bytes =
        usize::try_from(protocol::read_u64(bytes, field::MAX_CONSOLE_BYTES).unwrap_or_default())
            .map_err(|_| invalid("console size is not addressable"))?;
    let hart_count =
        usize::try_from(protocol::read_u32(bytes, field::HART_COUNT).unwrap_or_default())
            .map_err(|_| invalid("hart count is not addressable"))?;
    if checkpoint.ram_bytes() != ram_bytes
        || checkpoint.max_console_bytes() != max_console_bytes
        || checkpoint.hart_count() != hart_count
    {
        return Err(machine_error(
            "checkpoint resources do not match the admitted Linux envelope",
        ));
    }
    let pending_request = Some(checkpoint.pending_host_request().id);
    let machine = checkpoint.into_machine();
    *STATE.lock().expect("worker state lock") = Some(WorkerState {
        machine,
        pending_request,
    });
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn load_linux_image() -> Result<Vec<u8>, (Status, String)> {
    load_asset(
        LINUX_KERNEL_ASSET_INDEX,
        MAX_LINUX_KERNEL_BYTES,
        "Linux kernel",
    )
}

#[cfg(target_arch = "wasm32")]
fn load_checkpoint() -> Result<Vec<u8>, (Status, String)> {
    load_asset(
        LINUX_CHECKPOINT_ASSET_INDEX,
        MAX_LINUX_CHECKPOINT_BYTES,
        "Linux checkpoint",
    )
}

#[cfg(target_arch = "wasm32")]
fn read_system_block(offset: u64, length: usize) -> Result<Vec<u8>, (Status, String)> {
    let system_bytes = system_asset_size()?;
    let offset =
        usize::try_from(offset).map_err(|_| invalid("Linux system block offset is too large"))?;
    offset
        .checked_add(length)
        .filter(|end| *end <= system_bytes)
        .ok_or_else(|| invalid("Linux system block read exceeds the immutable asset"))?;
    let mut response = vec![0_u8; length];
    let destination = i64::try_from(response.as_mut_ptr() as usize)
        .map_err(|_| machine_error("Linux system block destination is not addressable"))?;
    let offset = i64::try_from(offset)
        .map_err(|_| machine_error("Linux system block offset is not addressable"))?;
    let length = i64::try_from(length)
        .map_err(|_| machine_error("Linux system block length is not addressable"))?;
    // SAFETY: `response` is a live allocation of exactly `length` bytes and
    // the source range was checked against the immutable asset size above.
    let status = unsafe { host_asset_read(LINUX_SYSTEM_ASSET_INDEX, offset, destination, length) };
    if status != ASSET_OK {
        return Err(machine_error(format!(
            "Linux system block read failed with status {status}"
        )));
    }
    Ok(response)
}

#[cfg(target_arch = "wasm32")]
fn system_asset_size() -> Result<usize, (Status, String)> {
    // SAFETY: the attached asset list is admitted by Astrid before this signed
    // worker starts. The index names the hash-pinned immutable system image.
    let asset_count = unsafe { host_asset_count() };
    if asset_count <= LINUX_SYSTEM_ASSET_INDEX {
        return Err(machine_error("Linux system asset is not attached"));
    }
    // SAFETY: the asset index was admitted above.
    let asset_size = unsafe { host_asset_size(LINUX_SYSTEM_ASSET_INDEX) };
    admitted_asset_size(asset_size, MAX_LINUX_SYSTEM_BYTES)
        .filter(|bytes| bytes.is_multiple_of(512))
        .ok_or_else(|| machine_error("Linux system asset size is outside the admitted range"))
}

#[cfg(not(target_arch = "wasm32"))]
fn read_system_block(_offset: u64, _length: usize) -> Result<Vec<u8>, (Status, String)> {
    Err(machine_error(
        "Linux system asset reads require the signed compute worker",
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn system_asset_size() -> Result<usize, (Status, String)> {
    Ok(4096)
}

#[cfg(target_arch = "wasm32")]
fn load_asset(
    asset_index: i32,
    max_asset_bytes: usize,
    asset_name: &str,
) -> Result<Vec<u8>, (Status, String)> {
    // SAFETY: these exact imports are validated and linked by Astrid before the
    // worker can start. They expose no path and read only the hash-bound asset
    // list attached to this signed compute worker.
    let asset_count = unsafe { host_asset_count() };
    if asset_count <= asset_index {
        return Err(machine_error(format!("{asset_name} asset is not attached")));
    }
    // SAFETY: the index was admitted above; a negative status still fails
    // closed before allocation.
    let asset_size = unsafe { host_asset_size(asset_index) };
    let size = admitted_asset_size(asset_size, max_asset_bytes).ok_or_else(|| {
        machine_error(format!(
            "{asset_name} asset size is outside the admitted range"
        ))
    })?;
    let mut image = Vec::new();
    image
        .try_reserve_exact(size)
        .map_err(|_| machine_error("Linux kernel asset allocation was denied"))?;
    image.resize(size, 0);
    for offset in (0..size).step_by(MAX_ASSET_READ_BYTES) {
        let length = (size - offset).min(MAX_ASSET_READ_BYTES);
        let destination = i64::try_from(image[offset..].as_mut_ptr() as usize)
            .map_err(|_| machine_error("Linux kernel destination is not addressable"))?;
        let offset = i64::try_from(offset)
            .map_err(|_| machine_error("Linux kernel offset is not addressable"))?;
        let length = i64::try_from(length)
            .map_err(|_| machine_error("Linux kernel chunk is not addressable"))?;
        // SAFETY: `destination..destination+length` is the live mutable chunk
        // in `image`; Astrid bounds the source and destination and returns only
        // after the atomic copy has completed.
        let status = unsafe { host_asset_read(asset_index, offset, destination, length) };
        if status != ASSET_OK {
            return Err(machine_error(format!(
                "{asset_name} asset read failed with status {status}"
            )));
        }
    }
    Ok(image)
}

#[cfg(test)]
fn admitted_linux_kernel_size(asset_size: i64) -> Option<usize> {
    admitted_asset_size(asset_size, MAX_LINUX_KERNEL_BYTES)
}

#[cfg(any(target_arch = "wasm32", test))]
fn admitted_asset_size(asset_size: i64, max_asset_bytes: usize) -> Option<usize> {
    usize::try_from(asset_size)
        .ok()
        .filter(|size| (1..=max_asset_bytes).contains(size))
}

#[cfg(not(target_arch = "wasm32"))]
fn load_linux_image() -> Result<Vec<u8>, (Status, String)> {
    Ok(include_bytes!("../../../assets/linux-kernel.img").to_vec())
}

#[cfg(not(target_arch = "wasm32"))]
fn load_checkpoint() -> Result<Vec<u8>, (Status, String)> {
    Ok(include_bytes!("../../../assets/linux-prewarm-1g-1h.aos-machine").to_vec())
}

fn run_slice(worker_index: Option<u32>, bytes: &mut [u8]) -> WorkerResult {
    if !input(bytes)?.is_empty() {
        return Err(invalid("run slice does not accept an input payload"));
    }
    if let Some(worker_index) = worker_index {
        let hart_id = protocol::read_u32(bytes, field::HART_ID).unwrap_or(u32::MAX);
        if hart_id != worker_index {
            return Err(invalid(
                "exact hart identity does not match the runtime worker",
            ));
        }
    }
    let budget = protocol::read_u64(bytes, field::SLICE_BUDGET).unwrap_or_default();
    if !(1..=protocol::MAX_SLICE_STEPS).contains(&budget) {
        return Err(invalid("slice budget is outside the admitted range"));
    }
    let mut state = STATE.lock().expect("worker state lock");
    let state = state.as_mut().ok_or_else(|| {
        (
            Status::NotInitialized,
            "Linux vCPU is not initialized".to_string(),
        )
    })?;
    let mut steps_executed = 0_u64;
    let mut instructions_retired = 0_u64;
    let mut console = Vec::new();
    loop {
        let report = if let Some(worker_index) = worker_index {
            state
                .machine
                .run_hart_slice(worker_index as usize, budget - steps_executed)
                .map_err(|error| machine_error(error.to_string()))?
        } else {
            state.machine.run_slice(budget - steps_executed)
        };
        steps_executed = steps_executed.saturating_add(report.steps_executed);
        instructions_retired = instructions_retired.saturating_add(report.instructions_retired);
        console.extend_from_slice(&report.console);
        protocol::write_u64(bytes, field::STEPS_EXECUTED, steps_executed);
        protocol::write_u64(
            bytes,
            field::TOTAL_STEPS_EXECUTED,
            report.total_steps_executed,
        );
        protocol::write_u64(bytes, field::INSTRUCTIONS_RETIRED, instructions_retired);
        protocol::write_u64(
            bytes,
            field::TOTAL_INSTRUCTIONS_RETIRED,
            report.total_instructions_retired,
        );

        if let SliceOutcome::HostRequest(request) = &report.outcome
            && request.channel == LINUX_SYSTEM_BLOCK_CHANNEL
        {
            complete_system_block_request(&mut state.machine, request)?;
            if steps_executed < budget {
                continue;
            }
            protocol::write_u32(bytes, field::OUTCOME, Outcome::Yielded as u32);
            return encode_payload(bytes, &console, &[], &[]);
        }

        let mut message = &[][..];
        let mut error = String::new();
        match &report.outcome {
            SliceOutcome::Yielded => {
                protocol::write_u32(bytes, field::OUTCOME, Outcome::Yielded as u32);
            }
            SliceOutcome::Halted(HaltStatus { passed, code }) => {
                protocol::write_u32(bytes, field::OUTCOME, Outcome::Halted as u32);
                protocol::write_u32(bytes, field::HALT_CODE, *code);
                protocol::write_u32(bytes, field::HALT_PASSED, u32::from(*passed));
            }
            SliceOutcome::HostRequest(request) => {
                protocol::write_u32(bytes, field::OUTCOME, Outcome::HostRequest as u32);
                protocol::write_u64(bytes, field::REQUEST_ID, request.id.get());
                protocol::write_u32(bytes, field::REQUEST_CHANNEL, request.channel);
                protocol::write_u32(
                    bytes,
                    field::MAX_RESPONSE_BYTES,
                    u32::try_from(request.max_response_bytes).unwrap_or(u32::MAX),
                );
                message = &request.message;
                state.pending_request = Some(request.id);
            }
            SliceOutcome::Trapped(trap) => {
                protocol::write_u32(bytes, field::OUTCOME, Outcome::Trapped as u32);
                error = trap.to_string();
            }
        }
        return encode_payload(bytes, &console, message, error.as_bytes());
    }
}

fn complete_system_block_request(
    machine: &mut Machine,
    request: &aos_realm_machine::Plan9Request,
) -> WorkerResult {
    let offset = system_block_offset(&request.message, request.max_response_bytes)?;
    let response = read_system_block(offset, request.max_response_bytes)?;
    machine
        .complete_9p_request(request.id, &response)
        .map_err(|error| machine_error(format!("complete Linux system block read: {error}")))
}

fn system_block_offset(message: &[u8], response_bytes: usize) -> Result<u64, (Status, String)> {
    if !(LINUX_SYSTEM_SECTOR_BYTES..=aos_realm_machine::MAX_9P_MESSAGE_BYTES)
        .contains(&response_bytes)
        || !response_bytes.is_multiple_of(LINUX_SYSTEM_SECTOR_BYTES)
    {
        return Err(invalid(
            "Linux system block read length is outside the admitted range",
        ));
    }
    let offset = message
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| invalid("Linux system block request has an invalid offset"))?;
    if !offset.is_multiple_of(LINUX_SYSTEM_SECTOR_BYTES as u64) {
        return Err(invalid("Linux system block offset is not sector aligned"));
    }
    Ok(offset)
}

fn push_console(bytes: &mut [u8]) -> WorkerResult {
    let input = input(bytes)?.to_vec();
    if input.len() > protocol::MAX_CONSOLE_INPUT_BYTES {
        return Err(invalid("console input exceeds the per-descriptor limit"));
    }
    let mut state = STATE.lock().expect("worker state lock");
    state
        .as_mut()
        .ok_or_else(|| {
            (
                Status::NotInitialized,
                "Linux vCPU is not initialized".to_string(),
            )
        })?
        .machine
        .push_console_input(&input);
    Ok(())
}

fn complete_9p(bytes: &mut [u8]) -> WorkerResult {
    let request_id = protocol::read_u64(bytes, field::REQUEST_ID).unwrap_or_default();
    let response = input(bytes)?.to_vec();
    let mut state = STATE.lock().expect("worker state lock");
    let state = state.as_mut().ok_or_else(|| {
        (
            Status::NotInitialized,
            "Linux vCPU is not initialized".to_string(),
        )
    })?;
    let id = pending_request(state, request_id)?;
    state
        .machine
        .complete_9p_request(id, &response)
        .map_err(|error| machine_error(error.to_string()))?;
    state.pending_request = None;
    Ok(())
}

fn fail_9p(bytes: &mut [u8]) -> WorkerResult {
    if !input(bytes)?.is_empty() {
        return Err(invalid("9P failure does not accept an input payload"));
    }
    let request_id = protocol::read_u64(bytes, field::REQUEST_ID).unwrap_or_default();
    let failure = match protocol::read_u32(bytes, field::REQUEST_FAILURE).unwrap_or_default() {
        value if value == RequestFailure::Denied as u32 => HostRequestFailure::Denied,
        value if value == RequestFailure::Failed as u32 => HostRequestFailure::Failed,
        _ => return Err(invalid("unknown 9P failure code")),
    };
    let mut state = STATE.lock().expect("worker state lock");
    let state = state.as_mut().ok_or_else(|| {
        (
            Status::NotInitialized,
            "Linux vCPU is not initialized".to_string(),
        )
    })?;
    let id = pending_request(state, request_id)?;
    state
        .machine
        .fail_9p_request(id, failure)
        .map_err(|error| machine_error(error.to_string()))?;
    state.pending_request = None;
    Ok(())
}

fn pending_request(state: &WorkerState, raw: u64) -> Result<HostRequestId, (Status, String)> {
    let Some(id) = state.pending_request else {
        return Err((
            Status::RequestMismatch,
            "Linux vCPU has no pending 9P request".to_string(),
        ));
    };
    if id.get() != raw {
        return Err((
            Status::RequestMismatch,
            "Linux vCPU 9P request identity mismatch".to_string(),
        ));
    }
    Ok(id)
}

fn input(bytes: &[u8]) -> Result<&[u8], (Status, String)> {
    let length = usize::try_from(protocol::read_u32(bytes, field::INPUT_LEN).unwrap_or_default())
        .map_err(|_| invalid("input length is not addressable"))?;
    bytes
        .get(protocol::HEADER_BYTES..protocol::HEADER_BYTES.saturating_add(length))
        .ok_or_else(|| invalid("input extends beyond the descriptor"))
}

fn clear_response(bytes: &mut [u8]) {
    protocol::write_u32(bytes, field::STATUS, Status::Ok as u32);
    protocol::write_u32(bytes, field::RESPONSE_LEN, 0);
    protocol::write_u32(bytes, field::OUTCOME, Outcome::None as u32);
    for offset in [
        field::STEPS_EXECUTED,
        field::TOTAL_STEPS_EXECUTED,
        field::INSTRUCTIONS_RETIRED,
        field::TOTAL_INSTRUCTIONS_RETIRED,
    ] {
        protocol::write_u64(bytes, offset, 0);
    }
    for offset in [
        field::HALT_CODE,
        field::HALT_PASSED,
        field::REQUEST_CHANNEL,
        field::MAX_RESPONSE_BYTES,
        field::CONSOLE_LEN,
        field::MESSAGE_LEN,
        field::ERROR_LEN,
    ] {
        protocol::write_u32(bytes, offset, 0);
    }
}

fn encode_payload(bytes: &mut [u8], console: &[u8], message: &[u8], error: &[u8]) -> WorkerResult {
    let total = console
        .len()
        .checked_add(message.len())
        .and_then(|value| value.checked_add(error.len()))
        .ok_or_else(|| invalid("response payload length overflow"))?;
    let end = protocol::HEADER_BYTES
        .checked_add(total)
        .ok_or_else(|| invalid("response payload range overflow"))?;
    let Some(payload) = bytes.get_mut(protocol::HEADER_BYTES..end) else {
        return Err(invalid("response exceeds the descriptor"));
    };
    let (console_out, rest) = payload.split_at_mut(console.len());
    let (message_out, error_out) = rest.split_at_mut(message.len());
    console_out.copy_from_slice(console);
    message_out.copy_from_slice(message);
    error_out.copy_from_slice(error);
    protocol::write_u32(
        bytes,
        field::CONSOLE_LEN,
        u32::try_from(console.len()).unwrap_or(u32::MAX),
    );
    protocol::write_u32(
        bytes,
        field::MESSAGE_LEN,
        u32::try_from(message.len()).unwrap_or(u32::MAX),
    );
    protocol::write_u32(
        bytes,
        field::ERROR_LEN,
        u32::try_from(error.len()).unwrap_or(u32::MAX),
    );
    protocol::write_u32(
        bytes,
        field::RESPONSE_LEN,
        u32::try_from(total).unwrap_or(u32::MAX),
    );
    Ok(())
}

fn write_failure(bytes: &mut [u8], status: Status, error: &str) {
    protocol::write_u32(bytes, field::STATUS, status as u32);
    protocol::write_u32(bytes, field::OUTCOME, Outcome::None as u32);
    let available = bytes.len().saturating_sub(protocol::HEADER_BYTES);
    let error = error.as_bytes();
    let error = &error[..error.len().min(available)];
    if let Some(output) =
        bytes.get_mut(protocol::HEADER_BYTES..protocol::HEADER_BYTES.saturating_add(error.len()))
    {
        output.copy_from_slice(error);
    }
    protocol::write_u32(bytes, field::CONSOLE_LEN, 0);
    protocol::write_u32(bytes, field::MESSAGE_LEN, 0);
    protocol::write_u32(
        bytes,
        field::ERROR_LEN,
        u32::try_from(error.len()).unwrap_or(u32::MAX),
    );
    protocol::write_u32(
        bytes,
        field::RESPONSE_LEN,
        u32::try_from(error.len()).unwrap_or(u32::MAX),
    );
}

fn invalid(message: &str) -> (Status, String) {
    (Status::Invalid, message.to_string())
}

fn machine_error(message: impl Into<String>) -> (Status, String) {
    (Status::Machine, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNED_WORKER_HASH: &str =
        "blake3:601923e6397749715b4bc16c527035b1fceb6c7781ed7658c4acef06418320d9";
    const SIGNED_KERNEL_HASH: &str =
        "blake3:60cc6c3c01222a3a33b108593974de5636747b32cacc10bf8c0f45c1cdd8b285";
    const SIGNED_TEST_SYSTEM_HASH: &str =
        "blake3:0ba0ad681b5d03178bfc8b3c308e67164c8daf3301386ca573a6838945078aad";
    const SIGNED_SYSTEM_HASH: &str =
        "blake3:c436bb2bfe0941f183f58f0e2e56df05a4bc03f01147ad1d095f48df0004afaa";
    const SIGNED_CHECKPOINT_HASH: &str =
        "blake3:59c7a2cb08f4fec01d6bd0ba1351a9c7d4b3fa3eecff7578ed9bbfc6229835fd";

    fn digest(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(std::str::from_utf8(pair).expect("digest pair"), 16)
                    .expect("lowercase digest")
            })
            .collect()
    }

    fn request(operation: u32, input: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0_u8; protocol::HEADER_BYTES + input.len()];
        protocol::write_u32(&mut bytes, field::MAGIC, protocol::MAGIC);
        protocol::write_u32(&mut bytes, field::VERSION, protocol::VERSION);
        protocol::write_u32(&mut bytes, field::OPERATION, operation);
        protocol::write_u32(&mut bytes, field::INPUT_LEN, input.len() as u32);
        bytes[protocol::HEADER_BYTES..].copy_from_slice(input);
        bytes
    }

    fn assert_bounded_response(bytes: &[u8], expected_status: Status) {
        assert_eq!(
            protocol::read_u32(bytes, field::STATUS),
            Some(expected_status as u32)
        );
        let response = protocol::read_u32(bytes, field::RESPONSE_LEN).expect("response length");
        let console = protocol::read_u32(bytes, field::CONSOLE_LEN).expect("console length");
        let message = protocol::read_u32(bytes, field::MESSAGE_LEN).expect("message length");
        let error = protocol::read_u32(bytes, field::ERROR_LEN).expect("error length");
        assert_eq!(response, console + message + error);
        assert!(response as usize <= bytes.len() - protocol::HEADER_BYTES);
    }

    #[test]
    fn protocol_fields_round_trip() {
        let mut bytes = [0_u8; protocol::HEADER_BYTES];
        protocol::write_u32(&mut bytes, field::MAGIC, protocol::MAGIC);
        protocol::write_u64(&mut bytes, field::RAM_BYTES, 32 * 1024 * 1024);
        assert_eq!(
            protocol::read_u32(&bytes, field::MAGIC),
            Some(protocol::MAGIC)
        );
        assert_eq!(
            protocol::read_u64(&bytes, field::RAM_BYTES),
            Some(32 * 1024 * 1024)
        );
    }

    #[test]
    fn payload_layout_is_bounded_and_ordered() {
        let mut bytes = [0_u8; 256];
        encode_payload(&mut bytes, b"console", b"request", b"trap").expect("payload fits");
        assert_eq!(protocol::read_u32(&bytes, field::CONSOLE_LEN), Some(7));
        assert_eq!(protocol::read_u32(&bytes, field::MESSAGE_LEN), Some(7));
        assert_eq!(protocol::read_u32(&bytes, field::ERROR_LEN), Some(4));
        assert_eq!(
            &bytes[protocol::HEADER_BYTES..protocol::HEADER_BYTES + 18],
            b"consolerequesttrap"
        );
    }

    #[test]
    fn malformed_and_out_of_state_descriptors_fail_closed() {
        *STATE.lock().expect("worker state lock") = None;

        let mut bytes = request(Operation::Reset as u32, &[]);
        assert_eq!(dispatch(&mut bytes, Operation::RunSlice as i64), 0);
        assert_bounded_response(&bytes, Status::Invalid);

        let mut bytes = request(99, &[]);
        assert_eq!(dispatch(&mut bytes, 99), 0);
        assert_bounded_response(&bytes, Status::Invalid);

        let mut bytes = request(Operation::RunSlice as u32, b"unexpected");
        protocol::write_u64(&mut bytes, field::SLICE_BUDGET, 1);
        assert_eq!(dispatch(&mut bytes, Operation::RunSlice as i64), 0);
        assert_bounded_response(&bytes, Status::Invalid);

        for budget in [0, protocol::MAX_SLICE_STEPS + 1] {
            let mut bytes = request(Operation::RunSlice as u32, &[]);
            protocol::write_u64(&mut bytes, field::SLICE_BUDGET, budget);
            assert_eq!(dispatch(&mut bytes, Operation::RunSlice as i64), 0);
            assert_bounded_response(&bytes, Status::Invalid);
        }

        let mut bytes = request(Operation::RunHartSlice as u32, &[]);
        protocol::write_u64(&mut bytes, field::SLICE_BUDGET, 1);
        protocol::write_u32(&mut bytes, field::HART_ID, 0);
        assert_eq!(
            dispatch_worker(1, &mut bytes, Operation::RunHartSlice as i64),
            0
        );
        assert_bounded_response(&bytes, Status::Invalid);

        let mut bytes = request(Operation::RunHartSlice as u32, &[]);
        protocol::write_u64(&mut bytes, field::SLICE_BUDGET, 1);
        protocol::write_u32(&mut bytes, field::HART_ID, 1);
        assert_eq!(
            dispatch_worker(1, &mut bytes, Operation::RunHartSlice as i64),
            0
        );
        assert_bounded_response(&bytes, Status::NotInitialized);

        let mut bytes = request(
            Operation::PushConsole as u32,
            &vec![0; protocol::MAX_CONSOLE_INPUT_BYTES + 1],
        );
        assert_eq!(dispatch(&mut bytes, Operation::PushConsole as i64), 0);
        assert_bounded_response(&bytes, Status::Invalid);

        let mut bytes = request(Operation::Complete9p as u32, &[]);
        protocol::write_u64(&mut bytes, field::REQUEST_ID, 1);
        assert_eq!(dispatch(&mut bytes, Operation::Complete9p as i64), 0);
        assert_bounded_response(&bytes, Status::NotInitialized);

        let mut bytes = request(Operation::Fail9p as u32, b"unexpected");
        protocol::write_u32(
            &mut bytes,
            field::REQUEST_FAILURE,
            RequestFailure::Denied as u32,
        );
        assert_eq!(dispatch(&mut bytes, Operation::Fail9p as i64), 0);
        assert_bounded_response(&bytes, Status::Invalid);

        let mut bytes = request(Operation::Reset as u32, b"unexpected");
        assert_eq!(dispatch(&mut bytes, Operation::Reset as i64), 0);
        assert_bounded_response(&bytes, Status::Invalid);

        let mut bytes = request(Operation::Reset as u32, &[]);
        assert_eq!(dispatch(&mut bytes, Operation::Reset as i64), 0);
        assert_bounded_response(&bytes, Status::Ok);

        let mut bytes = request(Operation::Reset as u32, &[]);
        assert_eq!(dispatch_worker(1, &mut bytes, Operation::Reset as i64), 0);
        assert_bounded_response(&bytes, Status::Invalid);

        let mut binding =
            digest("60cc6c3c01222a3a33b108593974de5636747b32cacc10bf8c0f45c1cdd8b285");
        binding.extend(digest(
            "c436bb2bfe0941f183f58f0e2e56df05a4bc03f01147ad1d095f48df0004afaa",
        ));
        let mut bytes = request(Operation::InitCheckpoint as u32, &binding);
        protocol::write_u64(&mut bytes, field::RAM_BYTES, 1024 * 1024 * 1024);
        protocol::write_u64(&mut bytes, field::MAX_CONSOLE_BYTES, 64 * 1024);
        protocol::write_u32(&mut bytes, field::HART_COUNT, 1);
        assert_eq!(dispatch(&mut bytes, Operation::InitCheckpoint as i64), 0);
        assert_bounded_response(&bytes, Status::Ok);
        {
            let state = STATE.lock().expect("worker state lock");
            let state = state.as_ref().expect("restored worker state");
            assert_eq!(state.machine.hart_count(), 1);
            assert!(state.pending_request.is_some());
        }
        let mut bytes = request(Operation::Reset as u32, &[]);
        assert_eq!(dispatch(&mut bytes, Operation::Reset as i64), 0);
        assert_bounded_response(&bytes, Status::Ok);
    }

    #[test]
    fn arbitrary_invalid_envelopes_never_escape_the_descriptor() {
        let mut state = 0x4d59_5df4_d0f3_3173_u64;
        for length in protocol::HEADER_BYTES..protocol::HEADER_BYTES + 257 {
            let mut bytes = vec![0_u8; length];
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state as u8;
            }
            // Force at least one envelope field invalid so fuzzing cannot
            // allocate a machine; all remaining bytes stay adversarial.
            protocol::write_u32(&mut bytes, field::MAGIC, protocol::MAGIC ^ 1);
            assert_eq!(dispatch(&mut bytes, i64::MAX), 0);
            assert_bounded_response(&bytes, Status::Invalid);
        }
    }

    #[test]
    fn transport_rejects_negative_worker_and_descriptor_geometry_before_dereference() {
        assert_eq!(astrid_compute_run(-1, 0, 0, 0), -1);
        assert_eq!(astrid_compute_run(1, 0, 0, 0), -1);
        assert_eq!(
            astrid_compute_run(0, -1, protocol::HEADER_BYTES as i64, 0),
            -1
        );
        assert_eq!(astrid_compute_run(0, 64, -1, 0), -1);
        assert_eq!(
            astrid_compute_run(0, 64, (protocol::HEADER_BYTES - 1) as i64, 0),
            -1
        );
        assert_eq!(
            astrid_compute_run(0, 64, (protocol::CONTROL_BYTES + 1) as i64, 0),
            -1
        );
    }

    #[test]
    fn signed_rust_workers_cross_a_real_parallel_compute_barrier() {
        use astrid_compute::{
            ComputeLedger, ComputeLimits, ComputeRuntime, ExecutionMode, GroupRequest, Parallelism,
            WorkDescriptor, WorkerArtifact,
        };
        use astrid_core::principal::PrincipalId;
        use std::path::Path;

        let capsule_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("capsule root");
        let artifact = WorkerArtifact::from_capsule_path(
            protocol::WORKER_ID,
            capsule_root,
            Path::new("assets/linux-vcpu.wasm"),
            SIGNED_WORKER_HASH,
        )
        .expect("signed Rust worker");
        let runtime = ComputeRuntime::new(ComputeLedger::default(), ComputeLimits::default())
            .expect("compute runtime");
        let group = runtime
            .open_group(
                &PrincipalId::new("linux-parallel-worker-conformance").expect("principal"),
                &artifact,
                GroupRequest {
                    mode: ExecutionMode::Parallel,
                    parallelism: Parallelism::Exact(2),
                    initial_memory_pages: 1024,
                    maximum_memory_pages: 2048,
                },
            )
            .expect("two-worker admission");

        let offsets = [
            protocol::control_offset(0).expect("worker zero descriptor"),
            protocol::control_offset(1).expect("worker one descriptor"),
        ];
        let mut jobs = Vec::new();
        for (worker_index, offset) in offsets.into_iter().enumerate() {
            let mut request = vec![0_u8; protocol::HEADER_BYTES];
            protocol::write_u32(&mut request, field::MAGIC, protocol::MAGIC);
            protocol::write_u32(&mut request, field::VERSION, protocol::VERSION);
            protocol::write_u32(
                &mut request,
                field::OPERATION,
                Operation::ParallelProbe as u32,
            );
            protocol::write_u32(&mut request, field::INPUT_LEN, 0);
            protocol::write_u32(&mut request, field::HART_COUNT, 2);
            group.write(offset, &request).expect("write probe");
            jobs.push(
                group
                    .submit(WorkDescriptor {
                        offset,
                        length: protocol::CONTROL_BYTES as u64,
                        tag: Operation::ParallelProbe as u64,
                        worker_index: Some(worker_index as u32),
                        fuel: None,
                    })
                    .expect("submit targeted probe"),
            );
        }

        for (worker_index, (job, offset)) in jobs.into_iter().zip(offsets).enumerate() {
            let result = job.join().expect("parallel probe completion");
            assert_eq!(result.worker_index, worker_index as u32);
            assert_eq!(result.worker_status, 0);
            let response = group
                .read(offset, protocol::HEADER_BYTES as u32)
                .expect("read probe response");
            let response_len =
                protocol::read_u32(&response, field::RESPONSE_LEN).unwrap_or_default();
            let detail = group
                .read(offset + protocol::HEADER_BYTES as u64, response_len)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            assert_eq!(
                protocol::read_u32(&response, field::STATUS),
                Some(Status::Ok as u32),
                "{detail}"
            );
            assert_eq!(
                protocol::read_u32(&response, field::HART_COUNT),
                Some(worker_index as u32)
            );
        }
        assert_eq!(group.parallelism(), 2);
        assert_eq!(group.mode(), ExecutionMode::Parallel);
    }

    #[test]
    fn linux_kernel_asset_size_is_positive_and_finitely_bounded() {
        assert_eq!(admitted_linux_kernel_size(-1), None);
        assert_eq!(admitted_linux_kernel_size(0), None);
        assert_eq!(admitted_linux_kernel_size(1), Some(1));
        assert_eq!(
            admitted_linux_kernel_size(MAX_LINUX_KERNEL_BYTES as i64),
            Some(MAX_LINUX_KERNEL_BYTES)
        );
        assert_eq!(
            admitted_linux_kernel_size(MAX_LINUX_KERNEL_BYTES as i64 + 1),
            None
        );
    }

    #[test]
    fn immutable_system_requests_admit_only_one_bounded_offset() {
        assert_eq!(
            system_block_offset(&4096_u64.to_le_bytes(), 4096).expect("page read"),
            4096
        );
        for malformed in [&[][..], &[0; 7], &[0; 9]] {
            assert_eq!(
                system_block_offset(malformed, 4096)
                    .expect_err("offset must be exactly eight bytes")
                    .0,
                Status::Invalid
            );
        }
        for response_bytes in [
            0,
            LINUX_SYSTEM_SECTOR_BYTES - 1,
            LINUX_SYSTEM_SECTOR_BYTES + 1,
            aos_realm_machine::MAX_9P_MESSAGE_BYTES + 1,
        ] {
            assert_eq!(
                system_block_offset(&0_u64.to_le_bytes(), response_bytes)
                    .expect_err("response range must stay machine-bounded")
                    .0,
                Status::Invalid
            );
        }
        assert_eq!(
            system_block_offset(&1_u64.to_le_bytes(), LINUX_SYSTEM_SECTOR_BYTES)
                .expect_err("offset must be sector aligned")
                .0,
            Status::Invalid
        );
    }

    #[test]
    fn signed_worker_restores_one_gib_prewarm_inside_the_real_compute_runtime() {
        use astrid_compute::{
            ComputeLedger, ComputeLimits, ComputeRuntime, ExecutionMode, GroupRequest, Parallelism,
            WorkDescriptor, WorkerArtifact, WorkerAssetSpec,
        };
        use astrid_core::principal::PrincipalId;
        use std::path::{Path, PathBuf};
        use std::time::Instant;

        let capsule_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("capsule root");
        let artifact = WorkerArtifact::from_capsule_path_with_assets(
            protocol::WORKER_ID,
            capsule_root,
            Path::new("assets/linux-vcpu.wasm"),
            SIGNED_WORKER_HASH,
            &[
                WorkerAssetSpec {
                    id: "linux-kernel".to_owned(),
                    relative_path: PathBuf::from("assets/linux-kernel.img"),
                    expected_hash: SIGNED_KERNEL_HASH.to_owned(),
                },
                WorkerAssetSpec {
                    id: "linux-system".to_owned(),
                    relative_path: PathBuf::from("assets/linux-system.squashfs"),
                    expected_hash: SIGNED_SYSTEM_HASH.to_owned(),
                },
                WorkerAssetSpec {
                    id: "linux-prewarm".to_owned(),
                    relative_path: PathBuf::from("assets/linux-prewarm-1g-1h.aos-machine"),
                    expected_hash: SIGNED_CHECKPOINT_HASH.to_owned(),
                },
            ],
        )
        .expect("signed worker and prewarm assets");
        let runtime = ComputeRuntime::new(ComputeLedger::default(), ComputeLimits::default())
            .expect("compute runtime");
        let started = Instant::now();
        let group = runtime
            .open_group(
                &PrincipalId::new("linux-prewarm-worker-conformance").expect("principal"),
                &artifact,
                GroupRequest {
                    mode: ExecutionMode::Deterministic,
                    parallelism: Parallelism::Exact(1),
                    initial_memory_pages: 1024,
                    maximum_memory_pages: 57_344,
                },
            )
            .expect("prewarm worker admission");
        let control_offset = protocol::control_offset(0).expect("worker zero descriptor");
        let descriptor = WorkDescriptor {
            offset: control_offset,
            length: protocol::CONTROL_BYTES as u64,
            tag: Operation::InitCheckpoint as u64,
            worker_index: None,
            fuel: None,
        };
        let mut binding =
            digest("60cc6c3c01222a3a33b108593974de5636747b32cacc10bf8c0f45c1cdd8b285");
        binding.extend(digest(
            "c436bb2bfe0941f183f58f0e2e56df05a4bc03f01147ad1d095f48df0004afaa",
        ));
        let mut request = request(Operation::InitCheckpoint as u32, &binding);
        protocol::write_u64(&mut request, field::RAM_BYTES, 1024 * 1024 * 1024);
        protocol::write_u64(&mut request, field::MAX_CONSOLE_BYTES, 64 * 1024);
        protocol::write_u32(&mut request, field::HART_COUNT, 1);
        group
            .write(control_offset, &request)
            .expect("write checkpoint init");
        group
            .submit(descriptor)
            .expect("submit checkpoint init")
            .join()
            .expect("restore signed prewarm");
        let header = group
            .read(control_offset, protocol::HEADER_BYTES as u32)
            .expect("read checkpoint response");
        assert_eq!(
            protocol::read_u32(&header, field::STATUS),
            Some(Status::Ok as u32)
        );
        let restore_elapsed = started.elapsed();

        let warm_started = Instant::now();
        protocol::write_u32(&mut request, field::OPERATION, Operation::RunSlice as u32);
        protocol::write_u32(&mut request, field::INPUT_LEN, 0);
        protocol::write_u64(&mut request, field::SLICE_BUDGET, 1);
        group.write(control_offset, &request).expect("write slice");
        group
            .submit(WorkDescriptor {
                tag: Operation::RunSlice as u64,
                ..descriptor
            })
            .expect("submit slice")
            .join()
            .expect("observe restored suspension");
        let header = group
            .read(control_offset, protocol::HEADER_BYTES as u32)
            .expect("read slice response");
        assert_eq!(
            protocol::read_u32(&header, field::STATUS),
            Some(Status::Ok as u32)
        );
        assert_eq!(
            protocol::read_u32(&header, field::OUTCOME),
            Some(Outcome::HostRequest as u32)
        );
        assert_eq!(protocol::read_u32(&header, field::REQUEST_CHANNEL), Some(1));
        assert_eq!(protocol::read_u64(&header, field::STEPS_EXECUTED), Some(0));
        let warm_elapsed = warm_started.elapsed();
        eprintln!(
            "signed 1 GiB prewarm restored in {:.3} ms; resident zero-step call in {:.3} ms",
            restore_elapsed.as_secs_f64() * 1_000.0,
            warm_elapsed.as_secs_f64() * 1_000.0
        );
    }

    #[test]
    fn signed_worker_cold_boots_two_linux_cpus_inside_the_real_compute_runtime() {
        use astrid_compute::{
            ComputeLedger, ComputeLimits, ComputeRuntime, ExecutionMode, GroupRequest, Parallelism,
            WorkDescriptor, WorkerArtifact, WorkerAssetSpec,
        };
        use astrid_core::principal::PrincipalId;
        use std::path::{Path, PathBuf};

        let capsule_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("capsule root");
        let test_root =
            std::env::temp_dir().join(format!("aos-realm-worker-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&test_root);
        std::fs::create_dir_all(test_root.join("assets")).expect("test asset directory");
        for (source, target) in [
            ("assets/linux-vcpu.wasm", "assets/linux-vcpu.wasm"),
            ("assets/linux-kernel.img", "assets/linux-kernel.img"),
            ("linux/rootfs.cpio", "assets/linux-system-test.img"),
        ] {
            std::fs::copy(capsule_root.join(source), test_root.join(target))
                .expect("copy signed test asset");
        }
        let artifact = WorkerArtifact::from_capsule_path_with_assets(
            protocol::WORKER_ID,
            &test_root,
            Path::new("assets/linux-vcpu.wasm"),
            SIGNED_WORKER_HASH,
            &[
                WorkerAssetSpec {
                    id: "linux-kernel".to_owned(),
                    relative_path: PathBuf::from("assets/linux-kernel.img"),
                    expected_hash: SIGNED_KERNEL_HASH.to_owned(),
                },
                WorkerAssetSpec {
                    id: "linux-system-test".to_owned(),
                    relative_path: PathBuf::from("assets/linux-system-test.img"),
                    expected_hash: SIGNED_TEST_SYSTEM_HASH.to_owned(),
                },
            ],
        )
        .expect("signed worker and kernel asset");
        let runtime = ComputeRuntime::new(ComputeLedger::default(), ComputeLimits::default())
            .expect("compute runtime");
        let group = runtime
            .open_group(
                &PrincipalId::new("linux-smp-worker-conformance").expect("principal"),
                &artifact,
                GroupRequest {
                    mode: ExecutionMode::Deterministic,
                    parallelism: Parallelism::Exact(1),
                    initial_memory_pages: 1024,
                    maximum_memory_pages: 2048,
                },
            )
            .expect("worker admission");
        let control_offset = protocol::control_offset(0).expect("worker zero descriptor");
        let descriptor = WorkDescriptor {
            offset: control_offset,
            length: protocol::CONTROL_BYTES as u64,
            tag: Operation::InitCold as u64,
            // The group has exactly one worker. Unspecified affinity exercises
            // the production queueing path without racing targeted-slot
            // bookkeeping immediately after `join`.
            worker_index: None,
            fuel: None,
        };
        let mut request = vec![0_u8; protocol::HEADER_BYTES + protocol::COLD_BOOT_INPUT_BYTES];
        protocol::write_u32(&mut request, field::MAGIC, protocol::MAGIC);
        protocol::write_u32(&mut request, field::VERSION, protocol::VERSION);
        protocol::write_u32(&mut request, field::OPERATION, Operation::InitCold as u32);
        protocol::write_u32(
            &mut request,
            field::INPUT_LEN,
            protocol::COLD_BOOT_INPUT_BYTES as u32,
        );
        protocol::write_u64(&mut request, field::RAM_BYTES, 32 * 1024 * 1024);
        protocol::write_u64(&mut request, field::MAX_CONSOLE_BYTES, 64 * 1024);
        protocol::write_u32(&mut request, field::HART_COUNT, 2);
        request[protocol::HEADER_BYTES..].copy_from_slice(&1_753_142_400_u64.to_le_bytes());
        group.write(control_offset, &request).expect("write init");
        group
            .submit(descriptor)
            .expect("submit init")
            .join()
            .expect("initialize two-hart Linux");

        let mut console = Vec::new();
        let total_steps = loop {
            protocol::write_u32(&mut request, field::OPERATION, Operation::RunSlice as u32);
            protocol::write_u32(&mut request, field::INPUT_LEN, 0);
            protocol::write_u64(&mut request, field::SLICE_BUDGET, 1_000_000);
            group.write(control_offset, &request).expect("write slice");
            group
                .submit(WorkDescriptor {
                    tag: Operation::RunSlice as u64,
                    ..descriptor
                })
                .expect("submit slice")
                .join()
                .expect("run two-hart Linux");
            let header = group
                .read(control_offset, protocol::HEADER_BYTES as u32)
                .expect("read slice response");
            assert_eq!(
                protocol::read_u32(&header, field::STATUS),
                Some(Status::Ok as u32)
            );
            let console_len =
                protocol::read_u32(&header, field::CONSOLE_LEN).expect("console length") as u32;
            if console_len != 0 {
                console.extend(
                    group
                        .read(control_offset + protocol::HEADER_BYTES as u64, console_len)
                        .expect("read Linux console"),
                );
            }
            let total_steps =
                protocol::read_u64(&header, field::TOTAL_STEPS_EXECUTED).expect("total steps");
            if console
                .windows(b"smp: Brought up 1 node, 2 CPUs".len())
                .any(|window| window == b"smp: Brought up 1 node, 2 CPUs")
            {
                break total_steps;
            }
            assert!(
                total_steps < 50_000_000,
                "Linux did not bring both CPUs online"
            );
        };

        let console = String::from_utf8_lossy(&console);
        assert!(console.contains("Linux version 6.18.39"), "{console}");
        assert!(
            console.contains("smp: Brought up 1 node, 2 CPUs"),
            "{console}"
        );
        assert!(
            (1..50_000_000).contains(&total_steps),
            "unexpected Linux SMP bring-up cost: {total_steps}"
        );
        std::fs::remove_dir_all(test_root).expect("remove test assets");
    }
}
