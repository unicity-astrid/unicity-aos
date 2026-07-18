//! Resumable two-process execution over the realm-core scheduler and one pipe.

use super::*;
use wasmi::{TypedResumableCall, TypedResumableCallHostTrap};

/// One completed process from a pipeline execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineProcessReport {
    /// Realm-local process identity for this machine run.
    pub process_id: ProcessId,
    /// Terminal result and resource accounting.
    pub execution: ExecutionReport,
}

/// Completed producer and consumer reports from a two-stage pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineReport {
    /// Process whose standard output was connected to the pipe writer.
    pub producer: PipelineProcessReport,
    /// Process whose standard input was connected to the pipe reader.
    pub consumer: PipelineProcessReport,
}

/// Observable accounting for one long-lived realm machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealmMachineStatus {
    /// Process records currently retained by the semantic kernel.
    pub process_records: usize,
    /// Pipe objects currently retained by the semantic kernel.
    pub pipe_objects: usize,
    /// Aggregate capacity reserved by retained pipes.
    pub reserved_pipe_bytes: usize,
    /// Next monotonic process identity, or `None` after exhaustion.
    pub next_process_id: Option<ProcessId>,
}

/// Principal-local interpreter and semantic-kernel owner.
///
/// A machine is intended to live for one Realm boot. Completed foreground
/// processes are reaped, but their identifiers are never reused during that
/// boot. Active process state is never shared between `RealmMachine` values.
pub struct RealmMachine {
    runtime: RealmRuntime,
    kernel: Rc<RefCell<RealmKernel>>,
    limits: RealmLimits,
}

impl Default for RealmMachine {
    fn default() -> Self {
        Self::new(RealmLimits {
            max_processes: 64,
            max_pipes: 64,
            max_pipe_bytes: MAX_IO_BYTES,
            max_total_pipe_bytes: MAX_IO_BYTES * 16,
            max_descriptors_per_process: 64,
        })
    }
}

impl RealmMachine {
    /// Create a machine with explicit semantic-kernel quotas.
    #[must_use]
    pub fn new(limits: RealmLimits) -> Self {
        Self {
            runtime: RealmRuntime::default(),
            kernel: Rc::new(RefCell::new(RealmKernel::new(limits))),
            limits,
        }
    }

    /// Snapshot live kernel accounting without exposing mutable process state.
    #[must_use]
    pub fn status(&self) -> RealmMachineStatus {
        let kernel = self.kernel.borrow();
        RealmMachineStatus {
            process_records: kernel.process_count(),
            pipe_objects: kernel.pipe_count(),
            reserved_pipe_bytes: kernel.reserved_pipe_bytes(),
            next_process_id: kernel.next_process_id(),
        }
    }

    /// Execute and reap one foreground process in this machine.
    pub fn execute_process(
        &mut self,
        wasm: &[u8],
        process: ProcessConfig,
        limits: RunLimits,
        realm_host: Box<dyn RealmHost>,
    ) -> Result<PipelineProcessReport, PipelineError> {
        // Admission and instantiation happen before kernel mutation so a bad
        // module cannot consume a PID or strand a process record.
        let (store, start) =
            self.runtime
                .prepare_process(wasm, process.clone(), limits, realm_host, None)?;
        let process_id = {
            let mut kernel = self.kernel.borrow_mut();
            let process_id = kernel.spawn_root(process_spec(wasm, &process))?;
            if let Err(error) = kernel.admit(process_id) {
                abort_processes(&mut kernel, &[process_id])?;
                return Err(error.into());
            }
            if kernel.dispatch_next() != Some(process_id) {
                abort_processes(&mut kernel, &[process_id])?;
                return Err(PipelineError::Deadlock);
            }
            process_id
        };

        let mut slot = ProcessSlot {
            store,
            invocation: InvocationState::Start(start),
            limits,
            report: None,
        };
        attach_process(&mut slot, process_id, &self.kernel);
        if let Err(error) = drive_slot(process_id, &mut slot, &self.kernel) {
            abort_processes(&mut self.kernel.borrow_mut(), &[process_id])?;
            return Err(error);
        }
        if slot.report.is_none() {
            abort_processes(&mut self.kernel.borrow_mut(), &[process_id])?;
            return Err(PipelineError::Deadlock);
        }
        let execution = slot
            .report
            .take()
            .ok_or(PipelineError::MissingReport(process_id))?;
        self.kernel.borrow_mut().reap_root(process_id)?;
        Ok(PipelineProcessReport {
            process_id,
            execution,
        })
    }

