#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

//! Deterministic process, descriptor, and bounded-pipe semantics for AOS Realm.
//!
//! This crate owns no Wasmi instances and performs no Astrid host calls. It is a
//! host-testable semantic oracle for the realm actor and execution backends.

use aos_realm_abi::{Descriptor, FIRST_FILE_FD, MAX_PATH_BYTES, PipeId, ProcessId};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
};

/// Content identity of a process image admitted by the realm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutableId([u8; 32]);

impl ExecutableId {
    /// Reserved identity for the realm supervisor rather than a guest image.
    pub const REALM_SUPERVISOR: Self = Self([0; 32]);

    /// Construct an executable identity from its digest bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return the digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Signals whose terminal meaning is already defined by the seed process model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Interactive interruption request.
    Interrupt,
    /// Orderly termination request.
    Terminate,
    /// Uncatchable termination request.
    Kill,
    /// Write attempted after the final reader closed.
    Pipe,
}

/// Observable terminal status retained until a process is reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    /// The process exited with an integer status.
    Exited(i32),
    /// The process was terminated by a realm signal.
    Signaled(Signal),
}

/// Resource on which a process is parked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    /// Waiting for one direct child to terminate.
    Child(ProcessId),
    /// Waiting for bytes or EOF on a pipe.
    PipeReadable(PipeId),
    /// Waiting for capacity or a broken-pipe result on a pipe.
    PipeWritable(PipeId),
}

/// Process state visible to the deterministic scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    /// Allocated but not admitted to the run queue.
    Created,
    /// Admitted and queued for execution.
    Runnable,
    /// Currently allowed to execute guest instructions or syscalls.
    Running,
    /// Parked until a named resource changes state.
    Waiting(WaitReason),
    /// Exited and retained for its parent or the realm supervisor to reap.
    Exited(i32),
    /// Signal-terminated and retained for reaping.
    Signaled(Signal),
}

impl ProcessState {
    /// Whether the process has reached a waitable terminal state.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Exited(_) | Self::Signaled(_))
    }

    fn termination(self) -> Option<Termination> {
        match self {
            Self::Exited(status) => Some(Termination::Exited(status)),
            Self::Signaled(signal) => Some(Termination::Signaled(signal)),
            Self::Created | Self::Runnable | Self::Running | Self::Waiting(_) => None,
        }
    }
}

/// Immutable inputs retained for one process image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSpec {
    executable: ExecutableId,
    cwd: String,
}

impl ProcessSpec {
    /// Describe a process image and its normalized guest working directory.
    #[must_use]
    pub fn new(executable: ExecutableId, cwd: impl Into<String>) -> Self {
        Self {
            executable,
            cwd: cwd.into(),
        }
    }

    /// Return the process image identity.
    #[must_use]
    pub const fn executable(&self) -> ExecutableId {
        self.executable
    }

    /// Return the guest working directory.
    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
}

/// One inherited descriptor mapping applied atomically during child creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DescriptorBinding {
    /// Descriptor in the parent process.
    pub source: Descriptor,
    /// Descriptor number installed in the child.
    pub target: Descriptor,
}

/// The two process-local descriptors created for a pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipeEnds {
    /// Read endpoint.
    pub read: Descriptor,
    /// Write endpoint.
    pub write: Descriptor,
}

/// Process-local resource selected by a descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorResource {
    /// Read endpoint of a realm pipe.
    PipeRead(PipeId),
    /// Write endpoint of a realm pipe.
    PipeWrite(PipeId),
}

/// Immutable view of a process record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSnapshot {
    /// Process identity.
    pub id: ProcessId,
    /// Direct parent, or `None` when owned by the realm supervisor.
    pub parent: Option<ProcessId>,
    /// Process image identity.
    pub executable: ExecutableId,
    /// Normalized guest working directory.
    pub cwd: String,
    /// Current lifecycle state.
    pub state: ProcessState,
    /// Number of installed pipe descriptors.
    pub descriptors: usize,
}

/// Observable pipe accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipeSnapshot {
    /// Fixed byte capacity.
    pub capacity: usize,
    /// Bytes currently buffered.
    pub buffered: usize,
    /// Open read endpoints across all processes.
    pub readers: usize,
    /// Open write endpoints across all processes.
    pub writers: usize,
}

/// Admission limits owned by one realm kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealmLimits {
    /// Process records, including created processes and zombies.
    pub max_processes: usize,
    /// Live pipe objects.
    pub max_pipes: usize,
    /// Maximum capacity of one pipe.
    pub max_pipe_bytes: usize,
    /// Sum of capacities reserved by all live pipes.
    pub max_total_pipe_bytes: usize,
    /// Pipe descriptors installed in one process.
    pub max_descriptors_per_process: usize,
}

impl Default for RealmLimits {
    fn default() -> Self {
        Self {
            max_processes: 64,
            max_pipes: 64,
            max_pipe_bytes: 64 * 1024,
            max_total_pipe_bytes: 1024 * 1024,
            max_descriptors_per_process: 64,
        }
    }
}

/// Quota whose admission check failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quota {
    /// Process-table records.
    Processes,
    /// Live pipe objects.
    Pipes,
    /// One pipe's byte capacity.
    PipeBytes,
    /// Aggregate reserved pipe capacity.
    TotalPipeBytes,
    /// Descriptors installed in one process.
    Descriptors,
}

/// Lifecycle operation rejected for the process's current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessOperation {
    /// Admit a created process.
    Admit,
    /// Yield a running process.
    Yield,
    /// Exit normally.
    Exit,
    /// Terminate with a signal.
    Signal,
    /// Spawn a child.
    SpawnChild,
    /// Wait for a child.
    WaitChild,
    /// Create or operate on a descriptor.
    Descriptor,
    /// Park for pipe readiness.
    Park,
    /// Reap a supervisor-owned process.
    Reap,
}

