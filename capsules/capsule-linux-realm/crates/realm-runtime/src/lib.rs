#![deny(unsafe_code)]

//! Bounded execution of nested core WebAssembly processes.

use aos_realm_abi::{
    Descriptor, FIRST_FILE_FD, IMPORT_MODULE_V0, MAX_ARGUMENT_BYTES, MAX_PATH_BYTES, OPEN_READ,
    OPEN_WRITE_TRUNCATE, ProcessId, STDERR_FD, STDOUT_FD,
};
use aos_realm_core::{
    DescriptorBinding, DescriptorResource, ExecutableId, KernelError, ParkResult, PipeReadResult,
    PipeWriteResult, ProcessSpec, Quota, RealmKernel, RealmLimits, Signal,
};
use std::{cell::RefCell, collections::BTreeMap, fmt, rc::Rc, vec::Vec};
use wasmi::{
    Caller, Config, Engine, Error as WasmiError, Extern, Linker, Memory, Module, Store,
    StoreLimits, StoreLimitsBuilder, TrapCode, TypedFunc, Val,
};

mod pipeline;

pub use pipeline::{
    PipelineError, PipelineProcessReport, PipelineReport, RealmMachine, RealmMachineStatus,
};

/// Compiled smoke guest embedded into the capsule at build time.
pub const SMOKE_WRITE_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/smoke_write.wasm"));

/// Guest implementing `pwd` through the private realm ABI.
pub const PWD_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pwd.wasm"));

/// Guest implementing one-argument `echo` through the private realm ABI.
pub const ECHO_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/echo.wasm"));

/// Guest implementing truncate-or-create `write-file` through the private realm ABI.
pub const WRITE_FILE_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/write_file.wasm"));

/// Guest implementing streaming `cat` through the private realm ABI.
pub const CAT_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cat.wasm"));

/// Guest copying standard input to standard output with partial-write handling.
pub const STDIN_CAT_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stdin_cat.wasm"));

/// Hard limits for one nested process invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunLimits {
    /// Maximum interpreter fuel available to the process.
    pub fuel: u64,
    /// Maximum bytes in a single guest linear memory.
    pub memory_bytes: usize,
    /// Maximum combined bytes written to stdout and stderr.
    pub output_bytes: usize,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            fuel: 100_000,
            memory_bytes: 64 * 1024,
            output_bytes: 64 * 1024,
        }
    }
}

/// Process inputs supplied by the realm supervisor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessConfig {
    /// Argument vector including the program name at index zero.
    pub argv: Vec<String>,
    /// Guest-visible absolute current working directory.
    pub cwd: String,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            argv: vec!["smoke-write".to_string()],
            cwd: "/workspace".to_string(),
        }
    }
}

/// File-open operation admitted by the seed guest ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Open an existing file for reading.
    Read,
    /// Create or truncate a file for writing.
    WriteTruncate,
}

/// Stable I/O failure classes crossing the private guest boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealmIoError {
    /// The requested path does not exist.
    NotFound,
    /// The request is outside the realm's effective authority.
    Denied,
    /// The guest path is malformed or outside a mounted namespace.
    InvalidPath,
    /// A directory was requested as a file.
    IsDirectory,
    /// A file was requested as a directory.
    NotDirectory,
    /// A configured data or quota bound was exceeded.
    TooLarge,
    /// The operation is not implemented by this host profile.
    Unsupported,
    /// The backing host reported another I/O failure.
    Io,
}

impl fmt::Display for RealmIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "not found",
            Self::Denied => "denied",
            Self::InvalidPath => "invalid path",
            Self::IsDirectory => "is a directory",
            Self::NotDirectory => "not a directory",
            Self::TooLarge => "resource limit exceeded",
            Self::Unsupported => "unsupported",
            Self::Io => "I/O failure",
        })
    }
}

/// One opened file owned by a nested process descriptor.
pub trait RealmFile {
    /// Read up to `max_bytes` from the current cursor and advance it.
    fn read(&mut self, max_bytes: usize) -> Result<Vec<u8>, RealmIoError>;

    /// Write bytes at the current cursor and advance it.
    fn write(&mut self, bytes: &[u8]) -> Result<usize, RealmIoError>;

    /// Flush and close the guest-visible file.
    fn close(&mut self) -> Result<(), RealmIoError> {
        Ok(())
    }
}

/// Outer realm service used to resolve guest file opens.
pub trait RealmHost {
    /// Resolve `path` beneath `cwd` and return a bounded file object.
    fn open(
        &mut self,
        cwd: &str,
        path: &str,
        mode: OpenMode,
    ) -> Result<Box<dyn RealmFile>, RealmIoError>;
}

#[derive(Default)]
struct DenyRealmHost;