    /// Execute and reap two foreground processes connected by one bounded pipe.
    pub fn execute_pipeline(
        &mut self,
        producer_wasm: &[u8],
        producer: ProcessConfig,
        consumer_wasm: &[u8],
        consumer: ProcessConfig,
        limits: RunLimits,
        pipe_capacity: usize,
    ) -> Result<PipelineReport, PipelineError> {
        self.preflight_pipeline(pipe_capacity)?;
        let producer_limits = RunLimits {
            fuel: limits.fuel / 2,
            memory_bytes: limits.memory_bytes,
            output_bytes: limits.output_bytes / 2,
        };
        let consumer_limits = RunLimits {
            fuel: limits.fuel.saturating_sub(producer_limits.fuel),
            memory_bytes: limits.memory_bytes,
            output_bytes: limits
                .output_bytes
                .saturating_sub(producer_limits.output_bytes),
        };

        // Prepare both stores before allocating any semantic-kernel resource.
        let (consumer_store, consumer_start) = self.runtime.prepare_process(
            consumer_wasm,
            consumer.clone(),
            consumer_limits,
            Box::<DenyRealmHost>::default(),
            None,
        )?;
        let (producer_store, producer_start) = self.runtime.prepare_process(
            producer_wasm,
            producer.clone(),
            producer_limits,
            Box::<DenyRealmHost>::default(),
            None,
        )?;

        let mut allocated = Vec::with_capacity(3);
        let setup = (|| -> Result<(ProcessId, ProcessId), PipelineError> {
            let mut kernel = self.kernel.borrow_mut();
            let supervisor =
                kernel.spawn_root(ProcessSpec::new(ExecutableId::REALM_SUPERVISOR, "/"))?;
            allocated.push(supervisor);
            kernel.admit(supervisor)?;
            if kernel.dispatch_next() != Some(supervisor) {
                return Err(PipelineError::Deadlock);
            }
            let ends = kernel.create_pipe(supervisor, pipe_capacity)?;
            let consumer_id = kernel.spawn_child(
                supervisor,
                process_spec(consumer_wasm, &consumer),
                &[DescriptorBinding {
                    source: ends.read,
                    target: Descriptor::STDIN,
                }],
            )?;
            allocated.push(consumer_id);
            let producer_id = kernel.spawn_child(
                supervisor,
                process_spec(producer_wasm, &producer),
                &[DescriptorBinding {
                    source: ends.write,
                    target: Descriptor::STDOUT,
                }],
            )?;
            allocated.push(producer_id);
            kernel.close_descriptor(supervisor, ends.read)?;
            kernel.close_descriptor(supervisor, ends.write)?;
            kernel.admit(consumer_id)?;
            kernel.admit(producer_id)?;
            kernel.exit(supervisor, 0)?;
            kernel.reap_root(supervisor)?;
            Ok((consumer_id, producer_id))
        })();
        let (consumer_id, producer_id) = match setup {
            Ok(ids) => ids,
            Err(error) => {
                abort_processes(&mut self.kernel.borrow_mut(), &allocated)?;
                return Err(error);
            }
        };

        let mut slots = BTreeMap::new();
        let mut consumer_slot = ProcessSlot {
            store: consumer_store,
            invocation: InvocationState::Start(consumer_start),
            limits: consumer_limits,
            report: None,
        };
        attach_process(&mut consumer_slot, consumer_id, &self.kernel);
        slots.insert(consumer_id, consumer_slot);
        let mut producer_slot = ProcessSlot {
            store: producer_store,
            invocation: InvocationState::Start(producer_start),
            limits: producer_limits,
            report: None,
        };
        attach_process(&mut producer_slot, producer_id, &self.kernel);
        slots.insert(producer_id, producer_slot);

        let drive_result = (|| -> Result<(), PipelineError> {
            while slots.values().any(|slot| slot.report.is_none()) {
                let process = self
                    .kernel
                    .borrow_mut()
                    .dispatch_next()
                    .ok_or(PipelineError::Deadlock)?;
                let slot = slots
                    .get_mut(&process)
                    .ok_or(PipelineError::MissingReport(process))?;
                drive_slot(process, slot, &self.kernel)?;
            }
            Ok(())
        })();
        if let Err(error) = drive_result {
            abort_processes(&mut self.kernel.borrow_mut(), &[consumer_id, producer_id])?;
            return Err(error);
        }

        let report = PipelineReport {
            producer: PipelineProcessReport {
                process_id: producer_id,
                execution: take_report(&mut slots, producer_id)?,
            },
            consumer: PipelineProcessReport {
                process_id: consumer_id,
                execution: take_report(&mut slots, consumer_id)?,
            },
        };
        {
            let mut kernel = self.kernel.borrow_mut();
            kernel.reap_root(consumer_id)?;
            kernel.reap_root(producer_id)?;
        }
        Ok(report)
    }