/// Stable failures from the realm kernel model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernelError {
    /// No process has this identity.
    ProcessNotFound(ProcessId),
    /// No pipe has this identity.
    PipeNotFound(PipeId),
    /// A descriptor is absent from this process.
    DescriptorNotFound {
        /// Process containing the descriptor table.
        process: ProcessId,
        /// Requested descriptor.
        descriptor: Descriptor,
    },
    /// The descriptor exists but is not the required endpoint type.
    WrongDescriptorKind {
        /// Process containing the descriptor table.
        process: ProcessId,
        /// Requested descriptor.
        descriptor: Descriptor,
    },
    /// The target is not a direct child of the waiting process.
    NotChild {
        /// Waiting parent.
        parent: ProcessId,
        /// Requested child.
        child: ProcessId,
    },
    /// The operation is invalid for the current process state.
    InvalidTransition {
        /// Process being changed.
        process: ProcessId,
        /// Current state.
        state: ProcessState,
        /// Requested operation.
        operation: ProcessOperation,
    },
    /// A process working directory was not a normalized absolute guest path.
    InvalidCwd,
    /// A pipe capacity was zero or overflowed accounting.
    InvalidPipeCapacity,
    /// A child descriptor target was negative.
    InvalidDescriptorTarget(Descriptor),
    /// Two inheritance bindings selected the same child descriptor.
    DuplicateDescriptorTarget(Descriptor),
    /// A configured realm quota was exceeded.
    QuotaExceeded(Quota),
    /// A monotonic identifier reached the end of its representation.
    IdentifierExhausted,
    /// A live process cannot be reaped.
    ProcessStillLive(ProcessId),
    /// Only a supervisor-owned process can be reaped without a parent wait.
    ProcessHasParent(ProcessId),
}

impl fmt::Display for KernelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProcessNotFound(process) => {
                write!(formatter, "process {} does not exist", process.get())
            }
            Self::PipeNotFound(pipe) => write!(formatter, "pipe {} does not exist", pipe.get()),
            Self::DescriptorNotFound {
                process,
                descriptor,
            } => write!(
                formatter,
                "process {} has no descriptor {}",
                process.get(),
                descriptor.get()
            ),
            Self::WrongDescriptorKind {
                process,
                descriptor,
            } => write!(
                formatter,
                "process {} descriptor {} has the wrong resource type",
                process.get(),
                descriptor.get()
            ),
            Self::NotChild { parent, child } => write!(
                formatter,
                "process {} is not a direct child of {}",
                child.get(),
                parent.get()
            ),
            Self::InvalidTransition {
                process,
                state,
                operation,
            } => write!(
                formatter,
                "process {} cannot perform {operation:?} while {state:?}",
                process.get()
            ),
            Self::InvalidCwd => formatter.write_str("process cwd is not a normalized guest path"),
            Self::InvalidPipeCapacity => formatter.write_str("pipe capacity is invalid"),
            Self::InvalidDescriptorTarget(descriptor) => {
                write!(formatter, "invalid child descriptor {}", descriptor.get())
            }
            Self::DuplicateDescriptorTarget(descriptor) => write!(
                formatter,
                "child descriptor {} is mapped more than once",
                descriptor.get()
            ),
            Self::QuotaExceeded(quota) => write!(formatter, "realm {quota:?} quota exceeded"),
            Self::IdentifierExhausted => formatter.write_str("realm identifier space exhausted"),
            Self::ProcessStillLive(process) => {
                write!(formatter, "process {} is still live", process.get())
            }
            Self::ProcessHasParent(process) => write!(
                formatter,
                "process {} must be reaped by its parent",
                process.get()
            ),
        }
    }
}

impl std::error::Error for KernelError {}

/// Result of waiting for one direct child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitResult {
    /// The child is live and the parent was parked.
    Pending,
    /// The terminal child was removed from the process table.
    Reaped(Termination),
}

/// Result of one non-blocking pipe read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipeReadResult {
    /// Bytes removed from the pipe.
    Data(Vec<u8>),
    /// The pipe is empty but still has a writer.
    WouldBlock,
    /// The pipe is empty and its last writer is closed.
    Eof,
}

/// Result of one non-blocking pipe write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeWriteResult {
    /// Bytes appended; a partial count is valid.
    Written(usize),
    /// No capacity is currently available.
    WouldBlock,
    /// The pipe has no remaining reader.
    BrokenPipe,
}

/// Result of asking the scheduler to park for a pipe condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkResult {
    /// The operation can be retried immediately; the process remains running.
    Ready,
    /// The process entered `Waiting` and will be woken by a pipe transition.
    Parked,
}

struct ProcessRecord {
    parent: Option<ProcessId>,
    spec: ProcessSpec,
    state: ProcessState,
    descriptors: BTreeMap<Descriptor, DescriptorResource>,
}

struct Pipe {
    capacity: usize,
    bytes: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

/// Deterministic realm process and IPC state machine.
pub struct RealmKernel {
    limits: RealmLimits,
    processes: BTreeMap<ProcessId, ProcessRecord>,
    pipes: BTreeMap<PipeId, Pipe>,
    run_queue: VecDeque<ProcessId>,
    running: Option<ProcessId>,
    next_process_id: u64,
    next_pipe_id: u64,
    reserved_pipe_bytes: usize,
}

impl RealmKernel {
    /// Construct an empty realm kernel with explicit aggregate limits.
    #[must_use]
    pub fn new(limits: RealmLimits) -> Self {
        Self {
            limits,
            processes: BTreeMap::new(),
            pipes: BTreeMap::new(),
            run_queue: VecDeque::new(),
            running: None,
            next_process_id: 1,
            next_pipe_id: 1,
            reserved_pipe_bytes: 0,
        }
    }

    /// Number of allocated process records, including zombies.
    #[must_use]
    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    /// Number of live pipe objects.
    #[must_use]
    pub fn pipe_count(&self) -> usize {
        self.pipes.len()
    }

    /// Aggregate pipe capacity currently reserved.
    #[must_use]
    pub fn reserved_pipe_bytes(&self) -> usize {
        self.reserved_pipe_bytes
    }

    /// Next process identity that will be allocated, or `None` after exhaustion.
    #[must_use]
    pub fn next_process_id(&self) -> Option<ProcessId> {
        (self.next_process_id != 0).then(|| ProcessId::new(self.next_process_id))
    }

    /// Snapshot one process without exposing mutable kernel state.
    pub fn process(&self, process: ProcessId) -> Result<ProcessSnapshot, KernelError> {
        let record = self
            .processes
            .get(&process)
            .ok_or(KernelError::ProcessNotFound(process))?;
        Ok(ProcessSnapshot {
            id: process,
            parent: record.parent,
            executable: record.spec.executable,
            cwd: record.spec.cwd.clone(),
            state: record.state,
            descriptors: record.descriptors.len(),
        })
    }

