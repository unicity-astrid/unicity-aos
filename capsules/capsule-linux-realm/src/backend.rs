//! Private full-system backend boundary.
//!
//! Native conformance tests retain the AOS-owned reference interpreter in the
//! controller process. Production capsules place that exact machine behind a
//! signed Astrid compute worker, leaving 9P and every other host effect in the
//! principal-affine controller component.

use aos_realm_machine::{HostRequestFailure, MachineConfig};
#[cfg(any(target_arch = "wasm32", test))]
use aos_realm_vcpu_protocol as protocol;

#[cfg(target_arch = "wasm32")]
use aos_realm_vcpu_protocol::{Operation, Outcome, RequestFailure, Status, field};

#[cfg(not(target_arch = "wasm32"))]
use aos_realm_machine::{
    CheckpointBinding, CheckpointDigest, HostRequestId, Machine, MachineCheckpoint, SliceOutcome,
};
#[cfg(target_arch = "wasm32")]
use astrid_sdk::compute::{ComputeGroup, GroupRequest, WorkDescriptor};

/// Stable identity of the selected Linux machine implementation.
///
/// Compute changes where the interpreter executes, not the machine semantics
/// exposed to tools, traces, checkpoints, or differential tests.
pub(crate) const DEFAULT_LINUX_BACKEND_ID: &str = "aos-rv64-interpreter";
/// Exact prewarm artifact size recorded in `linux/PREWARM.lock`.
pub(crate) const LINUX_PREWARM_CHECKPOINT_BYTES: usize = 8_495_869;

#[cfg(not(target_arch = "wasm32"))]
const LINUX_IMAGE: &[u8] = include_bytes!("../linux/Image");
#[cfg(not(target_arch = "wasm32"))]
const LINUX_SOURCES: &[u8] = include_bytes!("../linux/SOURCES.lock");
#[cfg(not(target_arch = "wasm32"))]
const LINUX_PREWARM_32M: &[u8] = include_bytes!("../linux/prewarm-32m.aos-machine");

/// Normalized host request produced by the machine backend.
#[derive(Debug)]
pub(crate) struct LinuxHostRequest {
    pub(crate) id: u64,
    pub(crate) channel: u32,
    pub(crate) message: Vec<u8>,
    pub(crate) max_response_bytes: usize,
}

/// Normalized scheduling result independent of its execution substrate.
#[derive(Debug)]
pub(crate) enum LinuxSliceOutcome {
    Yielded,
    Halted { passed: bool, code: u32 },
    HostRequest(LinuxHostRequest),
    Trapped(String),
}

/// Normalized accounting and output for one machine slice.
#[derive(Debug)]
pub(crate) struct LinuxSliceReport {
    pub(crate) outcome: LinuxSliceOutcome,
    pub(crate) console: Vec<u8>,
    pub(crate) steps_executed: u64,
}

/// Principal-resident full-system Linux machine.
#[derive(Debug)]
pub(crate) enum LinuxMachine {
    #[cfg(not(target_arch = "wasm32"))]
    Reference(ReferenceMachine),
    #[cfg(target_arch = "wasm32")]
    Compute(ComputeMachine),
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct ReferenceMachine {
    machine: Machine,
    pending_request: Option<HostRequestId>,
}

impl LinuxMachine {
    /// Admit and initialize the production backend for this build target.
    pub(crate) fn new(config: MachineConfig, restore_prewarm: bool) -> Result<Self, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let machine = if restore_prewarm {
                let binding = CheckpointBinding::new(
                    CheckpointDigest::hash(LINUX_IMAGE),
                    CheckpointDigest::hash(LINUX_SOURCES),
                );
                let checkpoint = MachineCheckpoint::decode(LINUX_PREWARM_32M, binding)
                    .map_err(|error| error.to_string())?;
                if checkpoint.ram_bytes() != config.ram_bytes
                    || checkpoint.max_console_bytes() != config.max_console_bytes
                {
                    return Err(
                        "Linux prewarm checkpoint resources do not match admitted resources"
                            .to_string(),
                    );
                }
                checkpoint.into_machine()
            } else {
                let mut machine = Machine::new(config).map_err(|error| error.to_string())?;
                machine
                    .boot_linux(
                        LINUX_IMAGE,
                        &[],
                        "earlycon=sbi console=hvc0 init=/init panic=-1",
                    )
                    .map_err(|error| error.to_string())?;
                machine
            };
            Ok(Self::Reference(ReferenceMachine {
                machine,
                pending_request: None,
            }))
        }