    fn preflight_pipeline(&self, pipe_capacity: usize) -> Result<(), PipelineError> {
        let kernel = self.kernel.borrow();
        if pipe_capacity == 0 {
            return Err(KernelError::InvalidPipeCapacity.into());
        }
        if pipe_capacity > self.limits.max_pipe_bytes {
            return Err(KernelError::QuotaExceeded(Quota::PipeBytes).into());
        }
        if kernel.process_count().saturating_add(3) > self.limits.max_processes {
            return Err(KernelError::QuotaExceeded(Quota::Processes).into());
        }
        if kernel.pipe_count().saturating_add(1) > self.limits.max_pipes {
            return Err(KernelError::QuotaExceeded(Quota::Pipes).into());
        }
        let total_pipe_bytes = kernel
            .reserved_pipe_bytes()
            .checked_add(pipe_capacity)
            .ok_or(KernelError::InvalidPipeCapacity)?;
        if total_pipe_bytes > self.limits.max_total_pipe_bytes {
            return Err(KernelError::QuotaExceeded(Quota::TotalPipeBytes).into());
        }
        if self.limits.max_descriptors_per_process < 2 {
            return Err(KernelError::QuotaExceeded(Quota::Descriptors).into());
        }
        Ok(())
    }
}

/// Failure to construct or drive the bounded pipeline machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineError {
    /// A process module failed ordinary runtime admission.
    Launch(LaunchError),
    /// The process semantic kernel rejected setup or a transition.
    Kernel(KernelError),
    /// Wasmi could not resume a previously parked host call.
    Resume(String),
    /// No process was runnable while at least one process remained incomplete.
    Deadlock,
    /// A process completed without producing its accounting report.
    MissingReport(ProcessId),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Launch(error) => error.fmt(formatter),
            Self::Kernel(error) => error.fmt(formatter),
            Self::Resume(message) => write!(formatter, "pipeline resume failed: {message}"),
            Self::Deadlock => formatter.write_str("realm pipeline deadlocked"),
            Self::MissingReport(process) => write!(
                formatter,
                "realm process {} completed without a report",
                process.get()
            ),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<LaunchError> for PipelineError {
    fn from(error: LaunchError) -> Self {
        Self::Launch(error)
    }
}

impl From<KernelError> for PipelineError {
    fn from(error: KernelError) -> Self {
        Self::Kernel(error)
    }
}

enum InvocationState {
    Start(TypedFunc<(), ()>),
    Suspended(TypedResumableCallHostTrap<()>),
    Complete,
}

struct ProcessSlot {
    store: Store<HostState>,
    invocation: InvocationState,
    limits: RunLimits,
    report: Option<ExecutionReport>,
}

enum PendingCompletion {
    Resume(i32),
    Reparked,
    BrokenPipe,
}

