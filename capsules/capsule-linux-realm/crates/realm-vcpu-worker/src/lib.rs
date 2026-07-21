//! Stateful RV64 machine worker behind the Astrid generic-compute ABI.
//!
//! This core-Wasm module deliberately imports only
//! `astrid_compute.memory`. The controller owns every host effect and exchanges
//! bounded commands, slice reports, console bytes, and 9P messages through the
//! descriptor region in that shared memory.

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

const ABI_VERSION: i32 = 1;

const LINUX_IMAGE: &[u8] = include_bytes!("../../../linux/Image");
const LINUX_SOURCES: &[u8] = include_bytes!("../../../linux/SOURCES.lock");
const LINUX_PREWARM_32M: &[u8] = include_bytes!("../../../linux/prewarm-32m.aos-machine");

struct WorkerState {
    machine: Machine,
    pending_request: Option<HostRequestId>,
}

static STATE: Mutex<Option<WorkerState>> = Mutex::new(None);

/// Generic-compute worker ABI version.
#[unsafe(no_mangle)]
pub extern "C" fn astrid_compute_abi_version() -> i32 {
    ABI_VERSION
}

/// Execute one bounded vCPU control operation.
///
/// The runtime has already validated the descriptor range against shared
/// memory. This function validates the protocol envelope again before touching
/// it and returns a small transport status; detailed failures are encoded in
/// the response header.
#[unsafe(no_mangle)]
pub extern "C" fn astrid_compute_run(
    _worker_index: i32,
    descriptor_offset: i64,
    descriptor_length: i64,
    _descriptor_tag: i64,
) -> i32 {
    let Ok(offset) = usize::try_from(descriptor_offset) else {
        return -1;
    };
    let Ok(length) = usize::try_from(descriptor_length) else {
        return -1;
    };
    if offset < 64 || !(protocol::HEADER_BYTES..=protocol::CONTROL_BYTES).contains(&length) {
        return -1;
    }

    // SAFETY: Astrid validates `offset + length` against the imported shared
    // memory before dispatch. wasm32 pointers are offsets into that same memory,
    // and this invocation is the sole owner of the descriptor region.
    let bytes = unsafe { std::slice::from_raw_parts_mut(offset as *mut u8, length) };
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
    let result = match operation {
        Some(Operation::InitCold) => initialize(bytes, false),
        Some(Operation::InitPrewarm) => initialize(bytes, true),
        Some(Operation::RunSlice) => run_slice(bytes),
        Some(Operation::PushConsole) => push_console(bytes),
        Some(Operation::Complete9p) => complete_9p(bytes),
        Some(Operation::Fail9p) => fail_9p(bytes),
        Some(Operation::Reset) => {
            *STATE.lock().expect("worker state lock") = None;
            Ok(())
        }
        None => Err((Status::Invalid, "unknown Linux vCPU operation".to_string())),
    };
    if let Err((status, error)) = result {
        write_failure(bytes, status, &error);
    }
    0
}

type WorkerResult = Result<(), (Status, String)>;

fn initialize(bytes: &mut [u8], prewarm: bool) -> WorkerResult {
    let ram_bytes =
        usize::try_from(protocol::read_u64(bytes, field::RAM_BYTES).unwrap_or_default())
            .map_err(|_| invalid("RAM size is not addressable"))?;
    let max_console_bytes =
        usize::try_from(protocol::read_u64(bytes, field::MAX_CONSOLE_BYTES).unwrap_or_default())
            .map_err(|_| invalid("console size is not addressable"))?;
    let machine = if prewarm {
        let binding = CheckpointBinding::new(
            CheckpointDigest::hash(LINUX_IMAGE),
            CheckpointDigest::hash(LINUX_SOURCES),
        );
        let checkpoint = MachineCheckpoint::decode(LINUX_PREWARM_32M, binding)
            .map_err(|error| machine_error(error.to_string()))?;
        if checkpoint.ram_bytes() != ram_bytes
            || checkpoint.max_console_bytes() != max_console_bytes
        {
            return Err(machine_error(
                "prewarm checkpoint resources do not match admitted resources",
            ));
        }
        checkpoint.into_machine()
    } else {
        let mut machine = Machine::new(MachineConfig {
            ram_bytes,
            max_console_bytes,
        })
        .map_err(|error| machine_error(error.to_string()))?;
        machine
            .boot_linux(
                LINUX_IMAGE,
                &[],
                "earlycon=sbi console=hvc0 init=/init panic=-1",
            )
            .map_err(|error| machine_error(error.to_string()))?;
        machine
    };
    *STATE.lock().expect("worker state lock") = Some(WorkerState {
        machine,
        pending_request: None,
    });
    Ok(())
}

