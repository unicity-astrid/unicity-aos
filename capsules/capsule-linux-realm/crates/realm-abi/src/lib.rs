#![no_std]
#![deny(unsafe_code)]

//! Private, versioned guest ABI constants for AOS Realm.
//!
//! This is not a public Astrid WIT contract. It is the narrow boundary between
//! a process module and the realm runtime contained by the same capsule.

/// Import module used by the first realm guest ABI.
pub const IMPORT_MODULE_V0: &str = "aos_realm_v0";

/// Guest file descriptor for standard input.
pub const STDIN_FD: i32 = 0;

/// Guest file descriptor for standard output.
pub const STDOUT_FD: i32 = 1;

/// Guest file descriptor for standard error.
pub const STDERR_FD: i32 = 2;

/// First descriptor available to guest-opened files.
pub const FIRST_FILE_FD: i32 = 3;

/// Guest `open` mode for an existing read-only file.
pub const OPEN_READ: i32 = 0;

/// Guest `open` mode for a truncate-or-create writable file.
pub const OPEN_WRITE_TRUNCATE: i32 = 1;

/// Maximum UTF-8 path size admitted by the private seed ABI.
pub const MAX_PATH_BYTES: usize = 4096;

/// Maximum combined UTF-8 argument bytes admitted for one process.
pub const MAX_ARGUMENT_BYTES: usize = 32 * 1024;

/// Maximum entries admitted in one process argument vector.
pub const MAX_ARGUMENT_COUNT: usize = 64;

/// Maximum combined UTF-8 environment bytes admitted for one process.
pub const MAX_ENVIRONMENT_BYTES: usize = 32 * 1024;

/// Maximum `KEY=VALUE` entries admitted in one process environment.
pub const MAX_ENVIRONMENT_COUNT: usize = 64;

/// Maximum bytes in one signed executable-catalog path.
pub const MAX_EXECUTABLE_PATH_BYTES: usize = 256;

/// Maximum descriptor actions admitted in one signed spawn request.
pub const MAX_SPAWN_ACTIONS: usize = 16;

/// Signed executable-catalog selector for the embedded `echo` guest.
pub const SIGNED_PROGRAM_ECHO: i32 = 1;

/// Signed executable-catalog selector for the embedded `stdin-cat` guest.
pub const SIGNED_PROGRAM_STDIN_CAT: i32 = 2;

/// Descriptor scalar used when a spawn request has no inheritance binding.
pub const NO_DESCRIPTOR: i32 = -1;

/// Version of the fixed signed-spawn request record.
pub const SPAWN_RECORD_VERSION: i32 = 1;

/// Bytes in a signed-spawn request record.
///
/// Every field is an explicitly little-endian `i32`. In order, the fields are
/// `version`, `flags`, executable path pointer/length, argv table pointer/count,
/// environment table pointer/count, action table pointer/count, and output
/// process-handle pointer. Flags must be zero in version 1.
pub const SPAWN_RECORD_BYTES: usize = 44;

/// Offset of the record version.
pub const SPAWN_RECORD_VERSION_OFFSET: usize = 0;

/// Offset of the reserved flags word.
pub const SPAWN_RECORD_FLAGS_OFFSET: usize = 4;

/// Offset of the executable path pointer.
pub const SPAWN_RECORD_EXECUTABLE_POINTER_OFFSET: usize = 8;

/// Offset of the executable path length.
pub const SPAWN_RECORD_EXECUTABLE_LENGTH_OFFSET: usize = 12;

/// Offset of the argument-vector table pointer.
pub const SPAWN_RECORD_ARGV_POINTER_OFFSET: usize = 16;

/// Offset of the argument-vector entry count.
pub const SPAWN_RECORD_ARGV_COUNT_OFFSET: usize = 20;

/// Offset of the environment table pointer.
pub const SPAWN_RECORD_ENV_POINTER_OFFSET: usize = 24;

/// Offset of the environment entry count.
pub const SPAWN_RECORD_ENV_COUNT_OFFSET: usize = 28;

/// Offset of the descriptor-action table pointer.
pub const SPAWN_RECORD_ACTION_POINTER_OFFSET: usize = 32;

/// Offset of the descriptor-action entry count.
pub const SPAWN_RECORD_ACTION_COUNT_OFFSET: usize = 36;

/// Offset of the output process-handle pointer.
pub const SPAWN_RECORD_HANDLE_POINTER_OFFSET: usize = 40;

/// Bytes in one `{pointer, length}` UTF-8 string record.
pub const STRING_RECORD_BYTES: usize = 8;

/// Bytes in one `{kind, source, target}` descriptor-action record.
pub const SPAWN_ACTION_BYTES: usize = 12;

/// Spawn action that maps one exact parent descriptor into the child.
pub const SPAWN_ACTION_DUP: i32 = 1;