impl RealmRuntime {
    /// Execute two nested modules connected by one bounded stdout-to-stdin pipe.
    ///
    /// The consumer is admitted first so its initial empty read proves real
    /// suspension and resumption. Both process stores have independent memories.
    pub fn execute_pipeline(
        &self,
        producer_wasm: &[u8],
        producer: ProcessConfig,
        consumer_wasm: &[u8],
        consumer: ProcessConfig,
        limits: RunLimits,
        pipe_capacity: usize,
    ) -> Result<PipelineReport, PipelineError> {
        RealmMachine::default().execute_pipeline(
            producer_wasm,
            producer,
            consumer_wasm,
            consumer,
            limits,
            pipe_capacity,
        )
    }
}

fn attach_process(slot: &mut ProcessSlot, process: ProcessId, kernel: &Rc<RefCell<RealmKernel>>) {
    slot.store.data_mut().process = Some(ProcessContext {
        process,
        kernel: Rc::clone(kernel),
    });
}

fn abort_processes(kernel: &mut RealmKernel, processes: &[ProcessId]) -> Result<(), KernelError> {
    for process in processes {
        let Ok(snapshot) = kernel.process(*process) else {
            continue;
        };
        if !snapshot.state.is_terminal() {
            kernel.signal(*process, Signal::Kill)?;
        }
    }
    for process in processes {
        let Ok(snapshot) = kernel.process(*process) else {
            continue;
        };
        if snapshot.parent.is_none() && snapshot.state.is_terminal() {
            kernel.reap_root(*process)?;
        }
    }
    Ok(())
}

fn process_spec(wasm: &[u8], process: &ProcessConfig) -> ProcessSpec {
    ProcessSpec::new(
        ExecutableId::new(*blake3::hash(wasm).as_bytes()),
        process.cwd.clone(),
    )
}

fn drive_slot(
    process: ProcessId,
    slot: &mut ProcessSlot,
    kernel: &Rc<RefCell<RealmKernel>>,
) -> Result<(), PipelineError> {
    let invocation = std::mem::replace(&mut slot.invocation, InvocationState::Complete);
    let call = match invocation {
        InvocationState::Start(start) => match start.call_resumable(&mut slot.store, ()) {
            Ok(call) => call,
            Err(error) => {
                let outcome = classify_process_error(&error);
                finish_process(process, slot, kernel, outcome)?;
                return Ok(());
            }
        },
        InvocationState::Suspended(suspended) => {
            let value = match complete_pending_io(process, slot, kernel)? {
                PendingCompletion::Resume(value) => value,
                PendingCompletion::Reparked => {
                    slot.invocation = InvocationState::Suspended(suspended);
                    return Ok(());
                }
                PendingCompletion::BrokenPipe => {
                    finish_process(
                        process,
                        slot,
                        kernel,
                        ProcessOutcome::HostFault(HostFault::BrokenPipe),
                    )?;
                    return Ok(());
                }
            };
            match suspended.resume(&mut slot.store, &[Val::I32(value)]) {
                Ok(call) => call,
                Err(error) => {
                    let outcome = classify_process_error(&error);
                    finish_process(process, slot, kernel, outcome)?;
                    return Ok(());
                }
            }
        }
        InvocationState::Complete => return Err(PipelineError::MissingReport(process)),
    };
    handle_resumable_call(process, slot, kernel, call)
}

fn handle_resumable_call(
    process: ProcessId,
    slot: &mut ProcessSlot,
    kernel: &Rc<RefCell<RealmKernel>>,
    call: TypedResumableCall<()>,
) -> Result<(), PipelineError> {
    match call {
        TypedResumableCall::Finished(()) => {
            finish_process(process, slot, kernel, ProcessOutcome::Exited(0))
        }
        TypedResumableCall::OutOfFuel(_) => {
            finish_process(process, slot, kernel, ProcessOutcome::FuelExhausted)
        }
        TypedResumableCall::HostTrap(suspended) => {
            if suspended
                .host_error()
                .downcast_ref::<ProcessSuspended>()
                .is_some()
            {
                if slot.store.data().pending_io.is_none() {
                    return Err(PipelineError::Resume(
                        "suspended process has no pending I/O".to_string(),
                    ));
                }
                slot.invocation = InvocationState::Suspended(suspended);
                Ok(())
            } else {
                let outcome = classify_process_error(suspended.host_error());
                finish_process(process, slot, kernel, outcome)
            }
        }
    }
}