    /// Snapshot one pipe's accounting.
    pub fn pipe(&self, pipe: PipeId) -> Result<PipeSnapshot, KernelError> {
        let pipe = self
            .pipes
            .get(&pipe)
            .ok_or(KernelError::PipeNotFound(pipe))?;
        Ok(PipeSnapshot {
            capacity: pipe.capacity,
            buffered: pipe.bytes.len(),
            readers: pipe.readers,
            writers: pipe.writers,
        })
    }

    /// Inspect a process-local descriptor resource.
    pub fn descriptor(
        &self,
        process: ProcessId,
        descriptor: Descriptor,
    ) -> Result<DescriptorResource, KernelError> {
        self.processes
            .get(&process)
            .ok_or(KernelError::ProcessNotFound(process))?
            .descriptors
            .get(&descriptor)
            .copied()
            .ok_or(KernelError::DescriptorNotFound {
                process,
                descriptor,
            })
    }

    /// Allocate a process owned directly by the realm supervisor.
    pub fn spawn_root(&mut self, spec: ProcessSpec) -> Result<ProcessId, KernelError> {
        self.validate_spawn(&spec)?;
        self.insert_process(None, spec, BTreeMap::new())
    }

    /// Atomically allocate a direct child and inherit exact pipe descriptors.
    pub fn spawn_child(
        &mut self,
        parent: ProcessId,
        spec: ProcessSpec,
        bindings: &[DescriptorBinding],
    ) -> Result<ProcessId, KernelError> {
        self.require_state(parent, ProcessState::Running, ProcessOperation::SpawnChild)?;
        self.validate_spawn(&spec)?;

        let parent_record = self
            .processes
            .get(&parent)
            .ok_or(KernelError::ProcessNotFound(parent))?;
        let mut descriptors = BTreeMap::new();
        let mut targets = BTreeSet::new();
        for binding in bindings {
            if binding.target.get() < 0 {
                return Err(KernelError::InvalidDescriptorTarget(binding.target));
            }
            if !targets.insert(binding.target) {
                return Err(KernelError::DuplicateDescriptorTarget(binding.target));
            }
            let resource = parent_record
                .descriptors
                .get(&binding.source)
                .copied()
                .ok_or(KernelError::DescriptorNotFound {
                    process: parent,
                    descriptor: binding.source,
                })?;
            descriptors.insert(binding.target, resource);
        }
        if descriptors.len() > self.limits.max_descriptors_per_process {
            return Err(KernelError::QuotaExceeded(Quota::Descriptors));
        }

        self.validate_retained_resources(descriptors.values().copied())?;
        let child = self.insert_process(Some(parent), spec, descriptors.clone())?;
        for resource in descriptors.into_values() {
            self.retain_resource(resource);
        }
        Ok(child)
    }

    /// Move a created process onto the FIFO runnable queue.
    pub fn admit(&mut self, process: ProcessId) -> Result<(), KernelError> {
        self.require_state(process, ProcessState::Created, ProcessOperation::Admit)?;
        self.processes
            .get_mut(&process)
            .expect("process state checked")
            .state = ProcessState::Runnable;
        self.run_queue.push_back(process);
        Ok(())
    }

    /// Select the next runnable process in deterministic FIFO order.
    pub fn dispatch_next(&mut self) -> Option<ProcessId> {
        if self.running.is_some() {
            return None;
        }
        while let Some(process) = self.run_queue.pop_front() {
            let Some(record) = self.processes.get_mut(&process) else {
                continue;
            };
            if record.state == ProcessState::Runnable {
                record.state = ProcessState::Running;
                self.running = Some(process);
                return Some(process);
            }
        }
        None
    }

    /// Cooperatively return a running process to the tail of the run queue.
    pub fn yield_now(&mut self, process: ProcessId) -> Result<(), KernelError> {
        self.require_state(process, ProcessState::Running, ProcessOperation::Yield)?;
        self.processes
            .get_mut(&process)
            .expect("process state checked")
            .state = ProcessState::Runnable;
        self.running = None;
        self.run_queue.push_back(process);
        Ok(())
    }

    /// Exit a running process and retain its status until it is reaped.
    pub fn exit(&mut self, process: ProcessId, status: i32) -> Result<(), KernelError> {
        self.require_state(process, ProcessState::Running, ProcessOperation::Exit)?;
        self.terminate(process, Termination::Exited(status))
    }

    /// Terminate any live process with a realm signal.
    pub fn signal(&mut self, process: ProcessId, signal: Signal) -> Result<(), KernelError> {
        let state = self
            .processes
            .get(&process)
            .ok_or(KernelError::ProcessNotFound(process))?
            .state;
        if state.termination().is_some() {
            return Err(KernelError::InvalidTransition {
                process,
                state,
                operation: ProcessOperation::Signal,
            });
        }
        self.terminate(process, Termination::Signaled(signal))
    }

    /// Wait for a direct child, parking the running parent when necessary.
    pub fn wait_child(
        &mut self,
        parent: ProcessId,
        child: ProcessId,
    ) -> Result<WaitResult, KernelError> {
        self.require_state(parent, ProcessState::Running, ProcessOperation::WaitChild)?;
        let child_record = self
            .processes
            .get(&child)
            .ok_or(KernelError::ProcessNotFound(child))?;
        if child_record.parent != Some(parent) {
            return Err(KernelError::NotChild { parent, child });
        }
        if let Some(termination) = child_record.state.termination() {
            self.processes.remove(&child);
            return Ok(WaitResult::Reaped(termination));
        }
        self.processes
            .get_mut(&parent)
            .expect("parent state checked")
            .state = ProcessState::Waiting(WaitReason::Child(child));
        self.running = None;
        Ok(WaitResult::Pending)
    }

    /// Reap a terminated process owned directly by the realm supervisor.
    pub fn reap_root(&mut self, process: ProcessId) -> Result<Termination, KernelError> {
        let record = self
            .processes
            .get(&process)
            .ok_or(KernelError::ProcessNotFound(process))?;
        if record.parent.is_some() {
            return Err(KernelError::ProcessHasParent(process));
        }
        let termination = record
            .state
            .termination()
            .ok_or(KernelError::ProcessStillLive(process))?;
        self.processes.remove(&process);
        Ok(termination)
    }