fn run_slice(bytes: &mut [u8]) -> WorkerResult {
    let budget = protocol::read_u64(bytes, field::SLICE_BUDGET).unwrap_or_default();
    if budget == 0 {
        return Err(invalid("slice budget must be greater than zero"));
    }
    let mut state = STATE.lock().expect("worker state lock");
    let state = state.as_mut().ok_or_else(|| {
        (
            Status::NotInitialized,
            "Linux vCPU is not initialized".to_string(),
        )
    })?;
    let report = state.machine.run_slice(budget);
    protocol::write_u64(bytes, field::STEPS_EXECUTED, report.steps_executed);
    protocol::write_u64(
        bytes,
        field::TOTAL_STEPS_EXECUTED,
        report.total_steps_executed,
    );
    protocol::write_u64(
        bytes,
        field::INSTRUCTIONS_RETIRED,
        report.instructions_retired,
    );
    protocol::write_u64(
        bytes,
        field::TOTAL_INSTRUCTIONS_RETIRED,
        report.total_instructions_retired,
    );

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
    encode_payload(bytes, &report.console, message, error.as_bytes())
}

fn push_console(bytes: &mut [u8]) -> WorkerResult {
    let input = input(bytes)?.to_vec();
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

    const SIGNED_WORKER: &[u8] = include_bytes!("../../../assets/linux-vcpu.wasm");
    const SIGNED_WORKER_HASH: &str =
        "blake3:d935bf594b0282f29fe2cb90ab5c4cd10fed0446feab1df70fe7d0edd9f4a9fb";

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
    fn signed_worker_restores_linux_inside_the_real_compute_runtime() {
        use astrid_compute::{
            ComputeLedger, ComputeLimits, ComputeRuntime, ExecutionMode, GroupRequest, Parallelism,
            WorkDescriptor, WorkerArtifact,
        };
        use astrid_core::principal::PrincipalId;

        let artifact =
            WorkerArtifact::from_bytes(protocol::WORKER_ID, SIGNED_WORKER, SIGNED_WORKER_HASH)
                .expect("signed worker hash");
        let runtime = ComputeRuntime::new(ComputeLedger::default(), ComputeLimits::default())
            .expect("compute runtime");
        let group = runtime
            .open_group(
                &PrincipalId::new("linux-worker-conformance").expect("principal"),
                &artifact,
                GroupRequest {
                    mode: ExecutionMode::Deterministic,
                    parallelism: Parallelism::Exact(1),
                    initial_memory_pages: 1024,
                    maximum_memory_pages: 2048,
                },
            )
            .expect("worker admission");
        let control_offset = (63 * 1024 * 1024) as u64;
        let descriptor = WorkDescriptor {
            offset: control_offset,
            length: protocol::CONTROL_BYTES as u64,
            tag: Operation::InitPrewarm as u64,
            worker_index: Some(0),
            fuel: None,
        };
        let mut request = vec![0_u8; protocol::HEADER_BYTES];
        protocol::write_u32(&mut request, field::MAGIC, protocol::MAGIC);
        protocol::write_u32(&mut request, field::VERSION, protocol::VERSION);
        protocol::write_u32(
            &mut request,
            field::OPERATION,
            Operation::InitPrewarm as u32,
        );
        protocol::write_u64(&mut request, field::RAM_BYTES, 32 * 1024 * 1024);
        protocol::write_u64(&mut request, field::MAX_CONSOLE_BYTES, 64 * 1024);
        group.write(control_offset, &request).expect("write init");
        let result = group
            .submit(descriptor)
            .expect("submit init")
            .join()
            .expect("restore checkpoint");
        assert_eq!(result.worker_status, 0);
        let header = group
            .read(control_offset, protocol::HEADER_BYTES as u32)
            .expect("read init response");
        assert_eq!(
            protocol::read_u32(&header, field::STATUS),
            Some(Status::Ok as u32)
        );

        protocol::write_u32(&mut request, field::OPERATION, Operation::RunSlice as u32);
        protocol::write_u64(&mut request, field::SLICE_BUDGET, 100_000);
        group.write(control_offset, &request).expect("write slice");
        group
            .submit(WorkDescriptor {
                tag: Operation::RunSlice as u64,
                ..descriptor
            })
            .expect("submit slice")
            .join()
            .expect("run restored Linux");
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
        assert!(
            protocol::read_u64(&header, field::TOTAL_STEPS_EXECUTED)
                .is_some_and(|steps| steps >= 15_899_016)
        );
        assert!(protocol::read_u32(&header, field::MESSAGE_LEN).is_some_and(|length| length > 0));
    }
}