fn complete_pending_io(
    process: ProcessId,
    slot: &mut ProcessSlot,
    kernel: &Rc<RefCell<RealmKernel>>,
) -> Result<PendingCompletion, PipelineError> {
    let pending = slot
        .store
        .data_mut()
        .pending_io
        .take()
        .ok_or_else(|| PipelineError::Resume("pending I/O disappeared".to_string()))?;
    match pending {
        PendingIo::Read {
            descriptor,
            pointer,
            capacity,
        } => match {
            kernel
                .borrow_mut()
                .read_pipe(process, descriptor, capacity)?
        } {
            PipeReadResult::Data(bytes) => {
                let memory = slot.store.data().memory.ok_or_else(|| {
                    PipelineError::Resume("pending read process has no memory".to_string())
                })?;
                memory
                    .write(&mut slot.store, pointer, &bytes)
                    .map_err(|error| PipelineError::Resume(error.to_string()))?;
                let read = i32::try_from(bytes.len())
                    .map_err(|_| PipelineError::Resume("read count overflow".to_string()))?;
                Ok(PendingCompletion::Resume(read))
            }
            PipeReadResult::Eof => Ok(PendingCompletion::Resume(0)),
            PipeReadResult::WouldBlock => {
                if kernel.borrow_mut().park_pipe_read(process, descriptor)? != ParkResult::Parked {
                    return Err(PipelineError::Resume(
                        "spurious pipe-read wake could not repark".to_string(),
                    ));
                }
                slot.store.data_mut().pending_io = Some(PendingIo::Read {
                    descriptor,
                    pointer,
                    capacity,
                });
                Ok(PendingCompletion::Reparked)
            }
        },
        PendingIo::Write { descriptor, bytes } => {
            match {
                kernel
                    .borrow_mut()
                    .write_pipe(process, descriptor, &bytes)?
            } {
                PipeWriteResult::Written(written) => {
                    let written = i32::try_from(written)
                        .map_err(|_| PipelineError::Resume("write count overflow".to_string()))?;
                    Ok(PendingCompletion::Resume(written))
                }
                PipeWriteResult::BrokenPipe => Ok(PendingCompletion::BrokenPipe),
                PipeWriteResult::WouldBlock => {
                    if kernel.borrow_mut().park_pipe_write(process, descriptor)?
                        != ParkResult::Parked
                    {
                        return Err(PipelineError::Resume(
                            "spurious pipe-write wake could not repark".to_string(),
                        ));
                    }
                    slot.store.data_mut().pending_io = Some(PendingIo::Write { descriptor, bytes });
                    Ok(PendingCompletion::Reparked)
                }
            }
        }
    }
}

fn finish_process(
    process: ProcessId,
    slot: &mut ProcessSlot,
    kernel: &Rc<RefCell<RealmKernel>>,
    outcome: ProcessOutcome,
) -> Result<(), PipelineError> {
    match &outcome {
        ProcessOutcome::Exited(status) => kernel.borrow_mut().exit(process, *status)?,
        ProcessOutcome::HostFault(HostFault::BrokenPipe) => {
            kernel.borrow_mut().signal(process, Signal::Pipe)?;
        }
        ProcessOutcome::FuelExhausted
        | ProcessOutcome::HostFault(_)
        | ProcessOutcome::Trapped(_) => {
            kernel.borrow_mut().signal(process, Signal::Kill)?;
        }
    }
    slot.report = Some(execution_report(&slot.store, slot.limits, outcome));
    slot.invocation = InvocationState::Complete;
    Ok(())
}