impl RealmHost for DenyRealmHost {
    fn open(
        &mut self,
        _cwd: &str,
        _path: &str,
        _mode: OpenMode,
    ) -> Result<Box<dyn RealmFile>, RealmIoError> {
        Err(RealmIoError::Denied)
    }
}

/// Terminal state of a nested process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// Process called the realm `exit` import or returned from `_start`.
    Exited(i32),
    /// Process exhausted its deterministic instruction budget.
    FuelExhausted,
    /// Process violated a realm host-call boundary.
    HostFault(HostFault),
    /// Process trapped for another reason.
    Trapped(String),
}

/// Result and accounting for a process that was successfully launched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionReport {
    /// Terminal process state.
    pub outcome: ProcessOutcome,
    /// Bytes written to guest stdout.
    pub stdout: Vec<u8>,
    /// Bytes written to guest stderr.
    pub stderr: Vec<u8>,
    /// Interpreter fuel consumed by the process and its host calls.
    pub fuel_consumed: u64,
    /// Linear-memory ceiling applied to this process.
    pub memory_limit_bytes: usize,
    /// Host calls that parked and later resumed this process.
    pub suspensions: u64,
}

/// Failure before a process could start executing `_start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchError {
    /// Guest bytes are not a valid supported core WebAssembly module.
    InvalidModule(String),
    /// Guest imports, start behavior, or resource declarations cannot be admitted.
    Instantiation(String),
    /// Guest does not export `_start` with the required `() -> ()` signature.
    MissingStart(String),
    /// The runtime could not configure a required realm host import.
    RuntimeConfiguration(String),
    /// Process arguments or current directory violate the private ABI contract.
    InvalidProcess(String),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModule(message) => write!(f, "invalid guest module: {message}"),
            Self::Instantiation(message) => write!(f, "guest instantiation denied: {message}"),
            Self::MissingStart(message) => write!(f, "guest _start is invalid: {message}"),
            Self::RuntimeConfiguration(message) => {
                write!(f, "realm runtime configuration failed: {message}")
            }
            Self::InvalidProcess(message) => write!(f, "invalid process: {message}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Host-call violations exposed as stable realm faults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFault {
    /// The process did not export the memory used by its pointers.
    MissingMemory,
    /// A guest pointer or length was negative, overflowing, or out of bounds.
    InvalidPointer,
    /// The process attempted to write a descriptor it does not own.
    UnknownDescriptor(i32),
    /// The process exceeded its combined stdout/stderr budget.
    OutputLimit,
    /// An argument index was absent from the process vector.
    MissingArgument,
    /// A guest-provided buffer cannot hold a process argument or CWD.
    BufferTooSmall,
    /// Guest bytes that must be UTF-8 were not valid UTF-8.
    InvalidUtf8,
    /// An unsupported flag or another ABI scalar was supplied.
    InvalidArgument,
    /// The realm filesystem rejected an operation.
    Io(RealmIoError),
    /// A pipe write has no remaining reader.
    BrokenPipe,
}

impl fmt::Display for HostFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMemory => f.write_str("guest has no exported memory"),
            Self::InvalidPointer => f.write_str("guest memory range is invalid"),
            Self::UnknownDescriptor(fd) => write!(f, "unknown guest descriptor {fd}"),
            Self::OutputLimit => f.write_str("guest output limit exceeded"),
            Self::MissingArgument => f.write_str("guest argument is missing"),
            Self::BufferTooSmall => f.write_str("guest buffer is too small"),
            Self::InvalidUtf8 => f.write_str("guest string is not valid UTF-8"),
            Self::InvalidArgument => f.write_str("guest argument is invalid"),
            Self::Io(error) => write!(f, "realm I/O failed: {error}"),
            Self::BrokenPipe => f.write_str("realm pipe has no reader"),
        }
    }
}

impl std::error::Error for HostFault {}
impl wasmi::errors::HostError for HostFault {}

struct HostState {
    limits: StoreLimits,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    output_limit: usize,
    monotonic_ns: i64,
    argv: Vec<String>,
    cwd: String,
    realm_host: Box<dyn RealmHost>,
    files: BTreeMap<i32, Box<dyn RealmFile>>,
    next_fd: i32,
    process: Option<ProcessContext>,
    memory: Option<Memory>,
    pending_io: Option<PendingIo>,
    suspensions: u64,
}

impl HostState {
    fn output_len(&self) -> usize {
        self.stdout.len().saturating_add(self.stderr.len())
    }
}

#[derive(Clone)]
struct ProcessContext {
    process: ProcessId,
    kernel: Rc<RefCell<RealmKernel>>,
}

enum PendingIo {
    Read {
        descriptor: Descriptor,
        pointer: usize,
        capacity: usize,
    },
    Write {
        descriptor: Descriptor,
        bytes: Vec<u8>,
    },
}