        #[cfg(target_arch = "wasm32")]
        ComputeMachine::new(config, restore_prewarm).map(Self::Compute)
    }

    #[cfg(test)]
    pub(crate) fn new_reference(config: MachineConfig) -> Result<Self, String> {
        Machine::new(config)
            .map(|machine| {
                Self::Reference(ReferenceMachine {
                    machine,
                    pending_request: None,
                })
            })
            .map_err(|error| error.to_string())
    }

    pub(crate) const fn backend_id(&self) -> &'static str {
        DEFAULT_LINUX_BACKEND_ID
    }

    pub(crate) fn push_console_input(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Reference(reference) => {
                reference.machine.push_console_input(bytes);
                Ok(())
            }
            #[cfg(target_arch = "wasm32")]
            Self::Compute(compute) => compute.push_console_input(bytes),
        }
    }

    pub(crate) fn run_slice(
        &mut self,
        instruction_budget: u64,
    ) -> Result<LinuxSliceReport, String> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Reference(reference) => reference.run_slice(instruction_budget),
            #[cfg(target_arch = "wasm32")]
            Self::Compute(compute) => compute.run_slice(instruction_budget),
        }
    }

    pub(crate) fn complete_9p_request(&mut self, id: u64, response: &[u8]) -> Result<(), String> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Reference(reference) => reference.complete_9p_request(id, response),
            #[cfg(target_arch = "wasm32")]
            Self::Compute(compute) => compute.complete_9p_request(id, response),
        }
    }

    pub(crate) fn fail_9p_request(
        &mut self,
        id: u64,
        failure: HostRequestFailure,
    ) -> Result<(), String> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Reference(reference) => reference.fail_9p_request(id, failure),
            #[cfg(target_arch = "wasm32")]
            Self::Compute(compute) => compute.fail_9p_request(id, failure),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ReferenceMachine {
    fn run_slice(&mut self, instruction_budget: u64) -> Result<LinuxSliceReport, String> {
        let report = self.machine.run_slice(instruction_budget);
        let outcome = match report.outcome {
            SliceOutcome::Yielded => LinuxSliceOutcome::Yielded,
            SliceOutcome::Halted(status) => LinuxSliceOutcome::Halted {
                passed: status.passed,
                code: status.code,
            },
            SliceOutcome::HostRequest(request) => {
                self.pending_request = Some(request.id);
                LinuxSliceOutcome::HostRequest(LinuxHostRequest {
                    id: request.id.get(),
                    channel: request.channel,
                    message: request.message,
                    max_response_bytes: request.max_response_bytes,
                })
            }
            SliceOutcome::Trapped(trap) => LinuxSliceOutcome::Trapped(trap.to_string()),
        };
        Ok(LinuxSliceReport {
            outcome,
            console: report.console,
            steps_executed: report.steps_executed,
        })
    }

    fn pending_request(&self, raw: u64) -> Result<HostRequestId, String> {
        let id = self
            .pending_request
            .ok_or_else(|| "Linux machine has no pending 9P request".to_string())?;
        if id.get() != raw {
            return Err("Linux machine 9P request identity mismatch".to_string());
        }
        Ok(id)
    }

    fn complete_9p_request(&mut self, raw: u64, response: &[u8]) -> Result<(), String> {
        let id = self.pending_request(raw)?;
        self.machine
            .complete_9p_request(id, response)
            .map_err(|error| error.to_string())?;
        self.pending_request = None;
        Ok(())
    }

    fn fail_9p_request(&mut self, raw: u64, failure: HostRequestFailure) -> Result<(), String> {
        let id = self.pending_request(raw)?;
        self.machine
            .fail_9p_request(id, failure)
            .map_err(|error| error.to_string())?;
        self.pending_request = None;
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(crate) struct ComputeMachine {
    group: ComputeGroup,
    control_offset: u64,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
struct ComputeResponse {
    header: Vec<u8>,
    payload: Vec<u8>,
}

#[cfg(target_arch = "wasm32")]
impl ComputeMachine {
    fn new(config: MachineConfig, restore_prewarm: bool) -> Result<Self, String> {
        let (initial_pages, maximum_pages, control_offset) =
            shared_memory_layout(config.ram_bytes)?;
        let group = ComputeGroup::open(
            &GroupRequest::new(protocol::WORKER_ID, initial_pages, maximum_pages).deterministic(),
        )
        .map_err(|error| format!("admit Linux vCPU compute worker: {error}"))?;
        let machine = Self {
            group,
            control_offset,
        };
        machine.invoke(
            if restore_prewarm {
                Operation::InitPrewarm
            } else {
                Operation::InitCold
            },
            &[],
            |header| {
                protocol::write_u64(header, field::RAM_BYTES, config.ram_bytes as u64);
                protocol::write_u64(
                    header,
                    field::MAX_CONSOLE_BYTES,
                    config.max_console_bytes as u64,
                );
            },
        )?;
        Ok(machine)
    }

    fn push_console_input(&self, bytes: &[u8]) -> Result<(), String> {
        self.invoke(Operation::PushConsole, bytes, |_| {})?;
        Ok(())
    }

    fn complete_9p_request(&self, id: u64, response: &[u8]) -> Result<(), String> {
        self.invoke(Operation::Complete9p, response, |header| {
            protocol::write_u64(header, field::REQUEST_ID, id);
        })?;
        Ok(())
    }

    fn fail_9p_request(&self, id: u64, failure: HostRequestFailure) -> Result<(), String> {
        self.invoke(Operation::Fail9p, &[], |header| {
            protocol::write_u64(header, field::REQUEST_ID, id);
            protocol::write_u32(
                header,
                field::REQUEST_FAILURE,
                match failure {
                    HostRequestFailure::Failed => RequestFailure::Failed as u32,
                    HostRequestFailure::Denied => RequestFailure::Denied as u32,
                },
            );
        })?;
        Ok(())
    }

    fn run_slice(&self, instruction_budget: u64) -> Result<LinuxSliceReport, String> {
        let response = self.invoke(Operation::RunSlice, &[], |header| {
            protocol::write_u64(header, field::SLICE_BUDGET, instruction_budget);
        })?;
        let console_len = field_len(&response.header, field::CONSOLE_LEN)?;
        let message_len = field_len(&response.header, field::MESSAGE_LEN)?;
        let error_len = field_len(&response.header, field::ERROR_LEN)?;
        let console_end = console_len;
        let message_end = console_end
            .checked_add(message_len)
            .ok_or_else(|| "Linux vCPU response range overflow".to_string())?;
        let error_end = message_end
            .checked_add(error_len)
            .ok_or_else(|| "Linux vCPU response range overflow".to_string())?;
        if error_end != response.payload.len() {
            return Err("Linux vCPU response fields do not match its payload".to_string());
        }
        let outcome = protocol::read_u32(&response.header, field::OUTCOME)
            .and_then(|value| Outcome::try_from(value).ok())
            .ok_or_else(|| "Linux vCPU returned an unknown scheduling outcome".to_string())?;
        let outcome = match outcome {
            Outcome::Yielded => LinuxSliceOutcome::Yielded,
            Outcome::Halted => LinuxSliceOutcome::Halted {
                passed: protocol::read_u32(&response.header, field::HALT_PASSED)
                    .unwrap_or_default()
                    != 0,
                code: protocol::read_u32(&response.header, field::HALT_CODE).unwrap_or_default(),
            },
            Outcome::HostRequest => LinuxSliceOutcome::HostRequest(LinuxHostRequest {
                id: required_u64(&response.header, field::REQUEST_ID, "request id")?,
                channel: required_u32(&response.header, field::REQUEST_CHANNEL, "request channel")?,
                message: response.payload[console_end..message_end].to_vec(),
                max_response_bytes: field_len(&response.header, field::MAX_RESPONSE_BYTES)?,
            }),
            Outcome::Trapped => LinuxSliceOutcome::Trapped(
                String::from_utf8_lossy(&response.payload[message_end..error_end]).into_owned(),
            ),
            Outcome::None => {
                return Err("Linux vCPU run returned no scheduling outcome".to_string());
            }
        };
        Ok(LinuxSliceReport {
            outcome,
            console: response.payload[..console_end].to_vec(),
            steps_executed: required_u64(&response.header, field::STEPS_EXECUTED, "slice steps")?,
        })
    }

    fn invoke(
        &self,
        operation: Operation,
        input: &[u8],
        configure: impl FnOnce(&mut [u8]),
    ) -> Result<ComputeResponse, String> {
        let request_len = protocol::HEADER_BYTES
            .checked_add(input.len())
            .ok_or_else(|| "Linux vCPU request range overflow".to_string())?;
        if request_len > protocol::CONTROL_BYTES {
            return Err("Linux vCPU request exceeds its bounded descriptor".to_string());
        }
        let mut request = vec![0_u8; request_len];
        protocol::write_u32(&mut request, field::MAGIC, protocol::MAGIC);
        protocol::write_u32(&mut request, field::VERSION, protocol::VERSION);
        protocol::write_u32(&mut request, field::OPERATION, operation as u32);
        protocol::write_u32(
            &mut request,
            field::INPUT_LEN,
            u32::try_from(input.len()).map_err(|_| "Linux vCPU input is too large".to_string())?,
        );
        configure(&mut request[..protocol::HEADER_BYTES]);
        request[protocol::HEADER_BYTES..].copy_from_slice(input);
        self.group
            .write(self.control_offset, &request)
            .map_err(|error| format!("write Linux vCPU request: {error}"))?;
        let result = self
            .group
            .submit(
                WorkDescriptor::new(
                    self.control_offset,
                    protocol::CONTROL_BYTES as u64,
                    operation as u64,
                )
                .on_worker(0),
            )
            .map_err(|error| format!("submit Linux vCPU operation: {error}"))?
            .join()
            .map_err(|error| format!("join Linux vCPU operation: {error}"))?;
        if result.worker_status != 0 {
            return Err(format!(
                "Linux vCPU transport returned status {}",
                result.worker_status
            ));
        }
        let header = self
            .group
            .read(self.control_offset, protocol::HEADER_BYTES as u32)
            .map_err(|error| format!("read Linux vCPU response header: {error}"))?;
        if protocol::read_u32(&header, field::MAGIC) != Some(protocol::MAGIC)
            || protocol::read_u32(&header, field::VERSION) != Some(protocol::VERSION)
        {
            return Err("Linux vCPU returned an invalid protocol envelope".to_string());
        }
        let response_len = field_len(&header, field::RESPONSE_LEN)?;
        if response_len > protocol::CONTROL_BYTES - protocol::HEADER_BYTES {
            return Err("Linux vCPU response exceeds its bounded descriptor".to_string());
        }
        let payload = if response_len == 0 {
            Vec::new()
        } else {
            self.group
                .read(
                    self.control_offset + protocol::HEADER_BYTES as u64,
                    response_len as u32,
                )
                .map_err(|error| format!("read Linux vCPU response payload: {error}"))?
        };
        let status = protocol::read_u32(&header, field::STATUS)
            .and_then(|value| Status::try_from(value).ok())
            .ok_or_else(|| "Linux vCPU returned an unknown operation status".to_string())?;
        if status != Status::Ok {
            let error_len = field_len(&header, field::ERROR_LEN)?.min(payload.len());
            let error_start = payload.len().saturating_sub(error_len);
            let detail = String::from_utf8_lossy(&payload[error_start..]);
            return Err(format!("Linux vCPU {status:?}: {detail}"));
        }
        Ok(ComputeResponse { header, payload })
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn shared_memory_layout(ram_bytes: usize) -> Result<(u32, u32, u64), String> {
    // Rust's wasm allocator acquires heap segments with `memory.grow`; it does
    // not adopt unused bytes from the imported memory's initial extent. Keep
    // the signed worker's code/data and descriptor in the fixed 64-MiB base,
    // then reserve guest RAM plus bounded allocator headroom as the maximum.
    // Astrid reserves that maximum before the worker can grow itself.
    let maximum_required = protocol::WORKER_MIN_MEMORY_BYTES
        .checked_add(ram_bytes)
        .and_then(|value| value.checked_add(protocol::WORKER_HEAP_OVERHEAD_BYTES))
        .ok_or_else(|| "Linux vCPU shared-memory size overflow".to_string())?
        .max(protocol::WORKER_MIN_MEMORY_BYTES);
    let maximum_pages = maximum_required
        .checked_add(protocol::WASM_PAGE_BYTES - 1)
        .ok_or_else(|| "Linux vCPU shared-memory rounding overflow".to_string())?
        / protocol::WASM_PAGE_BYTES;
    let maximum_bytes = maximum_pages
        .checked_mul(protocol::WASM_PAGE_BYTES)
        .ok_or_else(|| "Linux vCPU shared-memory size overflow".to_string())?;
    if maximum_bytes > protocol::WORKER_MAX_MEMORY_BYTES {
        return Err("Linux vCPU shared memory exceeds the worker's signed maximum".to_string());
    }
    let initial_pages = protocol::WORKER_MIN_MEMORY_BYTES / protocol::WASM_PAGE_BYTES;
    let control_offset = protocol::WORKER_MIN_MEMORY_BYTES
        .checked_sub(protocol::CONTROL_BYTES)
        .ok_or_else(|| "Linux vCPU has no room for its control descriptor".to_string())?;
    Ok((
        u32::try_from(initial_pages)
            .map_err(|_| "Linux vCPU initial page count is too large".to_string())?,
        u32::try_from(maximum_pages)
            .map_err(|_| "Linux vCPU maximum page count is too large".to_string())?,
        control_offset as u64,
    ))
}

#[cfg(target_arch = "wasm32")]
fn field_len(header: &[u8], offset: usize) -> Result<usize, String> {
    required_u32(header, offset, "length").and_then(|value| {
        usize::try_from(value).map_err(|_| "length is not addressable".to_string())
    })
}

#[cfg(target_arch = "wasm32")]
fn required_u32(header: &[u8], offset: usize, name: &str) -> Result<u32, String> {
    protocol::read_u32(header, offset)
        .ok_or_else(|| format!("Linux vCPU response is missing {name}"))
}

#[cfg(target_arch = "wasm32")]
fn required_u64(header: &[u8], offset: usize, name: &str) -> Result<u64, String> {
    protocol::read_u64(header, offset)
        .ok_or_else(|| format!("Linux vCPU response is missing {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_backend_identity_is_explicit_before_boot() {
        let machine = LinuxMachine::new_reference(MachineConfig {
            ram_bytes: 16 * 1024 * 1024,
            max_console_bytes: 1024,
        })
        .expect("reference backend admission");

        assert_eq!(machine.backend_id(), DEFAULT_LINUX_BACKEND_ID);
    }

    #[test]
    fn shared_memory_keeps_machine_and_control_region_disjoint() {
        let (initial_pages, maximum_pages, control_offset) =
            shared_memory_layout(32 * 1024 * 1024).expect("default layout");
        assert_eq!(initial_pages, 1024);
        assert_eq!(maximum_pages, 2048);
        assert_eq!(control_offset, (63 * 1024 * 1024) as u64);

        let (initial_pages, maximum_pages, control_offset) =
            shared_memory_layout(256 * 1024 * 1024).expect("largest Realm layout");
        assert_eq!(initial_pages, 1024);
        assert_eq!(maximum_pages, 5632);
        assert_eq!(control_offset, (63 * 1024 * 1024) as u64);
    }
}
