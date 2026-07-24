//! Shared-memory control protocol for the AOS Linux Realm vCPU worker.
//!
//! The protocol is intentionally smaller than a device model. The worker owns
//! RV64 CPU, RAM, kernel, and emulated devices; the controller owns Astrid host
//! effects. A bounded descriptor carries lifecycle commands, scheduling slices,
//! console bytes, and 9P requests across that authority boundary.

#![no_std]
#![deny(missing_docs)]
#![deny(unsafe_code)]

/// Signed worker component id in `Capsule.toml`.
pub const WORKER_ID: &str = "linux-vcpu";
/// `LVC1`, encoded little-endian at the beginning of every descriptor.
pub const MAGIC: u32 = 0x4c56_4331;
/// Controller/worker protocol version.
///
/// This private ABI has not shipped, so its first releasable shape starts at
/// version 1 rather than preserving local development revisions.
pub const VERSION: u32 = 1;
/// Fixed header before any input or response payload.
pub const HEADER_BYTES: usize = 128;
/// Maximum descriptor admitted by the Astrid compute copy boundary.
pub const CONTROL_BYTES: usize = 1024 * 1024;
/// Dynamic heap headroom beyond the admitted guest RAM.
pub const WORKER_HEAP_OVERHEAD_BYTES: usize = 64 * 1024 * 1024;
/// Smallest shared memory accepted by the worker import.
pub const WORKER_MIN_MEMORY_BYTES: usize = 64 * 1024 * 1024;
/// Largest shared memory accepted by the worker import.
pub const WORKER_MAX_MEMORY_BYTES: usize = 3584 * 1024 * 1024;
/// Maximum private worker stacks admitted by the signed machine object.
pub const MAX_WORKER_STACKS: usize = 64;
/// Linear-memory stack bytes reserved for each possible compute worker.
pub const WORKER_STACK_STRIDE_BYTES: usize = 512 * 1024;
/// Total LLVM stack arena split by Astrid into one disjoint slot per worker.
pub const WORKER_STACK_RESERVE_BYTES: usize = MAX_WORKER_STACKS * WORKER_STACK_STRIDE_BYTES;
const _: () = assert!(WORKER_STACK_RESERVE_BYTES < WORKER_MIN_MEMORY_BYTES);
/// WebAssembly linear-memory page size.
pub const WASM_PAGE_BYTES: usize = 65_536;
/// Exact `InitCold` payload: admitted wall-clock seconds since Unix epoch.
pub const COLD_BOOT_INPUT_BYTES: usize = 8;
/// Exact `InitCheckpoint` payload: kernel and immutable-system BLAKE3 digests.
pub const CHECKPOINT_BINDING_BYTES: usize = 64;
/// Largest scheduling slice one descriptor may request.
///
/// Ten million interpreted steps keeps the worker cooperatively cancellable
/// while avoiding a durable generic-compute submission and audit record for
/// every million steps of initramfs inflation or compiler execution. This is a
/// private, pre-1.0 protocol bound; the controller always charges the exact
/// completed step count returned by the worker.
pub const MAX_SLICE_STEPS: u64 = 10_000_000;
/// Largest serial input accepted by one descriptor.
pub const MAX_CONSOLE_INPUT_BYTES: usize = 64 * 1024;