#[derive(Debug)]
struct ProcessSuspended;

impl fmt::Display for ProcessSuspended {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("realm process suspended")
    }
}

impl std::error::Error for ProcessSuspended {}
impl wasmi::errors::HostError for ProcessSuspended {}

/// Reference interpreter for the first AOS Realm guest ABI.
pub struct RealmRuntime {
    engine: Engine,
}

impl Default for RealmRuntime {
    fn default() -> Self {
        let mut config = Config::default();
        config.consume_fuel(true);
        Self {
            engine: Engine::new(&config),
        }
    }
}

impl RealmRuntime {
    /// Validates, instantiates, and runs one guest process.
    pub fn execute(&self, wasm: &[u8], limits: RunLimits) -> Result<ExecutionReport, LaunchError> {
        self.execute_process(
            wasm,
            ProcessConfig::default(),
            limits,
            Box::<DenyRealmHost>::default(),
        )
    }

    /// Runs one guest with explicit process inputs and an outer realm service.
    pub fn execute_process(
        &self,
        wasm: &[u8],
        process: ProcessConfig,
        limits: RunLimits,
        realm_host: Box<dyn RealmHost>,
    ) -> Result<ExecutionReport, LaunchError> {
        let (mut store, start) = self.prepare_process(wasm, process, limits, realm_host, None)?;
        let outcome = match start.call(&mut store, ()) {
            Ok(()) => ProcessOutcome::Exited(0),
            Err(error) => classify_process_error(&error),
        };
        Ok(execution_report(&store, limits, outcome))
    }

    fn prepare_process(
        &self,
        wasm: &[u8],
        process: ProcessConfig,
        limits: RunLimits,
        realm_host: Box<dyn RealmHost>,
        process_context: Option<ProcessContext>,
    ) -> Result<(Store<HostState>, TypedFunc<(), ()>), LaunchError> {
        validate_process(&process)?;
        let module = Module::new(&self.engine, wasm)
            .map_err(|error| LaunchError::InvalidModule(error.to_string()))?;
        let store_limits = StoreLimitsBuilder::new()
            .instances(1)
            .memories(1)
            .tables(1)
            .memory_size(limits.memory_bytes)
            .trap_on_grow_failure(true)
            .build();
        let state = HostState {
            limits: store_limits,
            stdout: Vec::new(),
            stderr: Vec::new(),
            output_limit: limits.output_bytes,
            monotonic_ns: 0,
            argv: process.argv,
            cwd: process.cwd,
            realm_host,
            files: BTreeMap::new(),
            next_fd: FIRST_FILE_FD,
            process: process_context,
            memory: None,
            pending_io: None,
            suspensions: 0,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(limits.fuel)
            .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;

        let mut linker = Linker::new(&self.engine);
        install_realm_v0(&mut linker)?;
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|error| LaunchError::Instantiation(error.to_string()))?;
        let start = instance
            .get_typed_func::<(), ()>(&store, "_start")
            .map_err(|error| LaunchError::MissingStart(error.to_string()))?;
        store.data_mut().memory = instance.get_memory(&store, "memory");
        Ok((store, start))
    }
}

fn execution_report(
    store: &Store<HostState>,
    limits: RunLimits,
    outcome: ProcessOutcome,
) -> ExecutionReport {
    let remaining_fuel = store.get_fuel().unwrap_or_default();
    let state = store.data();
    ExecutionReport {
        outcome,
        stdout: state.stdout.clone(),
        stderr: state.stderr.clone(),
        fuel_consumed: limits.fuel.saturating_sub(remaining_fuel),
        memory_limit_bytes: limits.memory_bytes,
        suspensions: state.suspensions,
    }
}

fn validate_process(process: &ProcessConfig) -> Result<(), LaunchError> {
    if process.argv.is_empty() {
        return Err(LaunchError::InvalidProcess(
            "argv must contain a program name".to_string(),
        ));
    }
    let argument_bytes = process
        .argv
        .iter()
        .try_fold(0usize, |total, value| total.checked_add(value.len()))
        .ok_or_else(|| LaunchError::InvalidProcess("argument size overflow".to_string()))?;
    if argument_bytes > MAX_ARGUMENT_BYTES {
        return Err(LaunchError::InvalidProcess(format!(
            "arguments use {argument_bytes} bytes; limit is {MAX_ARGUMENT_BYTES}"
        )));
    }
    if process.cwd.len() > MAX_PATH_BYTES || !process.cwd.starts_with('/') {
        return Err(LaunchError::InvalidProcess(
            "cwd must be an absolute guest path no larger than 4096 bytes".to_string(),
        ));
    }
    Ok(())
}

