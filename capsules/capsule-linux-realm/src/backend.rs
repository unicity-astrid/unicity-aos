//! Private full-system backend selection seam.
//!
//! This facade prevents the Realm actor from owning one emulator's concrete
//! type. The reference interpreter is the only admitted implementation today.
//! A second backend must normalize its slices and host requests here before it
//! can affect the public Realm tool surface.

use aos_realm_machine::{
    HostCompletionError, HostRequestFailure, HostRequestId, LinuxBootLayout, Machine,
    MachineConfig, MachineError, SliceReport,
};

/// Stable identity of the currently selected production Linux backend.
pub(crate) const DEFAULT_LINUX_BACKEND_ID: &str = "aos-rv64-interpreter";

/// Principal-resident full-system Linux machine.
///
/// Keeping selection as an enum makes the set closed and reviewable. Adding a
/// variant is deliberately insufficient by itself: the adapter must also map
/// every device request into an Astrid-owned provider and pass conformance.
#[derive(Debug)]
pub(crate) enum LinuxMachine {
    Reference(Machine),
}

impl LinuxMachine {
    pub(crate) fn new_reference(config: MachineConfig) -> Result<Self, MachineError> {
        Machine::new(config).map(Self::Reference)
    }

    pub(crate) const fn backend_id(&self) -> &'static str {
        match self {
            Self::Reference(_) => DEFAULT_LINUX_BACKEND_ID,
        }
    }

    pub(crate) fn boot_linux(
        &mut self,
        kernel: &[u8],
        initramfs: &[u8],
        bootargs: &str,
    ) -> Result<LinuxBootLayout, MachineError> {
        match self {
            Self::Reference(machine) => machine.boot_linux(kernel, initramfs, bootargs),
        }
    }

    pub(crate) fn push_console_input(&mut self, bytes: &[u8]) {
        match self {
            Self::Reference(machine) => machine.push_console_input(bytes),
        }
    }

    pub(crate) fn run_slice(&mut self, instruction_budget: u64) -> SliceReport {
        match self {
            Self::Reference(machine) => machine.run_slice(instruction_budget),
        }
    }

    pub(crate) fn complete_9p_request(
        &mut self,
        id: HostRequestId,
        response: &[u8],
    ) -> Result<(), HostCompletionError> {
        match self {
            Self::Reference(machine) => machine.complete_9p_request(id, response),
        }
    }

    pub(crate) fn fail_9p_request(
        &mut self,
        id: HostRequestId,
        failure: HostRequestFailure,
    ) -> Result<(), HostCompletionError> {
        match self {
            Self::Reference(machine) => machine.fail_9p_request(id, failure),
        }
    }
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
}