    /// Create a positive-capacity pipe and install both endpoints in a process.
    pub fn create_pipe(
        &mut self,
        owner: ProcessId,
        capacity: usize,
    ) -> Result<PipeEnds, KernelError> {
        self.require_state(owner, ProcessState::Running, ProcessOperation::Descriptor)?;
        if capacity == 0 {
            return Err(KernelError::InvalidPipeCapacity);
        }
        if capacity > self.limits.max_pipe_bytes {
            return Err(KernelError::QuotaExceeded(Quota::PipeBytes));
        }
        if self.pipes.len() >= self.limits.max_pipes {
            return Err(KernelError::QuotaExceeded(Quota::Pipes));
        }
        let new_reserved = self
            .reserved_pipe_bytes
            .checked_add(capacity)
            .ok_or(KernelError::InvalidPipeCapacity)?;
        if new_reserved > self.limits.max_total_pipe_bytes {
            return Err(KernelError::QuotaExceeded(Quota::TotalPipeBytes));
        }
        let owner_record = self
            .processes
            .get(&owner)
            .ok_or(KernelError::ProcessNotFound(owner))?;
        if owner_record.descriptors.len().saturating_add(2)
            > self.limits.max_descriptors_per_process
        {
            return Err(KernelError::QuotaExceeded(Quota::Descriptors));
        }
        let descriptors = available_descriptors(owner_record, 2)?;
        let pipe_id = self.take_pipe_id()?;
        let ends = PipeEnds {
            read: descriptors[0],
            write: descriptors[1],
        };

        self.pipes.insert(
            pipe_id,
            Pipe {
                capacity,
                bytes: VecDeque::new(),
                readers: 1,
                writers: 1,
            },
        );
        self.reserved_pipe_bytes = new_reserved;
        let record = self.processes.get_mut(&owner).expect("owner checked");
        record
            .descriptors
            .insert(ends.read, DescriptorResource::PipeRead(pipe_id));
        record
            .descriptors
            .insert(ends.write, DescriptorResource::PipeWrite(pipe_id));
        Ok(ends)
    }

    /// Close one process descriptor and update pipe endpoint references.
    pub fn close_descriptor(
        &mut self,
        process: ProcessId,
        descriptor: Descriptor,
    ) -> Result<(), KernelError> {
        self.require_state(process, ProcessState::Running, ProcessOperation::Descriptor)?;
        let resource = self
            .processes
            .get_mut(&process)
            .expect("process state checked")
            .descriptors
            .remove(&descriptor)
            .ok_or(KernelError::DescriptorNotFound {
                process,
                descriptor,
            })?;
        self.release_resource(resource);
        Ok(())
    }

    /// Perform one non-blocking read from a pipe descriptor.
    pub fn read_pipe(
        &mut self,
        process: ProcessId,
        descriptor: Descriptor,
        max_bytes: usize,
    ) -> Result<PipeReadResult, KernelError> {
        self.require_state(process, ProcessState::Running, ProcessOperation::Descriptor)?;
        let pipe_id = self.pipe_for_read(process, descriptor)?;
        let result = {
            let pipe = self
                .pipes
                .get_mut(&pipe_id)
                .ok_or(KernelError::PipeNotFound(pipe_id))?;
            if max_bytes == 0 {
                PipeReadResult::Data(Vec::new())
            } else if pipe.bytes.is_empty() && pipe.writers == 0 {
                PipeReadResult::Eof
            } else if pipe.bytes.is_empty() {
                PipeReadResult::WouldBlock
            } else {
                let count = max_bytes.min(pipe.bytes.len());
                PipeReadResult::Data(pipe.bytes.drain(..count).collect())
            }
        };
        if matches!(&result, PipeReadResult::Data(bytes) if !bytes.is_empty()) {
            self.wake_waiters(WaitReason::PipeWritable(pipe_id));
        }
        Ok(result)
    }

    /// Perform one non-blocking, possibly partial write to a pipe descriptor.
    pub fn write_pipe(
        &mut self,
        process: ProcessId,
        descriptor: Descriptor,
        bytes: &[u8],
    ) -> Result<PipeWriteResult, KernelError> {
        self.require_state(process, ProcessState::Running, ProcessOperation::Descriptor)?;
        let pipe_id = self.pipe_for_write(process, descriptor)?;
        let result = {
            let pipe = self
                .pipes
                .get_mut(&pipe_id)
                .ok_or(KernelError::PipeNotFound(pipe_id))?;
            if bytes.is_empty() {
                PipeWriteResult::Written(0)
            } else if pipe.readers == 0 {
                PipeWriteResult::BrokenPipe
            } else {
                let available = pipe.capacity.saturating_sub(pipe.bytes.len());
                if available == 0 {
                    PipeWriteResult::WouldBlock
                } else {
                    let count = available.min(bytes.len());
                    pipe.bytes.extend(&bytes[..count]);
                    PipeWriteResult::Written(count)
                }
            }
        };
        if matches!(result, PipeWriteResult::Written(count) if count > 0) {
            self.wake_waiters(WaitReason::PipeReadable(pipe_id));
        }
        Ok(result)
    }

    /// Park a running process until its pipe read can return data or EOF.
    pub fn park_pipe_read(
        &mut self,
        process: ProcessId,
        descriptor: Descriptor,
    ) -> Result<ParkResult, KernelError> {
        self.require_state(process, ProcessState::Running, ProcessOperation::Park)?;
        let pipe_id = self.pipe_for_read(process, descriptor)?;
        let pipe = self
            .pipes
            .get(&pipe_id)
            .ok_or(KernelError::PipeNotFound(pipe_id))?;
        if !pipe.bytes.is_empty() || pipe.writers == 0 {
            return Ok(ParkResult::Ready);
        }
        self.processes
            .get_mut(&process)
            .expect("process state checked")
            .state = ProcessState::Waiting(WaitReason::PipeReadable(pipe_id));
        self.running = None;
        Ok(ParkResult::Parked)
    }