fn install_realm_v0(linker: &mut Linker<HostState>) -> Result<(), LaunchError> {
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "arg-len",
            |caller: Caller<'_, HostState>, index: i32| realm_arg_len(&caller, index),
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "arg-read",
            |mut caller: Caller<'_, HostState>, index: i32, ptr: i32, capacity: i32| {
                realm_arg_read(&mut caller, index, ptr, capacity)
            },
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "cwd-read",
            |mut caller: Caller<'_, HostState>, ptr: i32, capacity: i32| {
                realm_cwd_read(&mut caller, ptr, capacity)
            },
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "write",
            |mut caller: Caller<'_, HostState>, fd: i32, ptr: i32, len: i32| {
                realm_write(&mut caller, fd, ptr, len)
            },
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "open",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32, mode: i32| {
                realm_open(&mut caller, ptr, len, mode)
            },
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "read",
            |mut caller: Caller<'_, HostState>, fd: i32, ptr: i32, capacity: i32| {
                realm_read(&mut caller, fd, ptr, capacity)
            },
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "close",
            |mut caller: Caller<'_, HostState>, fd: i32| realm_close(&mut caller, fd),
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "clock-monotonic-ns",
            |caller: Caller<'_, HostState>| -> i64 { caller.data().monotonic_ns },
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    linker
        .func_wrap(
            IMPORT_MODULE_V0,
            "exit",
            |_caller: Caller<'_, HostState>, status: i32| -> Result<(), WasmiError> {
                Err(WasmiError::i32_exit(status))
            },
        )
        .map_err(|error| LaunchError::RuntimeConfiguration(error.to_string()))?;
    Ok(())
}

const MAX_IO_BYTES: usize = 64 * 1024;

fn realm_arg_len(caller: &Caller<'_, HostState>, index: i32) -> Result<i32, WasmiError> {
    let index = usize::try_from(index).map_err(|_| WasmiError::host(HostFault::MissingArgument))?;
    let argument = caller
        .data()
        .argv
        .get(index)
        .ok_or_else(|| WasmiError::host(HostFault::MissingArgument))?;
    i32::try_from(argument.len()).map_err(|_| WasmiError::host(HostFault::InvalidArgument))
}

fn realm_arg_read(
    caller: &mut Caller<'_, HostState>,
    index: i32,
    ptr: i32,
    capacity: i32,
) -> Result<i32, WasmiError> {
    let index = usize::try_from(index).map_err(|_| WasmiError::host(HostFault::MissingArgument))?;
    let bytes = caller
        .data()
        .argv
        .get(index)
        .ok_or_else(|| WasmiError::host(HostFault::MissingArgument))?
        .as_bytes()
        .to_vec();
    copy_process_bytes(caller, ptr, capacity, &bytes)
}

fn realm_cwd_read(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    capacity: i32,
) -> Result<i32, WasmiError> {
    let bytes = caller.data().cwd.as_bytes().to_vec();
    copy_process_bytes(caller, ptr, capacity, &bytes)
}

fn copy_process_bytes(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    capacity: i32,
    bytes: &[u8],
) -> Result<i32, WasmiError> {
    let capacity =
        usize::try_from(capacity).map_err(|_| WasmiError::host(HostFault::InvalidPointer))?;
    if bytes.len() > capacity {
        return Err(WasmiError::host(HostFault::BufferTooSmall));
    }
    write_guest_bytes(caller, ptr, capacity, bytes)?;
    i32::try_from(bytes.len()).map_err(|_| WasmiError::host(HostFault::InvalidArgument))
}

fn realm_open(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
    mode: i32,
) -> Result<i32, WasmiError> {
    let length = usize::try_from(len).map_err(|_| WasmiError::host(HostFault::InvalidPointer))?;
    if length == 0 || length > MAX_PATH_BYTES {
        return Err(WasmiError::host(HostFault::InvalidArgument));
    }
    let bytes = read_guest_bytes(caller, ptr, length)?;
    let path = String::from_utf8(bytes).map_err(|_| WasmiError::host(HostFault::InvalidUtf8))?;
    let mode = match mode {
        OPEN_READ => OpenMode::Read,
        OPEN_WRITE_TRUNCATE => OpenMode::WriteTruncate,
        _ => return Err(WasmiError::host(HostFault::InvalidArgument)),
    };
    let cwd = caller.data().cwd.clone();
    let file = caller
        .data_mut()
        .realm_host
        .open(&cwd, &path, mode)
        .map_err(|error| WasmiError::host(HostFault::Io(error)))?;
    let mut fd = caller.data().next_fd;
    while caller.data().files.contains_key(&fd) || core_descriptor_exists(caller.data(), fd) {
        fd = fd
            .checked_add(1)
            .ok_or_else(|| WasmiError::host(HostFault::InvalidArgument))?;
    }
    let next_fd = fd
        .checked_add(1)
        .ok_or_else(|| WasmiError::host(HostFault::InvalidArgument))?;
    caller.data_mut().next_fd = next_fd;
    caller.data_mut().files.insert(fd, file);
    Ok(fd)
}