fn take_report(
    slots: &mut BTreeMap<ProcessId, ProcessSlot>,
    process: ProcessId,
) -> Result<ExecutionReport, PipelineError> {
    slots
        .remove(&process)
        .and_then(|slot| slot.report)
        .ok_or(PipelineError::MissingReport(process))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_isolated_guests_stream_through_a_resumable_bounded_pipe() {
        let report = RealmRuntime::default()
            .execute_pipeline(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "hello pipeline".to_string()],
                    cwd: "/workspace".to_string(),
                },
                STDIN_CAT_GUEST,
                ProcessConfig {
                    argv: vec!["stdin-cat".to_string()],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                4,
            )
            .expect("pipeline completes");

        assert_ne!(report.producer.process_id, report.consumer.process_id);
        assert_eq!(report.producer.execution.outcome, ProcessOutcome::Exited(0));
        assert!(report.producer.execution.stdout.is_empty());
        assert_eq!(report.consumer.execution.outcome, ProcessOutcome::Exited(0));
        assert_eq!(report.consumer.execution.stdout, b"hello pipeline\n");
        assert!(report.producer.execution.fuel_consumed > 0);
        assert!(report.consumer.execution.fuel_consumed > 0);
        assert!(report.producer.execution.suspensions > 0);
        assert!(report.consumer.execution.suspensions > 0);
    }

    #[test]
    fn zero_capacity_fails_before_any_guest_runs() {
        let error = RealmRuntime::default()
            .execute_pipeline(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "x".to_string()],
                    cwd: "/workspace".to_string(),
                },
                STDIN_CAT_GUEST,
                ProcessConfig {
                    argv: vec!["stdin-cat".to_string()],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                0,
            )
            .expect_err("zero-capacity pipe is rejected");

        assert_eq!(
            error,
            PipelineError::Kernel(KernelError::InvalidPipeCapacity)
        );
    }

    #[test]
    fn producer_gets_broken_pipe_when_consumer_exits_without_reading() {
        let immediate_exit =
            wat::parse_str(r#"(module (func (export "_start")))"#).expect("exit guest compiles");
        let report = RealmRuntime::default()
            .execute_pipeline(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "unread".to_string()],
                    cwd: "/workspace".to_string(),
                },
                &immediate_exit,
                ProcessConfig {
                    argv: vec!["exit".to_string()],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                4,
            )
            .expect("pipeline reaches terminal states");

        assert_eq!(report.consumer.execution.outcome, ProcessOutcome::Exited(0));
        assert_eq!(
            report.producer.execution.outcome,
            ProcessOutcome::HostFault(HostFault::BrokenPipe)
        );
    }

    #[test]
    fn long_lived_machine_reaps_resources_without_reusing_process_ids() {
        let mut machine = RealmMachine::default();
        let first = machine
            .execute_process(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "first".to_string()],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<DenyRealmHost>::default(),
            )
            .expect("first process completes");
        let pipeline = machine
            .execute_pipeline(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "second".to_string()],
                    cwd: "/workspace".to_string(),
                },
                STDIN_CAT_GUEST,
                ProcessConfig {
                    argv: vec!["stdin-cat".to_string()],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                4,
            )
            .expect("pipeline completes");
        let status = machine.status();

        assert_eq!(first.process_id.get(), 1);
        assert_eq!(pipeline.consumer.process_id.get(), 3);
        assert_eq!(pipeline.producer.process_id.get(), 4);
        assert_eq!(status.next_process_id.map(ProcessId::get), Some(5));
        assert_eq!(status.process_records, 0);
        assert_eq!(status.pipe_objects, 0);
        assert_eq!(status.reserved_pipe_bytes, 0);
    }

    #[test]
    fn failed_pipeline_admission_leaves_machine_state_unchanged() {
        let mut machine = RealmMachine::default();
        let before = machine.status();
        let error = machine
            .execute_pipeline(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "x".to_string()],
                    cwd: "/workspace".to_string(),
                },
                STDIN_CAT_GUEST,
                ProcessConfig {
                    argv: vec!["stdin-cat".to_string()],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                0,
            )
            .expect_err("zero capacity is rejected");

        assert_eq!(
            error,
            PipelineError::Kernel(KernelError::InvalidPipeCapacity)
        );
        assert_eq!(machine.status(), before);
    }
}