    /// Park a running process until its pipe write can use capacity or fail broken.
    pub fn park_pipe_write(
        &mut self,
        process: ProcessId,
        descriptor: Descriptor,
    ) -> Result<ParkResult, KernelError> {
        self.require_state(process, ProcessState::Running, ProcessOperation::Park)?;
        let pipe_id = self.pipe_for_write(process, descriptor)?;
        let pipe = self
            .pipes
            .get(&pipe_id)
            .ok_or(KernelError::PipeNotFound(pipe_id))?;
        if pipe.readers == 0 || pipe.bytes.len() < pipe.capacity {
            return Ok(ParkResult::Ready);
        }
        self.processes
            .get_mut(&process)
            .expect("process state checked")
            .state = ProcessState::Waiting(WaitReason::PipeWritable(pipe_id));
        self.running = None;
        Ok(ParkResult::Parked)
    }

    fn validate_spawn(&self, spec: &ProcessSpec) -> Result<(), KernelError> {
        if self.processes.len() >= self.limits.max_processes {
            return Err(KernelError::QuotaExceeded(Quota::Processes));
        }
        validate_cwd(&spec.cwd)
    }

    fn insert_process(
        &mut self,
        parent: Option<ProcessId>,
        spec: ProcessSpec,
        descriptors: BTreeMap<Descriptor, DescriptorResource>,
    ) -> Result<ProcessId, KernelError> {
        let process = self.take_process_id()?;
        self.processes.insert(
            process,
            ProcessRecord {
                parent,
                spec,
                state: ProcessState::Created,
                descriptors,
            },
        );
        Ok(process)
    }

    fn take_process_id(&mut self) -> Result<ProcessId, KernelError> {
        let raw = self.next_process_id;
        if raw == 0 {
            return Err(KernelError::IdentifierExhausted);
        }
        self.next_process_id = raw.checked_add(1).unwrap_or(0);
        Ok(ProcessId::new(raw))
    }

    fn take_pipe_id(&mut self) -> Result<PipeId, KernelError> {
        let raw = self.next_pipe_id;
        if raw == 0 {
            return Err(KernelError::IdentifierExhausted);
        }
        self.next_pipe_id = raw.checked_add(1).unwrap_or(0);
        Ok(PipeId::new(raw))
    }

    fn require_state(
        &self,
        process: ProcessId,
        expected: ProcessState,
        operation: ProcessOperation,
    ) -> Result<(), KernelError> {
        let state = self
            .processes
            .get(&process)
            .ok_or(KernelError::ProcessNotFound(process))?
            .state;
        if state == expected {
            Ok(())
        } else {
            Err(KernelError::InvalidTransition {
                process,
                state,
                operation,
            })
        }
    }

    fn terminate(
        &mut self,
        process: ProcessId,
        termination: Termination,
    ) -> Result<(), KernelError> {
        let descriptors = {
            let record = self
                .processes
                .get_mut(&process)
                .ok_or(KernelError::ProcessNotFound(process))?;
            std::mem::take(&mut record.descriptors)
        };
        for resource in descriptors.into_values() {
            self.release_resource(resource);
        }
        for record in self.processes.values_mut() {
            if record.parent == Some(process) {
                record.parent = None;
            }
        }
        self.processes
            .get_mut(&process)
            .expect("process exists during termination")
            .state = match termination {
            Termination::Exited(status) => ProcessState::Exited(status),
            Termination::Signaled(signal) => ProcessState::Signaled(signal),
        };
        if self.running == Some(process) {
            self.running = None;
        }
        self.wake_waiters(WaitReason::Child(process));
        Ok(())
    }

    fn validate_retained_resources(
        &self,
        resources: impl IntoIterator<Item = DescriptorResource>,
    ) -> Result<(), KernelError> {
        let mut increments = BTreeMap::<PipeId, (usize, usize)>::new();
        for resource in resources {
            let entry = match resource {
                DescriptorResource::PipeRead(pipe) => increments.entry(pipe).or_default(),
                DescriptorResource::PipeWrite(pipe) => increments.entry(pipe).or_default(),
            };
            match resource {
                DescriptorResource::PipeRead(_) => {
                    entry.0 = entry
                        .0
                        .checked_add(1)
                        .ok_or(KernelError::IdentifierExhausted)?;
                }
                DescriptorResource::PipeWrite(_) => {
                    entry.1 = entry
                        .1
                        .checked_add(1)
                        .ok_or(KernelError::IdentifierExhausted)?;
                }
            }
        }
        for (pipe_id, (readers, writers)) in increments {
            let pipe = self
                .pipes
                .get(&pipe_id)
                .ok_or(KernelError::PipeNotFound(pipe_id))?;
            pipe.readers
                .checked_add(readers)
                .ok_or(KernelError::IdentifierExhausted)?;
            pipe.writers
                .checked_add(writers)
                .ok_or(KernelError::IdentifierExhausted)?;
        }
        Ok(())
    }

    fn retain_resource(&mut self, resource: DescriptorResource) {
        match resource {
            DescriptorResource::PipeRead(pipe_id) => {
                let pipe = self
                    .pipes
                    .get_mut(&pipe_id)
                    .expect("inherited read pipe validated before child insertion");
                pipe.readers = pipe
                    .readers
                    .checked_add(1)
                    .expect("read endpoint count validated before child insertion");
            }
            DescriptorResource::PipeWrite(pipe_id) => {
                let pipe = self
                    .pipes
                    .get_mut(&pipe_id)
                    .expect("inherited write pipe validated before child insertion");
                pipe.writers = pipe
                    .writers
                    .checked_add(1)
                    .expect("write endpoint count validated before child insertion");
            }
        }
    }

    fn release_resource(&mut self, resource: DescriptorResource) {
        let (pipe_id, wake_reason, remove, capacity) = match resource {
            DescriptorResource::PipeRead(pipe_id) => {
                let pipe = self
                    .pipes
                    .get_mut(&pipe_id)
                    .expect("open read descriptor always references a live pipe");
                pipe.readers = pipe
                    .readers
                    .checked_sub(1)
                    .expect("read endpoint accounting is balanced");
                (
                    pipe_id,
                    (pipe.readers == 0).then_some(WaitReason::PipeWritable(pipe_id)),
                    pipe.readers == 0 && pipe.writers == 0,
                    pipe.capacity,
                )
            }
            DescriptorResource::PipeWrite(pipe_id) => {
                let pipe = self
                    .pipes
                    .get_mut(&pipe_id)
                    .expect("open write descriptor always references a live pipe");
                pipe.writers = pipe
                    .writers
                    .checked_sub(1)
                    .expect("write endpoint accounting is balanced");
                (
                    pipe_id,
                    (pipe.writers == 0).then_some(WaitReason::PipeReadable(pipe_id)),
                    pipe.readers == 0 && pipe.writers == 0,
                    pipe.capacity,
                )
            }
        };
        if remove {
            self.pipes.remove(&pipe_id);
            self.reserved_pipe_bytes = self
                .reserved_pipe_bytes
                .checked_sub(capacity)
                .expect("pipe capacity accounting is balanced");
        }
        if let Some(reason) = wake_reason {
            self.wake_waiters(reason);
        }
    }