/// Byte offsets for the fixed descriptor header.
pub mod field {
    /// Protocol magic, `u32`.
    pub const MAGIC: usize = 0;
    /// Protocol version, `u32`.
    pub const VERSION: usize = 4;
    /// [`crate::Operation`], `u32`.
    pub const OPERATION: usize = 8;
    /// [`crate::Status`], `u32`.
    pub const STATUS: usize = 12;
    /// Admitted Linux RAM bytes, `u64`.
    pub const RAM_BYTES: usize = 16;
    /// Bounded console buffer bytes, `u64`.
    pub const MAX_CONSOLE_BYTES: usize = 24;
    /// Guest instruction budget for one slice, `u64`.
    pub const SLICE_BUDGET: usize = 32;
    /// Pending 9P request identity, `u64`.
    pub const REQUEST_ID: usize = 40;
    /// [`crate::RequestFailure`], `u32`.
    pub const REQUEST_FAILURE: usize = 48;
    /// Request payload bytes after the header, `u32`.
    pub const INPUT_LEN: usize = 52;
    /// Total response payload bytes after the header, `u32`.
    pub const RESPONSE_LEN: usize = 56;
    /// [`crate::Outcome`], `u32`.
    pub const OUTCOME: usize = 60;
    /// Guest steps executed by this slice, `u64`.
    pub const STEPS_EXECUTED: usize = 64;
    /// Guest steps executed since machine creation, `u64`.
    pub const TOTAL_STEPS_EXECUTED: usize = 72;
    /// Guest instructions retired by this slice, `u64`.
    pub const INSTRUCTIONS_RETIRED: usize = 80;
    /// Guest instructions retired since machine creation, `u64`.
    pub const TOTAL_INSTRUCTIONS_RETIRED: usize = 88;
    /// RV64 halt code, `u32`.
    pub const HALT_CODE: usize = 96;
    /// RV64 pass flag, `u32` boolean.
    pub const HALT_PASSED: usize = 100;
    /// Host-request channel, `u32`.
    pub const REQUEST_CHANNEL: usize = 104;
    /// Maximum admitted host response bytes, `u32`.
    pub const MAX_RESPONSE_BYTES: usize = 108;
    /// Console response bytes, `u32`.
    pub const CONSOLE_LEN: usize = 112;
    /// Host-request message bytes, `u32`.
    pub const MESSAGE_LEN: usize = 116;
    /// UTF-8 error response bytes, `u32`.
    pub const ERROR_LEN: usize = 120;
    /// Admitted logical Linux hart count, `u32`.
    pub const HART_COUNT: usize = 124;
    /// Exact logical hart targeted by [`crate::Operation::RunHartSlice`],
    /// sharing the operation-specific slot used by [`HART_COUNT`].
    pub const HART_ID: usize = HART_COUNT;
}

/// Stateful operation requested by the Realm controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation {
    /// Create a machine and boot its immutable, hash-bound Linux kernel asset.
    InitCold = 1,
    /// Execute one bounded RV64 scheduling slice.
    RunSlice = 2,
    /// Append bytes to the emulated console input.
    PushConsole = 3,
    /// Complete the current 9P request.
    Complete9p = 4,
    /// Fail the current 9P request.
    Fail9p = 5,
    /// Drop all machine state held by this worker instance.
    Reset = 6,
    /// Restore the hash-bound, principal-free Linux boot checkpoint.
    InitCheckpoint = 7,
    /// Prove that distinct admitted workers can enter this signed Rust module
    /// concurrently over disjoint descriptors.
    ///
    /// This private operation is a substrate check, not a Linux-visible
    /// device. `hart-count` carries the expected worker count and is replaced
    /// by the executing worker index in the response.
    ParallelProbe = 8,
    /// Execute one bounded slice for the exact hart assigned to this worker.
    ///
    /// `hart-id` must equal the runtime-stamped worker index. This operation
    /// never invokes the deterministic round-robin scheduler.
    RunHartSlice = 9,
}

impl TryFrom<u32> for Operation {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::InitCold),
            2 => Ok(Self::RunSlice),
            3 => Ok(Self::PushConsole),
            4 => Ok(Self::Complete9p),
            5 => Ok(Self::Fail9p),
            6 => Ok(Self::Reset),
            7 => Ok(Self::InitCheckpoint),
            8 => Ok(Self::ParallelProbe),
            9 => Ok(Self::RunHartSlice),
            _ => Err(()),
        }
    }
}

/// Worker-level result for one control operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Status {
    /// Operation completed; inspect [`Outcome`] where applicable.
    Ok = 0,
    /// Envelope, field, or payload was invalid.
    Invalid = 1,
    /// The machine has not been initialized.
    NotInitialized = 2,
    /// The RV64 machine rejected the operation.
    Machine = 3,
    /// The supplied 9P identity did not match the pending request.
    RequestMismatch = 4,
}

impl TryFrom<u32> for Status {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Invalid),
            2 => Ok(Self::NotInitialized),
            3 => Ok(Self::Machine),
            4 => Ok(Self::RequestMismatch),
            _ => Err(()),
        }
    }
}

/// Normalized result of one RV64 scheduling slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Outcome {
    /// Operation has no machine scheduling outcome.
    None = 0,
    /// The guest used its current step budget.
    Yielded = 1,
    /// The RV64 supervisor halted.
    Halted = 2,
    /// The worker needs an Astrid-owned host service.
    HostRequest = 3,
    /// The RV64 interpreter produced a deterministic trap.
    Trapped = 4,
}

impl TryFrom<u32> for Outcome {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Yielded),
            2 => Ok(Self::Halted),
            3 => Ok(Self::HostRequest),
            4 => Ok(Self::Trapped),
            _ => Err(()),
        }
    }
}