fn core_descriptor_exists(state: &HostState, fd: i32) -> bool {
    state.process.as_ref().is_some_and(|context| {
        context
            .kernel
            .borrow()
            .descriptor(context.process, Descriptor::new(fd))
            .is_ok()
    })
}

fn realm_read(
    caller: &mut Caller<'_, HostState>,
    fd: i32,
    ptr: i32,
    capacity: i32,
) -> Result<i32, WasmiError> {
    let capacity =
        usize::try_from(capacity).map_err(|_| WasmiError::host(HostFault::InvalidPointer))?;
    if capacity > MAX_IO_BYTES {
        return Err(WasmiError::host(HostFault::InvalidArgument));
    }
    let (_, pointer) = validate_guest_range(caller, ptr, capacity)?;
    if let Some(context) = caller.data().process.clone()
        && let Some(resource) = process_descriptor(&context, fd)?
    {
        let DescriptorResource::PipeRead(_) = resource else {
            return Err(WasmiError::host(HostFault::InvalidArgument));
        };
        let descriptor = Descriptor::new(fd);
        let result = context
            .kernel
            .borrow_mut()
            .read_pipe(context.process, descriptor, capacity)
            .map_err(kernel_host_error)?;
        return match result {
            PipeReadResult::Data(bytes) => {
                write_guest_bytes(caller, ptr, capacity, &bytes)?;
                i32::try_from(bytes.len()).map_err(|_| WasmiError::host(HostFault::InvalidArgument))
            }
            PipeReadResult::Eof => Ok(0),
            PipeReadResult::WouldBlock => {
                let parked = context
                    .kernel
                    .borrow_mut()
                    .park_pipe_read(context.process, descriptor)
                    .map_err(kernel_host_error)?;
                if parked != ParkResult::Parked {
                    return Err(WasmiError::host(HostFault::InvalidArgument));
                }
                caller.data_mut().pending_io = Some(PendingIo::Read {
                    descriptor,
                    pointer,
                    capacity,
                });
                caller.data_mut().suspensions = caller.data().suspensions.saturating_add(1);
                Err(WasmiError::host(ProcessSuspended))
            }
        };
    }
    let bytes = caller
        .data_mut()
        .files
        .get_mut(&fd)
        .ok_or_else(|| WasmiError::host(HostFault::UnknownDescriptor(fd)))?
        .read(capacity)
        .map_err(|error| WasmiError::host(HostFault::Io(error)))?;
    if bytes.len() > capacity {
        return Err(WasmiError::host(HostFault::Io(RealmIoError::TooLarge)));
    }
    write_guest_bytes(caller, ptr, capacity, &bytes)?;
    i32::try_from(bytes.len()).map_err(|_| WasmiError::host(HostFault::InvalidArgument))
}

fn realm_close(caller: &mut Caller<'_, HostState>, fd: i32) -> Result<i32, WasmiError> {
    if let Some(context) = caller.data().process.clone()
        && process_descriptor(&context, fd)?.is_some()
    {
        context
            .kernel
            .borrow_mut()
            .close_descriptor(context.process, Descriptor::new(fd))
            .map_err(kernel_host_error)?;
        return Ok(0);
    }
    let mut file = caller
        .data_mut()
        .files
        .remove(&fd)
        .ok_or_else(|| WasmiError::host(HostFault::UnknownDescriptor(fd)))?;
    file.close()
        .map_err(|error| WasmiError::host(HostFault::Io(error)))?;
    Ok(0)
}

fn validate_guest_range(
    caller: &Caller<'_, HostState>,
    ptr: i32,
    length: usize,
) -> Result<(wasmi::Memory, usize), WasmiError> {
    let offset = usize::try_from(ptr).map_err(|_| WasmiError::host(HostFault::InvalidPointer))?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| WasmiError::host(HostFault::InvalidPointer))?;
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| WasmiError::host(HostFault::MissingMemory))?;
    if end > memory.data_size(caller) {
        return Err(WasmiError::host(HostFault::InvalidPointer));
    }
    Ok((memory, offset))
}

fn read_guest_bytes(
    caller: &Caller<'_, HostState>,
    ptr: i32,
    length: usize,
) -> Result<Vec<u8>, WasmiError> {
    if length > MAX_IO_BYTES {
        return Err(WasmiError::host(HostFault::InvalidArgument));
    }
    let (memory, offset) = validate_guest_range(caller, ptr, length)?;
    let mut bytes = vec![0; length];
    memory
        .read(caller, offset, &mut bytes)
        .map_err(|_| WasmiError::host(HostFault::InvalidPointer))?;
    Ok(bytes)
}