    fn pipe_for_read(
        &self,
        process: ProcessId,
        descriptor: Descriptor,
    ) -> Result<PipeId, KernelError> {
        match self.descriptor(process, descriptor)? {
            DescriptorResource::PipeRead(pipe) => Ok(pipe),
            DescriptorResource::PipeWrite(_) => Err(KernelError::WrongDescriptorKind {
                process,
                descriptor,
            }),
        }
    }

    fn pipe_for_write(
        &self,
        process: ProcessId,
        descriptor: Descriptor,
    ) -> Result<PipeId, KernelError> {
        match self.descriptor(process, descriptor)? {
            DescriptorResource::PipeWrite(pipe) => Ok(pipe),
            DescriptorResource::PipeRead(_) => Err(KernelError::WrongDescriptorKind {
                process,
                descriptor,
            }),
        }
    }

    fn wake_waiters(&mut self, reason: WaitReason) {
        let mut woken = Vec::new();
        for (process, record) in &mut self.processes {
            if record.state == ProcessState::Waiting(reason) {
                record.state = ProcessState::Runnable;
                woken.push(*process);
            }
        }
        self.run_queue.extend(woken);
    }
}

fn validate_cwd(cwd: &str) -> Result<(), KernelError> {
    if cwd.is_empty()
        || cwd.len() > MAX_PATH_BYTES
        || !cwd.starts_with('/')
        || cwd.contains('\\')
        || cwd.chars().any(char::is_control)
    {
        return Err(KernelError::InvalidCwd);
    }
    let tail = &cwd[1..];
    if !tail.is_empty()
        && tail
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(KernelError::InvalidCwd);
    }
    Ok(())
}