/// Controller decision for a pending 9P request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum RequestFailure {
    /// Provider failed without an authorization decision.
    Failed = 0,
    /// Astrid denied the requested channel or operation.
    Denied = 1,
}

/// Read a little-endian `u32` from a bounded descriptor.
#[must_use]
pub fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.checked_add(4)?)?
        .try_into()
        .ok()
        .map(u32::from_le_bytes)
}

/// Read a little-endian `u64` from a bounded descriptor.
#[must_use]
pub fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes
        .get(offset..offset.checked_add(8)?)?
        .try_into()
        .ok()
        .map(u64::from_le_bytes)
}

/// Write a little-endian `u32`; return `false` when out of bounds.
pub fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> bool {
    let Some(output) = bytes.get_mut(offset..offset.saturating_add(4)) else {
        return false;
    };
    output.copy_from_slice(&value.to_le_bytes());
    true
}

/// Write a little-endian `u64`; return `false` when out of bounds.
pub fn write_u64(bytes: &mut [u8], offset: usize, value: u64) -> bool {
    let Some(output) = bytes.get_mut(offset..offset.saturating_add(8)) else {
        return false;
    };
    output.copy_from_slice(&value.to_le_bytes());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_fields_round_trip_without_overlap() {
        let mut bytes = [0_u8; HEADER_BYTES];
        assert!(write_u32(&mut bytes, field::MAGIC, MAGIC));
        assert!(write_u64(&mut bytes, field::RAM_BYTES, 32 * 1024 * 1024));
        assert!(write_u32(&mut bytes, field::HART_COUNT, 8));
        assert_eq!(read_u32(&bytes, field::MAGIC), Some(MAGIC));
        assert_eq!(read_u64(&bytes, field::RAM_BYTES), Some(32 * 1024 * 1024));
        assert_eq!(read_u32(&bytes, field::HART_COUNT), Some(8));
    }

    #[test]
    fn complete_header_layout_is_non_overlapping_and_bounded() {
        let mut fields = [
            (field::MAGIC, 4),
            (field::VERSION, 4),
            (field::OPERATION, 4),
            (field::STATUS, 4),
            (field::RAM_BYTES, 8),
            (field::MAX_CONSOLE_BYTES, 8),
            (field::SLICE_BUDGET, 8),
            (field::REQUEST_ID, 8),
            (field::REQUEST_FAILURE, 4),
            (field::INPUT_LEN, 4),
            (field::RESPONSE_LEN, 4),
            (field::OUTCOME, 4),
            (field::STEPS_EXECUTED, 8),
            (field::TOTAL_STEPS_EXECUTED, 8),
            (field::INSTRUCTIONS_RETIRED, 8),
            (field::TOTAL_INSTRUCTIONS_RETIRED, 8),
            (field::HALT_CODE, 4),
            (field::HALT_PASSED, 4),
            (field::REQUEST_CHANNEL, 4),
            (field::MAX_RESPONSE_BYTES, 4),
            (field::CONSOLE_LEN, 4),
            (field::MESSAGE_LEN, 4),
            (field::ERROR_LEN, 4),
            (field::HART_COUNT, 4),
        ];
        fields.sort_unstable_by_key(|(offset, _)| *offset);
        for pair in fields.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0);
        }
        let (last_offset, last_width) = fields[fields.len() - 1];
        assert!(last_offset + last_width <= HEADER_BYTES);
    }

    #[test]
    fn operation_discriminants_are_dense_and_start_at_one() {
        assert_eq!(Operation::InitCold as u32, 1);
        assert_eq!(Operation::RunSlice as u32, 2);
        assert_eq!(Operation::PushConsole as u32, 3);
        assert_eq!(Operation::Complete9p as u32, 4);
        assert_eq!(Operation::Fail9p as u32, 5);
        assert_eq!(Operation::Reset as u32, 6);
        assert_eq!(Operation::InitCheckpoint as u32, 7);
        assert_eq!(Operation::ParallelProbe as u32, 8);
        assert_eq!(Operation::RunHartSlice as u32, 9);
        for value in 1..=9 {
            assert!(Operation::try_from(value).is_ok());
        }
        assert!(Operation::try_from(0).is_err());
        assert!(Operation::try_from(10).is_err());
    }

    #[test]
    fn private_worker_stack_arena_matches_the_machine_topology() {
        assert_eq!(MAX_WORKER_STACKS, 64);
        assert_eq!(WORKER_STACK_STRIDE_BYTES, 512 * 1024);
        assert_eq!(WORKER_STACK_RESERVE_BYTES, 32 * 1024 * 1024);
    }
}
