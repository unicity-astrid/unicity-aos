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
/// Current controller/worker protocol version.
pub const VERSION: u32 = 1;
/// Fixed header before any input or response payload.
pub const HEADER_BYTES: usize = 128;
/// Maximum descriptor admitted by the Astrid compute copy boundary.
pub const CONTROL_BYTES: usize = 1024 * 1024;
/// Dynamic heap headroom beyond the admitted guest RAM.
pub const WORKER_HEAP_OVERHEAD_BYTES: usize = 32 * 1024 * 1024;
/// Smallest shared memory accepted by the worker import.
pub const WORKER_MIN_MEMORY_BYTES: usize = 64 * 1024 * 1024;
/// Largest shared memory accepted by the worker import.
pub const WORKER_MAX_MEMORY_BYTES: usize = 512 * 1024 * 1024;
/// WebAssembly linear-memory page size.
pub const WASM_PAGE_BYTES: usize = 65_536;

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
}

/// Stateful operation requested by the Realm controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation {
    /// Create a machine and boot the embedded Linux image.
    InitCold = 1,
    /// Restore the signed 32-MiB prewarm checkpoint.
    InitPrewarm = 2,
    /// Execute one bounded RV64 scheduling slice.
    RunSlice = 3,
    /// Append bytes to the emulated console input.
    PushConsole = 4,
    /// Complete the current 9P request.
    Complete9p = 5,
    /// Fail the current 9P request.
    Fail9p = 6,
    /// Drop all machine state held by this worker instance.
    Reset = 7,
}

impl TryFrom<u32> for Operation {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::InitCold),
            2 => Ok(Self::InitPrewarm),
            3 => Ok(Self::RunSlice),
            4 => Ok(Self::PushConsole),
            5 => Ok(Self::Complete9p),
            6 => Ok(Self::Fail9p),
            7 => Ok(Self::Reset),
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
        assert_eq!(read_u32(&bytes, field::MAGIC), Some(MAGIC));
        assert_eq!(read_u64(&bytes, field::RAM_BYTES), Some(32 * 1024 * 1024));
    }
}
