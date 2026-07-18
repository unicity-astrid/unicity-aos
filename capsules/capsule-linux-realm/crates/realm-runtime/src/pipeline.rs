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

/// One foreground process tree created by guest `spawn-signed` calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessTreeReport {
    /// The guest process submitted by the outer Realm request.
    pub root: PipelineProcessReport,
    /// Every signed descendant created during that foreground request, ordered
    /// by its monotonic process identity.
    pub children: Vec<PipelineProcessReport>,
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
    generation: u64,
}

impl Default for RealmMachine {
    fn default() -> Self {
        Self::with_generation(0)
    }
}

impl RealmMachine {
    /// Create a default-quota machine for one explicit actor boot generation.
    #[must_use]
    pub fn with_generation(generation: u64) -> Self {
        Self::new_for_generation(
            RealmLimits {
                max_processes: 64,
                max_pipes: 64,
                max_pipe_bytes: MAX_IO_BYTES,
                max_total_pipe_bytes: MAX_IO_BYTES * 16,
                max_descriptors_per_process: 64,
            },
            generation,
        )
    }

    /// Create a machine with explicit semantic-kernel quotas.
    #[must_use]
    pub fn new(limits: RealmLimits) -> Self {
        Self::new_for_generation(limits, 0)
    }

    /// Create a machine with explicit quotas and actor boot generation.
    #[must_use]
    pub fn new_for_generation(limits: RealmLimits, generation: u64) -> Self {
        Self {
            runtime: RealmRuntime::default(),
            kernel: Rc::new(RefCell::new(RealmKernel::new(limits))),
            limits,
            generation,
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
        attach_process(
            &mut slot,
            process_id,
            &self.kernel,
            self.generation,
            self.limits.max_descriptors_per_process,
            None,
        );
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

    /// Execute one foreground guest that may create a bounded tree of signed
    /// child modules through the private Realm ABI.
    ///
    /// Fuel and output budgets are partitioned before execution across the root
    /// and the maximum admitted children. Unused child partitions are not
    /// reclaimed, keeping the aggregate request ceiling independent of guest
    /// scheduling choices.
    pub fn execute_process_tree(
        &mut self,
        wasm: &[u8],
        process: ProcessConfig,
        limits: RunLimits,
        realm_host: Box<dyn RealmHost>,
        max_children: usize,
    ) -> Result<ProcessTreeReport, PipelineError> {
        self.preflight_process_tree(max_children)?;
        let partitions = max_children
            .checked_add(1)
            .ok_or(KernelError::QuotaExceeded(Quota::Processes))?;
        let partitions_u64 =
            u64::try_from(partitions).map_err(|_| KernelError::QuotaExceeded(Quota::Processes))?;
        let child_limits = RunLimits {
            fuel: limits.fuel / partitions_u64,
            memory_bytes: limits.memory_bytes,
            output_bytes: limits.output_bytes / partitions,
        };
        let root_limits = RunLimits {
            fuel: child_limits
                .fuel
                .saturating_add(limits.fuel % partitions_u64),
            memory_bytes: limits.memory_bytes,
            output_bytes: child_limits
                .output_bytes
                .saturating_add(limits.output_bytes % partitions),
        };

        let (root_store, root_start) =
            self.runtime
                .prepare_process(wasm, process.clone(), root_limits, realm_host, None)?;
        let root_id = {
            let mut kernel = self.kernel.borrow_mut();
            let root = kernel.spawn_root(process_spec(wasm, &process))?;
            if let Err(error) = kernel.admit(root) {
                abort_processes(&mut kernel, &[root])?;
                return Err(error.into());
            }
            root
        };
        let spawn = Rc::new(RefCell::new(SpawnState {
            runtime: self.runtime.clone(),
            child_limits,
            remaining_children: max_children,
            prepared: Vec::new(),
            reaped: Vec::new(),
        }));
        let mut root_slot = ProcessSlot {
            store: root_store,
            invocation: InvocationState::Start(root_start),
            limits: root_limits,
            report: None,
        };
        attach_process(
            &mut root_slot,
            root_id,
            &self.kernel,
            self.generation,
            self.limits.max_descriptors_per_process,
            Some(Rc::clone(&spawn)),
        );
        let mut slots = BTreeMap::from([(root_id, root_slot)]);

        let drive_result = (|| -> Result<(), PipelineError> {
            while slots.values().any(|slot| slot.report.is_none()) {
                complete_preterminated_slots(&mut slots, &self.kernel)?;
                if slots.values().all(|slot| slot.report.is_some()) {
                    break;
                }
                let process = self
                    .kernel
                    .borrow_mut()
                    .dispatch_next()
                    .ok_or(PipelineError::Deadlock)?;
                let slot = slots
                    .get_mut(&process)
                    .ok_or(PipelineError::MissingReport(process))?;
                drive_slot(process, slot, &self.kernel)?;
                drain_prepared_children(&spawn, &mut slots)?;
                complete_reaped_slots(&spawn, &mut slots)?;
            }
            Ok(())
        })();
        if let Err(error) = drive_result {
            let processes = slots.keys().copied().collect::<Vec<_>>();
            abort_processes(&mut self.kernel.borrow_mut(), &processes)?;
            return Err(error);
        }

        let process_ids = slots.keys().copied().collect::<Vec<_>>();
        reap_completed_roots(&mut self.kernel.borrow_mut(), &process_ids)?;
        let root = PipelineProcessReport {
            process_id: root_id,
            execution: take_report(&mut slots, root_id)?,
        };
        let children = process_ids
            .into_iter()
            .filter(|process| *process != root_id)
            .map(|process| {
                Ok(PipelineProcessReport {
                    process_id: process,
                    execution: take_report(&mut slots, process)?,
                })
            })
            .collect::<Result<Vec<_>, PipelineError>>()?;
        Ok(ProcessTreeReport { root, children })
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
        attach_process(
            &mut consumer_slot,
            consumer_id,
            &self.kernel,
            self.generation,
            self.limits.max_descriptors_per_process,
            None,
        );
        slots.insert(consumer_id, consumer_slot);
        let mut producer_slot = ProcessSlot {
            store: producer_store,
            invocation: InvocationState::Start(producer_start),
            limits: producer_limits,
            report: None,
        };
        attach_process(
            &mut producer_slot,
            producer_id,
            &self.kernel,
            self.generation,
            self.limits.max_descriptors_per_process,
            None,
        );
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

    fn preflight_process_tree(&self, max_children: usize) -> Result<(), PipelineError> {
        let requested = max_children
            .checked_add(1)
            .ok_or(KernelError::QuotaExceeded(Quota::Processes))?;
        let retained = self.kernel.borrow().process_count();
        if retained
            .checked_add(requested)
            .is_none_or(|count| count > self.limits.max_processes)
        {
            return Err(KernelError::QuotaExceeded(Quota::Processes).into());
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

fn attach_process(
    slot: &mut ProcessSlot,
    process: ProcessId,
    kernel: &Rc<RefCell<RealmKernel>>,
    generation: u64,
    descriptor_limit: usize,
    spawn: Option<Rc<RefCell<SpawnState>>>,
) {
    slot.store.data_mut().descriptor_limit = descriptor_limit;
    slot.store.data_mut().process = Some(ProcessContext {
        process,
        kernel: Rc::clone(kernel),
        generation,
        descriptor_limit,
        spawn,
    });
}

fn drain_prepared_children(
    spawn: &Rc<RefCell<SpawnState>>,
    slots: &mut BTreeMap<ProcessId, ProcessSlot>,
) -> Result<(), PipelineError> {
    let prepared = std::mem::take(&mut spawn.borrow_mut().prepared);
    for child in prepared {
        let process = child.process;
        let slot = ProcessSlot {
            store: child.store,
            invocation: InvocationState::Start(child.start),
            limits: child.limits,
            report: None,
        };
        if slots.insert(process, slot).is_some() {
            return Err(PipelineError::Resume(format!(
                "duplicate prepared process {}",
                process.get()
            )));
        }
    }
    Ok(())
}

fn complete_reaped_slots(
    spawn: &Rc<RefCell<SpawnState>>,
    slots: &mut BTreeMap<ProcessId, ProcessSlot>,
) -> Result<(), PipelineError> {
    let reaped = std::mem::take(&mut spawn.borrow_mut().reaped);
    for (process, termination) in reaped {
        let slot = slots
            .get_mut(&process)
            .ok_or(PipelineError::MissingReport(process))?;
        if slot.report.is_none() {
            let outcome = match termination {
                Termination::Exited(status) => ProcessOutcome::Exited(status),
                Termination::Signaled(signal) => ProcessOutcome::Signaled(signal),
            };
            slot.store.data_mut().pending_io = None;
            slot.report = Some(execution_report(&slot.store, slot.limits, outcome));
            slot.invocation = InvocationState::Complete;
        }
    }
    Ok(())
}

fn complete_preterminated_slots(
    slots: &mut BTreeMap<ProcessId, ProcessSlot>,
    kernel: &Rc<RefCell<RealmKernel>>,
) -> Result<(), PipelineError> {
    let terminal = {
        let kernel = kernel.borrow();
        slots
            .iter()
            .filter_map(|(process, slot)| {
                if slot.report.is_some() {
                    return None;
                }
                match kernel.process(*process).ok()?.state {
                    ProcessState::Exited(status) => {
                        Some((*process, ProcessOutcome::Exited(status)))
                    }
                    ProcessState::Signaled(signal) => {
                        Some((*process, ProcessOutcome::Signaled(signal)))
                    }
                    ProcessState::Created
                    | ProcessState::Runnable
                    | ProcessState::Running
                    | ProcessState::Waiting(_) => None,
                }
            })
            .collect::<Vec<_>>()
    };
    for (process, outcome) in terminal {
        let slot = slots
            .get_mut(&process)
            .ok_or(PipelineError::MissingReport(process))?;
        slot.store.data_mut().pending_io = None;
        slot.report = Some(execution_report(&slot.store, slot.limits, outcome));
        slot.invocation = InvocationState::Complete;
    }
    Ok(())
}

fn reap_completed_roots(
    kernel: &mut RealmKernel,
    processes: &[ProcessId],
) -> Result<(), KernelError> {
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

fn abort_processes(kernel: &mut RealmKernel, processes: &[ProcessId]) -> Result<(), KernelError> {
    for process in processes {
        let Ok(snapshot) = kernel.process(*process) else {
            continue;
        };
        if !snapshot.state.is_terminal() {
            kernel.signal(*process, Signal::Kill)?;
        }
    }
    reap_completed_roots(kernel, processes)
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
                        "suspended process has no pending host call".to_string(),
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
        .ok_or_else(|| PipelineError::Resume("pending host call disappeared".to_string()))?;
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
        PendingIo::Spawn {
            child,
            handle_pointer,
        } => {
            let generation = slot
                .store
                .data()
                .process
                .as_ref()
                .ok_or_else(|| PipelineError::Resume("spawned process has no context".to_string()))?
                .generation;
            let bytes = encode_process_handle(ProcessHandle::new(generation, child));
            write_store_bytes(slot, handle_pointer, &bytes, "spawn handle")?;
            Ok(PendingCompletion::Resume(0))
        }
        PendingIo::Wait {
            child,
            status_pointer,
        } => match kernel.borrow_mut().wait_child(process, child)? {
            WaitResult::Reaped(termination) => {
                if let Some(context) = slot.store.data().process.as_ref() {
                    record_reaped_child(context, child, termination);
                }
                let bytes = encode_termination(termination);
                write_store_bytes(slot, status_pointer, &bytes, "wait status")?;
                Ok(PendingCompletion::Resume(0))
            }
            WaitResult::Pending => {
                slot.store.data_mut().pending_io = Some(PendingIo::Wait {
                    child,
                    status_pointer,
                });
                Ok(PendingCompletion::Reparked)
            }
        },
    }
}

fn write_store_bytes(
    slot: &mut ProcessSlot,
    pointer: usize,
    bytes: &[u8],
    operation: &str,
) -> Result<(), PipelineError> {
    let memory = slot.store.data().memory.ok_or_else(|| {
        PipelineError::Resume(format!("pending {operation} process has no memory"))
    })?;
    memory
        .write(&mut slot.store, pointer, bytes)
        .map_err(|error| PipelineError::Resume(error.to_string()))
}

fn finish_process(
    process: ProcessId,
    slot: &mut ProcessSlot,
    kernel: &Rc<RefCell<RealmKernel>>,
    outcome: ProcessOutcome,
) -> Result<(), PipelineError> {
    match &outcome {
        ProcessOutcome::Exited(status) => kernel.borrow_mut().exit(process, *status)?,
        ProcessOutcome::Signaled(signal) => kernel.borrow_mut().signal(process, *signal)?,
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

    #[derive(Default)]
    struct OpenHost;

    impl RealmHost for OpenHost {
        fn open(
            &mut self,
            _cwd: &str,
            _path: &str,
            _mode: OpenMode,
        ) -> Result<Box<dyn RealmFile>, RealmIoError> {
            Ok(Box::new(EmptyFile))
        }
    }

    struct EmptyFile;

    impl RealmFile for EmptyFile {
        fn read(&mut self, _max_bytes: usize) -> Result<Vec<u8>, RealmIoError> {
            Ok(Vec::new())
        }

        fn write(&mut self, bytes: &[u8]) -> Result<usize, RealmIoError> {
            Ok(bytes.len())
        }
    }

    fn assert_spawn_record_rejected_without_child(wat_source: &str, fault: HostFault) {
        let guest = wat::parse_str(wat_source).expect("spawn-record test guest compiles");
        let mut machine = RealmMachine::with_generation(21);
        let report = machine
            .execute_process_tree(
                &guest,
                ProcessConfig {
                    argv: vec!["malformed-spawn".to_string()],
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<DenyRealmHost>::default(),
                1,
            )
            .expect("malformed request returns a bounded process report");

        assert_eq!(
            report.root.execution.outcome,
            ProcessOutcome::HostFault(fault)
        );
        assert!(report.children.is_empty());
        assert_eq!(machine.status().process_records, 0);
        assert_eq!(machine.status().pipe_objects, 0);
        assert_eq!(machine.status().reserved_pipe_bytes, 0);
        assert_eq!(machine.status().next_process_id, Some(ProcessId::new(2)));
    }

    #[test]
    fn guest_file_and_pipe_descriptors_share_one_number_space() {
        let guest = wat::parse_str(
            r#"(module
                (import "aos_realm_v0" "open"
                    (func $open (param i32 i32 i32) (result i32)))
                (import "aos_realm_v0" "pipe"
                    (func $pipe (param i32 i32) (result i32)))
                (import "aos_realm_v0" "close"
                    (func $close (param i32) (result i32)))
                (memory (export "memory") 1 1)
                (data (i32.const 64) "file")
                (func (export "_start")
                    (local $file i32)
                    i32.const 64
                    i32.const 4
                    i32.const 1
                    call $open
                    local.tee $file
                    i32.const 3
                    i32.ne
                    if unreachable end
                    i32.const 4
                    i32.const 0
                    call $pipe
                    drop
                    i32.const 0
                    i32.load
                    i32.const 4
                    i32.ne
                    if unreachable end
                    i32.const 4
                    i32.load
                    i32.const 5
                    i32.ne
                    if unreachable end
                    local.get $file
                    call $close
                    drop
                    i32.const 0
                    i32.load
                    call $close
                    drop
                    i32.const 4
                    i32.load
                    call $close
                    drop))"#,
        )
        .expect("descriptor guest compiles");
        let mut machine = RealmMachine::default();
        let report = machine
            .execute_process(
                &guest,
                ProcessConfig {
                    argv: vec!["descriptor-space".to_string()],
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<OpenHost>::default(),
            )
            .expect("file then pipe completes");

        assert_eq!(report.execution.outcome, ProcessOutcome::Exited(0));
        assert_eq!(machine.status().process_records, 0);
        assert_eq!(machine.status().pipe_objects, 0);
    }

    #[test]
    fn guest_creates_waits_and_reaps_its_own_signed_pipeline() {
        let limits = RunLimits::default();
        let mut machine = RealmMachine::with_generation(9);
        let report = machine
            .execute_process_tree(
                GUEST_PIPELINE_GUEST,
                ProcessConfig {
                    argv: vec![
                        "guest-pipeline".to_string(),
                        "hello from guest-created children".to_string(),
                    ],
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                limits,
                Box::<DenyRealmHost>::default(),
                2,
            )
            .expect("guest-created process tree completes");

        assert_eq!(report.root.process_id, ProcessId::new(1));
        assert_eq!(report.root.execution.outcome, ProcessOutcome::Exited(0));
        assert!(report.root.execution.suspensions >= 2);
        assert_eq!(report.children.len(), 2);
        assert_eq!(report.children[0].process_id, ProcessId::new(2));
        assert_eq!(report.children[1].process_id, ProcessId::new(3));
        assert_eq!(
            report.children[0].execution.stdout,
            b"hello from guest-created children\n"
        );
        assert!(report.children[1].execution.stdout.is_empty());
        assert!(
            report
                .children
                .iter()
                .all(|child| child.execution.outcome == ProcessOutcome::Exited(0))
        );
        let fuel = report
            .children
            .iter()
            .fold(report.root.execution.fuel_consumed, |total, child| {
                total.saturating_add(child.execution.fuel_consumed)
            });
        assert!(fuel <= limits.fuel);
        assert_eq!(machine.status().process_records, 0);
        assert_eq!(machine.status().pipe_objects, 0);
        assert_eq!(machine.status().reserved_pipe_bytes, 0);
        assert_eq!(machine.status().next_process_id, Some(ProcessId::new(4)));
    }

    #[test]
    fn mini_shell_builds_a_record_spawn_pipeline_and_releases_every_endpoint() {
        let limits = RunLimits::default();
        let mut machine = RealmMachine::with_generation(10);
        let report = machine
            .execute_process_tree(
                MINI_SHELL_GUEST,
                ProcessConfig {
                    argv: vec![
                        "realm-sh".to_string(),
                        "echo".to_string(),
                        "shell-owned topology".to_string(),
                        "|".to_string(),
                        "cat".to_string(),
                    ],
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                limits,
                Box::<DenyRealmHost>::default(),
                2,
            )
            .expect("mini shell process tree completes");

        assert_eq!(report.root.process_id, ProcessId::new(1));
        assert_eq!(report.children.len(), 2);
        assert_eq!(report.children[0].process_id, ProcessId::new(2));
        assert_eq!(
            report.children[0].execution.stdout,
            b"shell-owned topology\n"
        );
        assert_eq!(report.children[1].process_id, ProcessId::new(3));
        assert!(report.children[1].execution.stdout.is_empty());
        assert_eq!(report.root.execution.outcome, ProcessOutcome::Exited(0));
        assert!(
            report
                .children
                .iter()
                .all(|child| child.execution.outcome == ProcessOutcome::Exited(0))
        );
        assert_eq!(machine.status().process_records, 0);
        assert_eq!(machine.status().pipe_objects, 0);
        assert_eq!(machine.status().reserved_pipe_bytes, 0);
        assert_eq!(machine.status().next_process_id, Some(ProcessId::new(4)));
    }

    #[test]
    fn malformed_signed_spawn_records_fail_before_allocating_a_child_pid() {
        assert_spawn_record_rejected_without_child(
            r#"(module
                (import "aos_realm_v0" "spawn-signed-record"
                    (func $spawn (param i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (func (export "_start")
                    i32.const 0 i32.const 99 i32.store
                    i32.const 0 i32.const 44 call $spawn drop))"#,
            HostFault::InvalidArgument,
        );

        assert_spawn_record_rejected_without_child(
            r#"(module
                (import "aos_realm_v0" "spawn-signed-record"
                    (func $spawn (param i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (data (i32.const 512) "/usr/bin/env")
                (data (i32.const 524) "env")
                (data (i32.const 527) "BAD-KEY=x")
                (func (export "_start")
                    i32.const 0 i32.const 1 i32.store
                    i32.const 8 i32.const 512 i32.store
                    i32.const 12 i32.const 12 i32.store
                    i32.const 16 i32.const 64 i32.store
                    i32.const 20 i32.const 1 i32.store
                    i32.const 24 i32.const 72 i32.store
                    i32.const 28 i32.const 1 i32.store
                    i32.const 40 i32.const 128 i32.store
                    i32.const 64 i32.const 524 i32.store
                    i32.const 68 i32.const 3 i32.store
                    i32.const 72 i32.const 527 i32.store
                    i32.const 76 i32.const 9 i32.store
                    i32.const 0 i32.const 44 call $spawn drop))"#,
            HostFault::InvalidArgument,
        );

        assert_spawn_record_rejected_without_child(
            r#"(module
                (import "aos_realm_v0" "pipe"
                    (func $pipe (param i32 i32) (result i32)))
                (import "aos_realm_v0" "spawn-signed-record"
                    (func $spawn (param i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (data (i32.const 512) "/bin/echo")
                (data (i32.const 521) "echo")
                (data (i32.const 525) "x")
                (func (export "_start")
                    i32.const 4 i32.const 48 call $pipe drop
                    i32.const 0 i32.const 1 i32.store
                    i32.const 8 i32.const 512 i32.store
                    i32.const 12 i32.const 9 i32.store
                    i32.const 16 i32.const 64 i32.store
                    i32.const 20 i32.const 2 i32.store
                    i32.const 32 i32.const 80 i32.store
                    i32.const 36 i32.const 2 i32.store
                    i32.const 40 i32.const 128 i32.store
                    i32.const 64 i32.const 521 i32.store
                    i32.const 68 i32.const 4 i32.store
                    i32.const 72 i32.const 525 i32.store
                    i32.const 76 i32.const 1 i32.store
                    i32.const 80 i32.const 1 i32.store
                    i32.const 84 i32.const 48 i32.load i32.store
                    i32.const 88 i32.const 1 i32.store
                    i32.const 92 i32.const 1 i32.store
                    i32.const 96 i32.const 52 i32.load i32.store
                    i32.const 100 i32.const 1 i32.store
                    i32.const 0 i32.const 44 call $spawn drop))"#,
            HostFault::InvalidArgument,
        );

        assert_spawn_record_rejected_without_child(
            r#"(module
                (import "aos_realm_v0" "spawn-signed-record"
                    (func $spawn (param i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (func (export "_start")
                    i32.const 65520 i32.const 44 call $spawn drop))"#,
            HostFault::InvalidPointer,
        );

        assert_spawn_record_rejected_without_child(
            r#"(module
                (import "aos_realm_v0" "spawn-signed-record"
                    (func $spawn (param i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (data (i32.const 512) "/bin/echo")
                (func (export "_start")
                    i32.const 0 i32.const 1 i32.store
                    i32.const 8 i32.const 512 i32.store
                    i32.const 12 i32.const 9 i32.store
                    i32.const 20 i32.const 65 i32.store
                    i32.const 40 i32.const 128 i32.store
                    i32.const 0 i32.const 44 call $spawn drop))"#,
            HostFault::InvalidArgument,
        );

        assert_spawn_record_rejected_without_child(
            r#"(module
                (import "aos_realm_v0" "spawn-signed-record"
                    (func $spawn (param i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (data (i32.const 512) "/bin/echo")
                (data (i32.const 521) "echo")
                (data (i32.const 525) "x")
                (func (export "_start")
                    i32.const 0 i32.const 1 i32.store
                    i32.const 8 i32.const 512 i32.store
                    i32.const 12 i32.const 9 i32.store
                    i32.const 16 i32.const 64 i32.store
                    i32.const 20 i32.const 2 i32.store
                    i32.const 32 i32.const 80 i32.store
                    i32.const 36 i32.const 1 i32.store
                    i32.const 40 i32.const 128 i32.store
                    i32.const 64 i32.const 521 i32.store
                    i32.const 68 i32.const 4 i32.store
                    i32.const 72 i32.const 525 i32.store
                    i32.const 76 i32.const 1 i32.store
                    i32.const 80 i32.const 2 i32.store
                    i32.const 84 i32.const 9 i32.store
                    i32.const 88 i32.const -1 i32.store
                    i32.const 0 i32.const 44 call $spawn drop))"#,
            HostFault::InvalidArgument,
        );
    }

    #[test]
    fn guest_child_budget_fails_closed_and_cleans_up_the_partial_tree() {
        let mut machine = RealmMachine::with_generation(4);
        let report = machine
            .execute_process_tree(
                GUEST_PIPELINE_GUEST,
                ProcessConfig {
                    argv: vec!["guest-pipeline".to_string(), "bounded".to_string()],
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<DenyRealmHost>::default(),
                1,
            )
            .expect("runtime returns the bounded failure report");

        assert_eq!(
            report.root.execution.outcome,
            ProcessOutcome::HostFault(HostFault::InvalidArgument)
        );
        assert_eq!(report.children.len(), 1);
        assert_eq!(
            report.children[0].execution.outcome,
            ProcessOutcome::Exited(0)
        );
        assert_eq!(machine.status().process_records, 0);
        assert_eq!(machine.status().pipe_objects, 0);
    }

    #[test]
    fn stale_generation_handle_cannot_wait_for_a_child() {
        let guest = wat::parse_str(
            r#"(module
                (import "aos_realm_v0" "spawn-signed"
                    (func $spawn (param i32 i32 i32 i32 i32 i32) (result i32)))
                (import "aos_realm_v0" "wait"
                    (func $wait (param i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (func (export "_start")
                    i32.const 1
                    i32.const 0
                    i32.const 0
                    i32.const -1
                    i32.const -1
                    i32.const 16
                    call $spawn
                    drop
                    i32.const 16
                    i64.const 10
                    i64.store
                    i32.const 16
                    i32.const 32
                    call $wait
                    drop))"#,
        )
        .expect("forged-handle guest compiles");
        let mut machine = RealmMachine::with_generation(9);
        let report = machine
            .execute_process_tree(
                &guest,
                ProcessConfig {
                    argv: vec!["forged-wait".to_string()],
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<DenyRealmHost>::default(),
                1,
            )
            .expect("runtime returns the forged-handle fault");

        assert_eq!(
            report.root.execution.outcome,
            ProcessOutcome::HostFault(HostFault::InvalidArgument)
        );
        assert_eq!(report.children.len(), 1);
        assert_eq!(machine.status().process_records, 0);
    }

    #[test]
    fn guest_can_signal_and_reap_its_own_blocked_child() {
        let guest = wat::parse_str(
            r#"(module
                (import "aos_realm_v0" "pipe"
                    (func $pipe (param i32 i32) (result i32)))
                (import "aos_realm_v0" "spawn-signed"
                    (func $spawn (param i32 i32 i32 i32 i32 i32) (result i32)))
                (import "aos_realm_v0" "signal"
                    (func $signal (param i32 i32) (result i32)))
                (import "aos_realm_v0" "close"
                    (func $close (param i32) (result i32)))
                (import "aos_realm_v0" "wait"
                    (func $wait (param i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (func (export "_start")
                    i32.const 4
                    i32.const 0
                    call $pipe
                    drop
                    i32.const 2
                    i32.const 0
                    i32.const 0
                    i32.const 0
                    i32.load
                    i32.const 0
                    i32.const 16
                    call $spawn
                    drop
                    i32.const 16
                    i32.const 3
                    call $signal
                    drop
                    i32.const 0
                    i32.load
                    call $close
                    drop
                    i32.const 4
                    i32.load
                    call $close
                    drop
                    i32.const 16
                    i32.const 32
                    call $wait
                    drop
                    i32.const 32
                    i32.load
                    i32.const 1
                    i32.ne
                    if unreachable end
                    i32.const 36
                    i32.load
                    i32.const 3
                    i32.ne
                    if unreachable end))"#,
        )
        .expect("signal guest compiles");
        let mut machine = RealmMachine::with_generation(12);
        let report = machine
            .execute_process_tree(
                &guest,
                ProcessConfig {
                    argv: vec!["signal-child".to_string()],
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<DenyRealmHost>::default(),
                1,
            )
            .expect("guest signals and waits for child");

        assert_eq!(report.root.execution.outcome, ProcessOutcome::Exited(0));
        assert_eq!(report.children.len(), 1);
        assert_eq!(
            report.children[0].execution.outcome,
            ProcessOutcome::Signaled(Signal::Kill)
        );
        assert_eq!(machine.status().process_records, 0);
        assert_eq!(machine.status().pipe_objects, 0);
    }

    #[test]
    fn two_isolated_guests_stream_through_a_resumable_bounded_pipe() {
        let report = RealmRuntime::default()
            .execute_pipeline(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "hello pipeline".to_string()],
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                STDIN_CAT_GUEST,
                ProcessConfig {
                    argv: vec!["stdin-cat".to_string()],
                    environment: Vec::new(),
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
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                STDIN_CAT_GUEST,
                ProcessConfig {
                    argv: vec!["stdin-cat".to_string()],
                    environment: Vec::new(),
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
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                &immediate_exit,
                ProcessConfig {
                    argv: vec!["exit".to_string()],
                    environment: Vec::new(),
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
                    environment: Vec::new(),
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
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                STDIN_CAT_GUEST,
                ProcessConfig {
                    argv: vec!["stdin-cat".to_string()],
                    environment: Vec::new(),
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
                    environment: Vec::new(),
                    cwd: "/workspace".to_string(),
                },
                STDIN_CAT_GUEST,
                ProcessConfig {
                    argv: vec!["stdin-cat".to_string()],
                    environment: Vec::new(),
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