fn write_guest_bytes(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    capacity: usize,
    bytes: &[u8],
) -> Result<(), WasmiError> {
    if bytes.len() > capacity || capacity > MAX_IO_BYTES {
        return Err(WasmiError::host(HostFault::BufferTooSmall));
    }
    let (memory, offset) = validate_guest_range(caller, ptr, capacity)?;
    memory
        .write(caller, offset, bytes)
        .map_err(|_| WasmiError::host(HostFault::InvalidPointer))
}

fn realm_write(
    caller: &mut Caller<'_, HostState>,
    fd: i32,
    ptr: i32,
    len: i32,
) -> Result<i32, WasmiError> {
    let length = usize::try_from(len).map_err(|_| WasmiError::host(HostFault::InvalidPointer))?;
    if let Some(context) = caller.data().process.clone()
        && let Some(resource) = process_descriptor(&context, fd)?
    {
        let DescriptorResource::PipeWrite(_) = resource else {
            return Err(WasmiError::host(HostFault::InvalidArgument));
        };
        let descriptor = Descriptor::new(fd);
        let bytes = read_guest_bytes(caller, ptr, length)?;
        let result = context
            .kernel
            .borrow_mut()
            .write_pipe(context.process, descriptor, &bytes)
            .map_err(kernel_host_error)?;
        return match result {
            PipeWriteResult::Written(written) => {
                i32::try_from(written).map_err(|_| WasmiError::host(HostFault::InvalidArgument))
            }
            PipeWriteResult::BrokenPipe => Err(WasmiError::host(HostFault::BrokenPipe)),
            PipeWriteResult::WouldBlock => {
                let parked = context
                    .kernel
                    .borrow_mut()
                    .park_pipe_write(context.process, descriptor)
                    .map_err(kernel_host_error)?;
                if parked != ParkResult::Parked {
                    return Err(WasmiError::host(HostFault::InvalidArgument));
                }
                caller.data_mut().pending_io = Some(PendingIo::Write { descriptor, bytes });
                caller.data_mut().suspensions = caller.data().suspensions.saturating_add(1);
                Err(WasmiError::host(ProcessSuspended))
            }
        };
    }
    match fd {
        STDOUT_FD | STDERR_FD => {
            let new_total = caller
                .data()
                .output_len()
                .checked_add(length)
                .ok_or_else(|| WasmiError::host(HostFault::OutputLimit))?;
            if new_total > caller.data().output_limit {
                return Err(WasmiError::host(HostFault::OutputLimit));
            }
            let bytes = read_guest_bytes(caller, ptr, length)?;
            if fd == STDOUT_FD {
                caller.data_mut().stdout.extend_from_slice(&bytes);
            } else {
                caller.data_mut().stderr.extend_from_slice(&bytes);
            }
            Ok(len)
        }
        file_fd => {
            if !caller.data().files.contains_key(&file_fd) {
                return Err(WasmiError::host(HostFault::UnknownDescriptor(file_fd)));
            }
            let bytes = read_guest_bytes(caller, ptr, length)?;
            let written = caller
                .data_mut()
                .files
                .get_mut(&file_fd)
                .expect("descriptor existence checked")
                .write(&bytes)
                .map_err(|error| WasmiError::host(HostFault::Io(error)))?;
            i32::try_from(written).map_err(|_| WasmiError::host(HostFault::InvalidArgument))
        }
    }
}

fn process_descriptor(
    context: &ProcessContext,
    fd: i32,
) -> Result<Option<DescriptorResource>, WasmiError> {
    let descriptor = Descriptor::new(fd);
    match context
        .kernel
        .borrow()
        .descriptor(context.process, descriptor)
    {
        Ok(resource) => Ok(Some(resource)),
        Err(KernelError::DescriptorNotFound { .. }) => Ok(None),
        Err(error) => Err(kernel_host_error(error)),
    }
}

fn kernel_host_error(_error: KernelError) -> WasmiError {
    WasmiError::host(HostFault::InvalidArgument)
}