fn available_descriptors(
    record: &ProcessRecord,
    count: usize,
) -> Result<Vec<Descriptor>, KernelError> {
    let mut result = Vec::with_capacity(count);
    let mut raw = FIRST_FILE_FD;
    while result.len() < count {
        let descriptor = Descriptor::new(raw);
        if !record.descriptors.contains_key(&descriptor) {
            result.push(descriptor);
        }
        raw = raw.checked_add(1).ok_or(KernelError::IdentifierExhausted)?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(byte: u8) -> ExecutableId {
        ExecutableId::new([byte; 32])
    }

    fn spec(byte: u8) -> ProcessSpec {
        ProcessSpec::new(image(byte), "/workspace")
    }

    fn running_root(kernel: &mut RealmKernel, byte: u8) -> ProcessId {
        let process = kernel.spawn_root(spec(byte)).expect("root spawns");
        kernel.admit(process).expect("root admits");
        assert_eq!(kernel.dispatch_next(), Some(process));
        process
    }

    fn pipe_id(kernel: &RealmKernel, process: ProcessId, descriptor: Descriptor) -> PipeId {
        match kernel.descriptor(process, descriptor).expect("descriptor") {
            DescriptorResource::PipeRead(pipe) | DescriptorResource::PipeWrite(pipe) => pipe,
        }
    }

    #[test]
    fn lifecycle_is_explicit_fifo_and_identifiers_are_not_reused() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let first = kernel.spawn_root(spec(1)).expect("first spawns");
        let second = kernel.spawn_root(spec(2)).expect("second spawns");
        kernel.admit(first).expect("first admits");
        kernel.admit(second).expect("second admits");

        assert_eq!(kernel.dispatch_next(), Some(first));
        assert_eq!(kernel.dispatch_next(), None);
        kernel.yield_now(first).expect("first yields");
        assert_eq!(kernel.dispatch_next(), Some(second));
        kernel.exit(second, 7).expect("second exits");
        assert_eq!(kernel.dispatch_next(), Some(first));
        kernel.exit(first, 0).expect("first exits");
        assert_eq!(kernel.reap_root(first), Ok(Termination::Exited(0)));
        assert_eq!(kernel.reap_root(second), Ok(Termination::Exited(7)));
        assert!(ProcessState::Exited(7).is_terminal());
        assert!(!ProcessState::Runnable.is_terminal());

        let third = kernel.spawn_root(spec(3)).expect("third spawns");
        assert!(third.get() > second.get());
        assert_eq!(kernel.next_process_id().map(ProcessId::get), Some(4));
    }

    #[test]
    fn invalid_lifecycle_transition_does_not_mutate_state() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let process = kernel.spawn_root(spec(1)).expect("root spawns");

        assert!(matches!(
            kernel.exit(process, 0),
            Err(KernelError::InvalidTransition { .. })
        ));
        assert_eq!(
            kernel.process(process).expect("process").state,
            ProcessState::Created
        );
        assert_eq!(kernel.dispatch_next(), None);
    }

    #[test]
    fn child_wait_parks_wakes_and_reaps_only_the_direct_child() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let parent = running_root(&mut kernel, 1);
        let child = kernel
            .spawn_child(parent, spec(2), &[])
            .expect("child spawns");
        let unrelated = kernel.spawn_root(spec(3)).expect("other root spawns");
        kernel.admit(child).expect("child admits");
        kernel.admit(unrelated).expect("other root admits");

        assert_eq!(
            kernel.wait_child(parent, unrelated),
            Err(KernelError::NotChild {
                parent,
                child: unrelated,
            })
        );
        assert_eq!(kernel.wait_child(parent, child), Ok(WaitResult::Pending));
        assert_eq!(kernel.dispatch_next(), Some(child));
        kernel.exit(child, 23).expect("child exits");
        assert_eq!(kernel.dispatch_next(), Some(unrelated));
        kernel.yield_now(unrelated).expect("other root yields");
        assert_eq!(kernel.dispatch_next(), Some(parent));
        assert_eq!(
            kernel.wait_child(parent, child),
            Ok(WaitResult::Reaped(Termination::Exited(23)))
        );
        assert!(matches!(
            kernel.process(child),
            Err(KernelError::ProcessNotFound(_))
        ));
    }

    #[test]
    fn parent_termination_reparents_children_and_closes_its_descriptors() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let parent = running_root(&mut kernel, 1);
        let ends = kernel.create_pipe(parent, 8).expect("pipe creates");
        let child = kernel
            .spawn_child(
                parent,
                spec(2),
                &[DescriptorBinding {
                    source: ends.read,
                    target: Descriptor::STDIN,
                }],
            )
            .expect("child spawns");
        let pipe = pipe_id(&kernel, parent, ends.read);

        kernel
            .signal(parent, Signal::Kill)
            .expect("parent terminates");

        assert_eq!(kernel.process(child).expect("child remains").parent, None);
        assert_eq!(kernel.pipe(pipe).expect("pipe remains").readers, 1);
        assert_eq!(kernel.pipe(pipe).expect("pipe remains").writers, 0);
        assert_eq!(
            kernel.reap_root(parent),
            Ok(Termination::Signaled(Signal::Kill))
        );
    }

    #[test]
    fn process_quota_counts_zombies_until_reaped() {
        let limits = RealmLimits {
            max_processes: 1,
            ..RealmLimits::default()
        };
        let mut kernel = RealmKernel::new(limits);
        let process = running_root(&mut kernel, 1);
        kernel.exit(process, 0).expect("root exits");

        assert_eq!(
            kernel.spawn_root(spec(2)),
            Err(KernelError::QuotaExceeded(Quota::Processes))
        );
        kernel.reap_root(process).expect("root reaps");
        assert!(kernel.spawn_root(spec(2)).is_ok());
    }

    #[test]
    fn bounded_pipe_applies_partial_write_backpressure_and_fifo_reads() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let process = running_root(&mut kernel, 1);
        let ends = kernel.create_pipe(process, 4).expect("pipe creates");

        assert_eq!(
            kernel.write_pipe(process, ends.write, b"abcdef"),
            Ok(PipeWriteResult::Written(4))
        );
        assert_eq!(
            kernel.write_pipe(process, ends.write, b"z"),
            Ok(PipeWriteResult::WouldBlock)
        );
        assert_eq!(
            kernel.read_pipe(process, ends.read, 2),
            Ok(PipeReadResult::Data(b"ab".to_vec()))
        );
        assert_eq!(
            kernel.write_pipe(process, ends.write, b"xy"),
            Ok(PipeWriteResult::Written(2))
        );
        assert_eq!(
            kernel.read_pipe(process, ends.read, 8),
            Ok(PipeReadResult::Data(b"cdxy".to_vec()))
        );
    }

    #[test]
    fn eof_is_visible_only_after_the_last_writer_and_buffer_drain() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let parent = running_root(&mut kernel, 1);
        let ends = kernel.create_pipe(parent, 8).expect("pipe creates");
        let child = kernel
            .spawn_child(
                parent,
                spec(2),
                &[DescriptorBinding {
                    source: ends.write,
                    target: Descriptor::STDOUT,
                }],
            )
            .expect("child spawns");
        kernel
            .write_pipe(parent, ends.write, b"data")
            .expect("write succeeds");
        kernel
            .close_descriptor(parent, ends.write)
            .expect("parent writer closes");
        assert_eq!(
            kernel.read_pipe(parent, ends.read, 8),
            Ok(PipeReadResult::Data(b"data".to_vec()))
        );
        assert_eq!(
            kernel.read_pipe(parent, ends.read, 8),
            Ok(PipeReadResult::WouldBlock)
        );

        kernel.admit(child).expect("child admits");
        kernel.yield_now(parent).expect("parent yields");
        assert_eq!(kernel.dispatch_next(), Some(child));
        kernel
            .exit(child, 0)
            .expect("child closes inherited writer");
        assert_eq!(kernel.dispatch_next(), Some(parent));
        assert_eq!(
            kernel.read_pipe(parent, ends.read, 8),
            Ok(PipeReadResult::Eof)
        );
    }

    #[test]
    fn final_reader_close_yields_broken_pipe_and_wakes_a_writer() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let parent = running_root(&mut kernel, 1);
        let ends = kernel.create_pipe(parent, 1).expect("pipe creates");
        let child = kernel
            .spawn_child(
                parent,
                spec(2),
                &[DescriptorBinding {
                    source: ends.write,
                    target: Descriptor::STDOUT,
                }],
            )
            .expect("child spawns");
        kernel.admit(child).expect("child admits");
        kernel.yield_now(parent).expect("parent yields");
        assert_eq!(kernel.dispatch_next(), Some(child));
        assert_eq!(
            kernel.write_pipe(child, Descriptor::STDOUT, b"x"),
            Ok(PipeWriteResult::Written(1))
        );
        assert_eq!(
            kernel.park_pipe_write(child, Descriptor::STDOUT),
            Ok(ParkResult::Parked)
        );
        assert_eq!(kernel.dispatch_next(), Some(parent));
        kernel
            .close_descriptor(parent, ends.read)
            .expect("last reader closes");
        kernel.yield_now(parent).expect("parent yields");
        assert_eq!(kernel.dispatch_next(), Some(child));
        assert_eq!(
            kernel.write_pipe(child, Descriptor::STDOUT, b"y"),
            Ok(PipeWriteResult::BrokenPipe)
        );
    }

    #[test]
    fn pipe_reader_frees_capacity_and_wakes_a_parked_writer() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let writer = running_root(&mut kernel, 1);
        let ends = kernel.create_pipe(writer, 1).expect("pipe creates");
        let reader = kernel
            .spawn_child(
                writer,
                spec(2),
                &[DescriptorBinding {
                    source: ends.read,
                    target: Descriptor::STDIN,
                }],
            )
            .expect("reader spawns");
        kernel.admit(reader).expect("reader admits");
        kernel
            .close_descriptor(writer, ends.read)
            .expect("writer drops its unused reader");
        assert_eq!(
            kernel.write_pipe(writer, ends.write, b"x"),
            Ok(PipeWriteResult::Written(1))
        );
        assert_eq!(
            kernel.park_pipe_write(writer, ends.write),
            Ok(ParkResult::Parked)
        );
        assert_eq!(kernel.dispatch_next(), Some(reader));
        assert_eq!(
            kernel.read_pipe(reader, Descriptor::STDIN, 1),
            Ok(PipeReadResult::Data(b"x".to_vec()))
        );
        kernel.yield_now(reader).expect("reader yields");
        assert_eq!(kernel.dispatch_next(), Some(writer));
        assert_eq!(
            kernel.write_pipe(writer, ends.write, b"y"),
            Ok(PipeWriteResult::Written(1))
        );
    }

    #[test]
    fn pipe_read_waiter_wakes_for_data_and_for_eof() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let reader = running_root(&mut kernel, 1);
        let ends = kernel.create_pipe(reader, 8).expect("pipe creates");
        let writer = kernel
            .spawn_child(
                reader,
                spec(2),
                &[DescriptorBinding {
                    source: ends.write,
                    target: Descriptor::STDOUT,
                }],
            )
            .expect("writer spawns");
        kernel.admit(writer).expect("writer admits");
        kernel
            .close_descriptor(reader, ends.write)
            .expect("reader drops its unused writer");
        assert_eq!(
            kernel.park_pipe_read(reader, ends.read),
            Ok(ParkResult::Parked)
        );
        assert_eq!(kernel.dispatch_next(), Some(writer));
        kernel
            .write_pipe(writer, Descriptor::STDOUT, b"x")
            .expect("writer writes");
        kernel.yield_now(writer).expect("writer yields");
        assert_eq!(kernel.dispatch_next(), Some(reader));
        assert_eq!(
            kernel.read_pipe(reader, ends.read, 1),
            Ok(PipeReadResult::Data(b"x".to_vec()))
        );
        assert_eq!(
            kernel.park_pipe_read(reader, ends.read),
            Ok(ParkResult::Parked)
        );
        assert_eq!(kernel.dispatch_next(), Some(writer));
        kernel.exit(writer, 0).expect("writer exits");
        assert_eq!(kernel.dispatch_next(), Some(reader));
        assert_eq!(
            kernel.read_pipe(reader, ends.read, 1),
            Ok(PipeReadResult::Eof)
        );
    }

    #[test]
    fn invalid_inheritance_is_atomic() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        let parent = running_root(&mut kernel, 1);
        let ends = kernel.create_pipe(parent, 8).expect("pipe creates");
        let pipe = pipe_id(&kernel, parent, ends.read);
        let before = kernel.pipe(pipe).expect("pipe snapshot");
        let process_count = kernel.process_count();

        let error = kernel
            .spawn_child(
                parent,
                spec(2),
                &[
                    DescriptorBinding {
                        source: ends.read,
                        target: Descriptor::STDIN,
                    },
                    DescriptorBinding {
                        source: ends.write,
                        target: Descriptor::STDIN,
                    },
                ],
            )
            .expect_err("duplicate child target fails");

        assert_eq!(
            error,
            KernelError::DuplicateDescriptorTarget(Descriptor::STDIN)
        );
        assert_eq!(kernel.process_count(), process_count);
        assert_eq!(kernel.pipe(pipe), Ok(before));
    }

    #[test]
    fn pipe_quota_failure_is_atomic_and_capacity_is_released_on_last_close() {
        let limits = RealmLimits {
            max_pipes: 1,
            max_pipe_bytes: 8,
            max_total_pipe_bytes: 8,
            ..RealmLimits::default()
        };
        let mut kernel = RealmKernel::new(limits);
        let process = running_root(&mut kernel, 1);
        let ends = kernel.create_pipe(process, 8).expect("pipe creates");
        let descriptor_count = kernel.process(process).expect("process").descriptors;

        assert_eq!(
            kernel.create_pipe(process, 1),
            Err(KernelError::QuotaExceeded(Quota::Pipes))
        );
        assert_eq!(
            kernel.process(process).expect("process").descriptors,
            descriptor_count
        );
        assert_eq!(kernel.reserved_pipe_bytes(), 8);

        kernel
            .close_descriptor(process, ends.read)
            .expect("reader closes");
        assert_eq!(kernel.pipe_count(), 1);
        kernel
            .close_descriptor(process, ends.write)
            .expect("writer closes");
        assert_eq!(kernel.pipe_count(), 0);
        assert_eq!(kernel.reserved_pipe_bytes(), 0);
        assert!(kernel.create_pipe(process, 8).is_ok());
    }

    #[test]
    fn descriptor_quota_failure_does_not_allocate_a_pipe() {
        let limits = RealmLimits {
            max_descriptors_per_process: 1,
            ..RealmLimits::default()
        };
        let mut kernel = RealmKernel::new(limits);
        let process = running_root(&mut kernel, 1);

        assert_eq!(
            kernel.create_pipe(process, 8),
            Err(KernelError::QuotaExceeded(Quota::Descriptors))
        );
        assert_eq!(kernel.pipe_count(), 0);
        assert_eq!(kernel.reserved_pipe_bytes(), 0);
    }

    #[test]
    fn malformed_working_directories_fail_before_pid_allocation() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        for cwd in ["relative", "/workspace/../secret", "/workspace//project"] {
            assert_eq!(
                kernel.spawn_root(ProcessSpec::new(image(1), cwd)),
                Err(KernelError::InvalidCwd)
            );
        }
        let process = kernel.spawn_root(spec(2)).expect("valid root spawns");
        assert_eq!(process.get(), 1);
    }

    #[test]
    fn identifier_exhaustion_never_wraps_or_partially_allocates() {
        let mut kernel = RealmKernel::new(RealmLimits::default());
        kernel.next_process_id = u64::MAX;
        let final_process = kernel.spawn_root(spec(1)).expect("last PID allocates");
        assert_eq!(final_process.get(), u64::MAX);
        assert_eq!(
            kernel.spawn_root(spec(2)),
            Err(KernelError::IdentifierExhausted)
        );
        assert_eq!(kernel.next_process_id(), None);
        assert_eq!(kernel.process_count(), 1);

        kernel.admit(final_process).expect("last process admits");
        assert_eq!(kernel.dispatch_next(), Some(final_process));
        kernel.next_pipe_id = 0;
        assert_eq!(
            kernel.create_pipe(final_process, 8),
            Err(KernelError::IdentifierExhausted)
        );
        assert_eq!(kernel.pipe_count(), 0);
        assert_eq!(
            kernel
                .process(final_process)
                .expect("process remains")
                .descriptors,
            0
        );
    }
}