/// Spawn action that closes one parent descriptor after child creation.
///
/// The close is part of the semantic-kernel transaction: a rejected spawn does
/// not consume the descriptor.
pub const SPAWN_ACTION_CLOSE_PARENT: i32 = 2;

/// Bytes in the guest-memory process-handle record.
///
/// The record contains little-endian `generation: u64` followed by
/// little-endian `process_id: u64`. It intentionally has an explicit wire
/// encoding rather than relying on a Rust layout.
pub const PROCESS_HANDLE_BYTES: usize = 16;

/// Byte offset of the realm generation in a process-handle record.
pub const PROCESS_HANDLE_GENERATION_OFFSET: usize = 0;

/// Byte offset of the process identifier in a process-handle record.
pub const PROCESS_HANDLE_ID_OFFSET: usize = 8;

/// Bytes in the guest-memory pipe-ends record.
///
/// The record contains little-endian `read_fd: i32` followed by
/// little-endian `write_fd: i32`.
pub const PIPE_ENDS_BYTES: usize = 8;

/// Bytes in the guest-memory child-termination record.
///
/// The record contains little-endian `kind: i32` followed by little-endian
/// `value: i32`.
pub const TERMINATION_BYTES: usize = 8;

/// Termination-record kind for an ordinary exit status.
pub const TERMINATION_EXITED: i32 = 0;

/// Termination-record kind for a realm signal.
pub const TERMINATION_SIGNALED: i32 = 1;

/// Stable guest code for the realm interrupt signal.
pub const SIGNAL_INTERRUPT: i32 = 1;

/// Stable guest code for the realm terminate signal.
pub const SIGNAL_TERMINATE: i32 = 2;

/// Stable guest code for the realm kill signal.
pub const SIGNAL_KILL: i32 = 3;

/// Stable guest code for the realm broken-pipe signal.
pub const SIGNAL_PIPE: i32 = 4;

/// Realm identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RealmId(u64);

impl RealmId {
    /// Creates an identifier from its stable realm-local representation.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the realm-local representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Process identifier, unique within one realm generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(u64);

impl ProcessId {
    /// Creates an identifier from its realm-local representation.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the realm-local representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Generation-checked process identity passed through guest memory.
///
/// The process number is only unique for one live Realm machine. The
/// generation prevents a retained or forged record from naming a process after
/// an actor restart reuses that number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessHandle {
    generation: u64,
    process: ProcessId,
}

impl ProcessHandle {
    /// Create a handle from its generation and realm-local process identity.
    pub const fn new(generation: u64, process: ProcessId) -> Self {
        Self {
            generation,
            process,
        }
    }

    /// Return the Realm boot generation that owns this process.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Return the realm-local process identity.
    pub const fn process(self) -> ProcessId {
        self.process
    }
}

/// Pipe identifier, unique within one live realm kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PipeId(u64);

impl PipeId {
    /// Creates an identifier from its realm-local representation.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the realm-local representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Descriptor number in a single process descriptor table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Descriptor(i32);

impl Descriptor {
    /// Standard input.
    pub const STDIN: Self = Self(STDIN_FD);

    /// Standard output.
    pub const STDOUT: Self = Self(STDOUT_FD);

    /// Standard error.
    pub const STDERR: Self = Self(STDERR_FD);

    /// Creates a descriptor from its guest representation.
    pub const fn new(value: i32) -> Self {
        Self(value)
    }

    /// Returns the guest representation.
    pub const fn get(self) -> i32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_identifiers_keep_their_types() {
        let realm = RealmId::new(7);
        let process = ProcessId::new(7);
        let pipe = PipeId::new(7);

        assert_eq!(realm.get(), process.get());
        assert_eq!(process.get(), pipe.get());
        assert_eq!(Descriptor::STDIN.get(), STDIN_FD);
        assert_eq!(Descriptor::STDOUT.get(), STDOUT_FD);
        assert_eq!(Descriptor::STDERR.get(), STDERR_FD);
        assert_eq!(Descriptor::new(FIRST_FILE_FD).get(), FIRST_FILE_FD);
        let handle = ProcessHandle::new(9, process);
        assert_eq!(handle.generation(), 9);
        assert_eq!(handle.process(), process);
        assert_eq!(PROCESS_HANDLE_BYTES, 16);
        assert_eq!(PIPE_ENDS_BYTES, 8);
        assert_eq!(TERMINATION_BYTES, 8);
        assert_eq!(SPAWN_RECORD_BYTES, 44);
        assert_eq!(SPAWN_RECORD_HANDLE_POINTER_OFFSET + 4, SPAWN_RECORD_BYTES);
        assert_eq!(STRING_RECORD_BYTES, 8);
        assert_eq!(SPAWN_ACTION_BYTES, 12);
        assert_ne!(SPAWN_ACTION_DUP, SPAWN_ACTION_CLOSE_PARENT);
    }
}