fn classify_process_error(error: &WasmiError) -> ProcessOutcome {
    if let Some(status) = error.i32_exit_status() {
        return ProcessOutcome::Exited(status);
    }
    if error.as_trap_code() == Some(TrapCode::OutOfFuel) {
        return ProcessOutcome::FuelExhausted;
    }
    if let Some(fault) = error.downcast_ref::<HostFault>() {
        return ProcessOutcome::HostFault(*fault);
    }
    ProcessOutcome::Trapped(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Default)]
    struct MemoryRealmHost {
        files: Rc<RefCell<BTreeMap<String, Vec<u8>>>>,
    }

    impl MemoryRealmHost {
        fn contents(&self, path: &str) -> Option<Vec<u8>> {
            self.files.borrow().get(path).cloned()
        }
    }

    impl RealmHost for MemoryRealmHost {
        fn open(
            &mut self,
            cwd: &str,
            path: &str,
            mode: OpenMode,
        ) -> Result<Box<dyn RealmFile>, RealmIoError> {
            let resolved = if path.starts_with('/') {
                path.to_string()
            } else {
                format!("{}/{path}", cwd.trim_end_matches('/'))
            };
            if mode == OpenMode::Read && !self.files.borrow().contains_key(&resolved) {
                return Err(RealmIoError::NotFound);
            }
            if mode == OpenMode::WriteTruncate {
                self.files.borrow_mut().insert(resolved.clone(), Vec::new());
            }
            Ok(Box::new(MemoryRealmFile {
                files: Rc::clone(&self.files),
                path: resolved,
                offset: 0,
                mode,
            }))
        }
    }

    struct MemoryRealmFile {
        files: Rc<RefCell<BTreeMap<String, Vec<u8>>>>,
        path: String,
        offset: usize,
        mode: OpenMode,
    }

    impl RealmFile for MemoryRealmFile {
        fn read(&mut self, max_bytes: usize) -> Result<Vec<u8>, RealmIoError> {
            if self.mode != OpenMode::Read {
                return Err(RealmIoError::Denied);
            }
            let files = self.files.borrow();
            let bytes = files.get(&self.path).ok_or(RealmIoError::NotFound)?;
            let end = self.offset.saturating_add(max_bytes).min(bytes.len());
            let result = bytes[self.offset..end].to_vec();
            self.offset = end;
            Ok(result)
        }

        fn write(&mut self, bytes: &[u8]) -> Result<usize, RealmIoError> {
            if self.mode != OpenMode::WriteTruncate {
                return Err(RealmIoError::Denied);
            }
            let mut files = self.files.borrow_mut();
            let file = files.get_mut(&self.path).ok_or(RealmIoError::NotFound)?;
            let end = self
                .offset
                .checked_add(bytes.len())
                .ok_or(RealmIoError::TooLarge)?;
            if end > file.len() {
                file.resize(end, 0);
            }
            file[self.offset..end].copy_from_slice(bytes);
            self.offset = end;
            Ok(bytes.len())
        }
    }

    fn compile(wat_source: &str) -> Vec<u8> {
        wat::parse_str(wat_source).expect("valid test WAT")
    }

    #[test]
    fn smoke_guest_runs_behind_realm_imports() {
        let report = RealmRuntime::default()
            .execute(SMOKE_WRITE_GUEST, RunLimits::default())
            .expect("smoke guest launches");

        assert_eq!(report.outcome, ProcessOutcome::Exited(0));
        assert_eq!(report.stdout, b"hello from AOS Realm\n");
        assert!(report.stderr.is_empty());
        assert!(report.fuel_consumed > 0);
        assert_eq!(report.memory_limit_bytes, 64 * 1024);
    }

    #[test]
    fn pwd_reads_the_guest_visible_current_directory() {
        let report = RealmRuntime::default()
            .execute_process(
                PWD_GUEST,
                ProcessConfig {
                    argv: vec!["pwd".to_string()],
                    cwd: "/workspace/project".to_string(),
                },
                RunLimits::default(),
                Box::<MemoryRealmHost>::default(),
            )
            .expect("pwd guest launches");

        assert_eq!(report.outcome, ProcessOutcome::Exited(0));
        assert_eq!(report.stdout, b"/workspace/project\n");
    }

    #[test]
    fn echo_reads_argv_through_the_guest_abi() {
        let report = RealmRuntime::default()
            .execute_process(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "hello realm".to_string()],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<MemoryRealmHost>::default(),
            )
            .expect("echo guest launches");

        assert_eq!(report.outcome, ProcessOutcome::Exited(0));
        assert_eq!(report.stdout, b"hello realm\n");
    }

    #[test]
    fn write_and_cat_persist_across_process_instances() {
        let host = MemoryRealmHost::default();
        let runtime = RealmRuntime::default();
        let write = runtime
            .execute_process(
                WRITE_FILE_GUEST,
                ProcessConfig {
                    argv: vec![
                        "write-file".to_string(),
                        "note.txt".to_string(),
                        "durable bytes".to_string(),
                    ],
                    cwd: "/workspace/project".to_string(),
                },
                RunLimits::default(),
                Box::new(host.clone()),
            )
            .expect("write guest launches");

        assert_eq!(write.outcome, ProcessOutcome::Exited(0));
        assert_eq!(
            host.contents("/workspace/project/note.txt"),
            Some(b"durable bytes".to_vec())
        );

        let read = runtime
            .execute_process(
                CAT_GUEST,
                ProcessConfig {
                    argv: vec!["cat".to_string(), "note.txt".to_string()],
                    cwd: "/workspace/project".to_string(),
                },
                RunLimits::default(),
                Box::new(host),
            )
            .expect("cat guest launches");

        assert_eq!(read.outcome, ProcessOutcome::Exited(0));
        assert_eq!(read.stdout, b"durable bytes");
    }

    #[test]
    fn missing_command_argument_is_a_stable_host_fault() {
        let report = RealmRuntime::default()
            .execute_process(
                CAT_GUEST,
                ProcessConfig {
                    argv: vec!["cat".to_string()],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<MemoryRealmHost>::default(),
            )
            .expect("guest launches before requesting absent argument");

        assert_eq!(
            report.outcome,
            ProcessOutcome::HostFault(HostFault::MissingArgument)
        );
    }

    #[test]
    fn oversized_argument_vector_is_rejected_before_launch() {
        let error = RealmRuntime::default()
            .execute_process(
                ECHO_GUEST,
                ProcessConfig {
                    argv: vec!["echo".to_string(), "x".repeat(MAX_ARGUMENT_BYTES)],
                    cwd: "/workspace".to_string(),
                },
                RunLimits::default(),
                Box::<MemoryRealmHost>::default(),
            )
            .expect_err("oversized arguments must fail admission");

        assert!(matches!(error, LaunchError::InvalidProcess(_)));
    }

    #[test]
    fn malformed_module_is_rejected_before_launch() {
        let error = RealmRuntime::default()
            .execute(&[0x00], RunLimits::default())
            .expect_err("malformed bytes must fail");

        assert!(matches!(error, LaunchError::InvalidModule(_)));
    }

    #[test]
    fn undeclared_import_is_rejected() {
        let wasm = compile(
            r#"(module
                (import "host" "ambient" (func $ambient))
                (func (export "_start") (call $ambient)))"#,
        );
        let error = RealmRuntime::default()
            .execute(&wasm, RunLimits::default())
            .expect_err("ambient import must fail");

        assert!(matches!(error, LaunchError::Instantiation(_)));
    }

    #[test]
    fn out_of_bounds_pointer_becomes_stable_host_fault() {
        let wasm = compile(
            r#"(module
                (import "aos_realm_v0" "write"
                    (func $write (param i32 i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (func (export "_start")
                    (drop (call $write (i32.const 1) (i32.const 65535) (i32.const 2)))))"#,
        );
        let report = RealmRuntime::default()
            .execute(&wasm, RunLimits::default())
            .expect("guest launches");

        assert_eq!(
            report.outcome,
            ProcessOutcome::HostFault(HostFault::InvalidPointer)
        );
    }

    #[test]
    fn unknown_descriptor_is_rejected() {
        let wasm = compile(
            r#"(module
                (import "aos_realm_v0" "write"
                    (func $write (param i32 i32 i32) (result i32)))
                (memory (export "memory") 1 1)
                (data (i32.const 0) "x")
                (func (export "_start")
                    (drop (call $write (i32.const 9) (i32.const 0) (i32.const 1)))))"#,
        );
        let report = RealmRuntime::default()
            .execute(&wasm, RunLimits::default())
            .expect("guest launches");

        assert_eq!(
            report.outcome,
            ProcessOutcome::HostFault(HostFault::UnknownDescriptor(9))
        );
    }

    #[test]
    fn output_is_bounded_before_copying() {
        let limits = RunLimits {
            output_bytes: 4,
            ..RunLimits::default()
        };
        let report = RealmRuntime::default()
            .execute(SMOKE_WRITE_GUEST, limits)
            .expect("guest launches");

        assert_eq!(
            report.outcome,
            ProcessOutcome::HostFault(HostFault::OutputLimit)
        );
        assert!(report.stdout.is_empty());
    }

    #[test]
    fn infinite_guest_exhausts_fuel() {
        let wasm = compile(
            r#"(module
                (func (export "_start")
                    (loop $forever (br $forever))))"#,
        );
        let limits = RunLimits {
            fuel: 100,
            ..RunLimits::default()
        };
        let report = RealmRuntime::default()
            .execute(&wasm, limits)
            .expect("guest launches");

        assert_eq!(report.outcome, ProcessOutcome::FuelExhausted);
        assert_eq!(report.fuel_consumed, limits.fuel);
    }

    #[test]
    fn declared_memory_over_limit_is_rejected() {
        let wasm = compile(
            r#"(module
                (memory 2 2)
                (func (export "_start")))"#,
        );
        let error = RealmRuntime::default()
            .execute(&wasm, RunLimits::default())
            .expect_err("two pages exceed one-page limit");

        assert!(matches!(error, LaunchError::Instantiation(_)));
    }
}
