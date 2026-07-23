use super::*;

const MAGIC: &[u8; 8] = b"AOSRVCHK";
const FORMAT_VERSION: u32 = 1;
const DIGEST_BYTES: usize = 32;
const PAGE_BYTES: usize = 4096;

/// Domain-bearing digest used to bind a machine checkpoint to immutable input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointDigest([u8; DIGEST_BYTES]);

impl CheckpointDigest {
    /// Construct a digest already verified by the artifact build pipeline.
    #[must_use]
    pub const fn new(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    /// Hash immutable artifact bytes with the checkpoint format's digest.
    #[must_use]
    pub fn hash(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Borrow the canonical digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }
}

/// Immutable identities that make a prewarmed machine state admissible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointBinding {
    linux_image: CheckpointDigest,
    distribution: CheckpointDigest,
}

impl CheckpointBinding {
    /// Bind a checkpoint to one Linux image and one distribution generation.
    #[must_use]
    pub const fn new(linux_image: CheckpointDigest, distribution: CheckpointDigest) -> Self {
        Self {
            linux_image,
            distribution,
        }
    }

    /// Exact Linux image digest.
    #[must_use]
    pub const fn linux_image(self) -> CheckpointDigest {
        self.linux_image
    }

    /// Exact immutable distribution-generation digest.
    #[must_use]
    pub const fn distribution(self) -> CheckpointDigest {
        self.distribution
    }
}

/// Rejected durable checkpoint bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointDecodeError {
    /// The fixed format marker is absent.
    InvalidMagic,
    /// The codec version is not implemented by this machine.
    UnsupportedVersion(u32),
    /// The checkpoint targets another machine model.
    MachineModelMismatch,
    /// Image or distribution identity differs from the selected artifact.
    BindingMismatch,
    /// The payload digest does not match its bytes.
    IntegrityMismatch,
    /// A length-prefixed field ends beyond the admitted input.
    Truncated,
    /// Bytes remained after the final format field.
    TrailingBytes,
    /// A decoded value violates a machine invariant.
    InvalidField(&'static str),
    /// The encoded resource envelope is not admissible.
    Machine(MachineError),
}

impl fmt::Display for CheckpointDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => write!(f, "invalid machine checkpoint marker"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported machine checkpoint version {version}")
            }
            Self::MachineModelMismatch => write!(f, "machine checkpoint model does not match"),
            Self::BindingMismatch => write!(f, "machine checkpoint binding does not match"),
            Self::IntegrityMismatch => write!(f, "machine checkpoint integrity check failed"),
            Self::Truncated => write!(f, "machine checkpoint is truncated"),
            Self::TrailingBytes => write!(f, "machine checkpoint has trailing bytes"),
            Self::InvalidField(field) => {
                write!(f, "machine checkpoint contains invalid {field}")
            }
            Self::Machine(error) => write!(f, "machine checkpoint resources are invalid: {error}"),
        }
    }
}

impl std::error::Error for CheckpointDecodeError {}

impl From<MachineError> for CheckpointDecodeError {
    fn from(error: MachineError) -> Self {
        Self::Machine(error)
    }
}

impl MachineCheckpoint {
    /// Encode a sparse, integrity-checked durable checkpoint.
    ///
    /// The outer signed capsule artifact supplies authenticity; the embedded
    /// checksum detects corruption and the binding prevents cross-image reuse.
    #[must_use]
    pub fn encode(&self, binding: CheckpointBinding) -> Vec<u8> {
        let machine = &self.machine;
        let mut bytes = Vec::with_capacity(machine.devices.ram.len() / 2);
        bytes.extend_from_slice(MAGIC);
        push_u32(&mut bytes, FORMAT_VERSION);
        push_len_bytes(&mut bytes, MACHINE_MODEL.as_bytes());
        bytes.extend_from_slice(binding.linux_image.as_bytes());
        bytes.extend_from_slice(binding.distribution.as_bytes());
        push_u64(&mut bytes, machine.config.ram_bytes as u64);
        push_u64(&mut bytes, machine.config.max_console_bytes as u64);
        push_u32(
            &mut bytes,
            u32::try_from(machine.hart_count).expect("admitted hart count fits u32"),
        );
        push_u32(
            &mut bytes,
            u32::try_from(machine.active_hart_id).expect("admitted hart id fits u32"),
        );
        push_u64(&mut bytes, machine.scheduler_quantum_remaining);
        let active = &machine.harts[machine.active_hart_id];
        encode_hart(
            &mut bytes,
            HartCheckpointRef {
                lifecycle: active.lifecycle,
                cpu: &active.cpu,
                csrs: &active.csrs,
                cycle: active.cycle,
                instret: active.instret,
                reservation: active.reservation,
                mtimecmp: active.mtimecmp,
                msip: active.msip,
            },
        );
        for hart_id in 0..machine.hart_count {
            if hart_id == machine.active_hart_id {
                continue;
            }
            let hart = &machine.harts[hart_id];
            push_u32(
                &mut bytes,
                u32::try_from(hart.id).expect("admitted hart id fits u32"),
            );
            encode_hart(
                &mut bytes,
                HartCheckpointRef {
                    lifecycle: hart.lifecycle,
                    cpu: &hart.cpu,
                    csrs: &hart.csrs,
                    cycle: hart.cycle,
                    instret: hart.instret,
                    reservation: hart.reservation,
                    mtimecmp: hart.mtimecmp,
                    msip: hart.msip,
                },
            );
        }

        push_u64(&mut bytes, machine.devices.mtime);
        push_u64(&mut bytes, machine.steps_executed);
        push_u64(&mut bytes, machine.instructions_retired);
        bytes.push(u8::from(machine.firmware.enabled));
        push_u64(&mut bytes, machine.next_host_request_id);

        let pending = machine
            .pending_9p_request
            .as_ref()
            .expect("checkpoint constructor requires a pending request");
        push_u64(&mut bytes, pending.request.id.get());
        push_u32(&mut bytes, pending.request.channel);
        push_len_bytes(&mut bytes, &pending.request.message);
        push_u64(&mut bytes, pending.request.max_response_bytes as u64);
        push_u64(&mut bytes, pending.response_address);
        push_u32(
            &mut bytes,
            u32::try_from(pending.hart_id).expect("admitted request hart id fits u32"),
        );

        let populated_pages = machine.devices.ram.nonzero_pages(PAGE_BYTES);
        push_u32(
            &mut bytes,
            u32::try_from(populated_pages.len()).expect("admitted RAM has at most 65,536 pages"),
        );
        for (index, page) in populated_pages {
            push_u32(
                &mut bytes,
                u32::try_from(index).expect("admitted RAM page index fits u32"),
            );
            bytes.extend_from_slice(&page);
        }

        let digest = blake3::hash(&bytes);
        bytes.extend_from_slice(digest.as_bytes());
        bytes
    }

    /// Decode and validate a durable checkpoint before it can enter a Realm.
    pub fn decode(
        bytes: &[u8],
        expected_binding: CheckpointBinding,
    ) -> Result<Self, CheckpointDecodeError> {
        if bytes.len() < DIGEST_BYTES {
            return Err(CheckpointDecodeError::Truncated);
        }
        let (payload, encoded_digest) = bytes.split_at(bytes.len() - DIGEST_BYTES);
        if blake3::hash(payload).as_bytes() != encoded_digest {
            return Err(CheckpointDecodeError::IntegrityMismatch);
        }

        let mut decoder = Decoder::new(payload);
        if decoder.take(MAGIC.len())? != MAGIC {
            return Err(CheckpointDecodeError::InvalidMagic);
        }
        let version = decoder.u32()?;
        if version != FORMAT_VERSION {
            return Err(CheckpointDecodeError::UnsupportedVersion(version));
        }
        if decoder.len_bytes()? != MACHINE_MODEL.as_bytes() {
            return Err(CheckpointDecodeError::MachineModelMismatch);
        }
        let linux_image = decoder.array::<DIGEST_BYTES>()?;
        let distribution = decoder.array::<DIGEST_BYTES>()?;
        let binding = CheckpointBinding::new(
            CheckpointDigest::new(linux_image),
            CheckpointDigest::new(distribution),
        );
        if binding != expected_binding {
            return Err(CheckpointDecodeError::BindingMismatch);
        }

        let ram_bytes = decoder.usize("RAM byte length")?;
        let max_console_bytes = decoder.usize("console byte limit")?;
        let config = MachineConfig {
            ram_bytes,
            max_console_bytes,
        };
        let hart_count = decoder.u32_usize()?;
        let active_hart_id = decoder.u32_usize()?;
        let scheduler_quantum_remaining = decoder.u64()?;
        if !(1..=MAX_HARTS).contains(&hart_count) || active_hart_id >= hart_count {
            return Err(CheckpointDecodeError::InvalidField("hart topology"));
        }
        if scheduler_quantum_remaining > HART_SCHEDULER_QUANTUM {
            return Err(CheckpointDecodeError::InvalidField(
                "hart scheduler quantum",
            ));
        }
        let mut machine = Machine::new_with_harts(config, hart_count)?;
        let active = decode_hart(&mut decoder, &machine.devices)?;
        let mut harts = (0..hart_count)
            .map(|hart_id| HartState::new(hart_id, HartLifecycle::Stopped))
            .collect::<Vec<_>>();
        for expected_hart_id in 0..hart_count {
            if expected_hart_id == active_hart_id {
                continue;
            }
            let hart_id = decoder.u32_usize()?;
            if hart_id != expected_hart_id {
                return Err(CheckpointDecodeError::InvalidField("parked hart order"));
            }
            let hart = decode_hart(&mut decoder, &machine.devices)?;
            harts[hart_id] = HartState {
                id: hart_id,
                lifecycle: hart.lifecycle,
                cpu: hart.cpu,
                csrs: hart.csrs,
                cycle: hart.cycle,
                instret: hart.instret,
                reservation: hart.reservation,
                mtimecmp: hart.mtimecmp,
                msip: hart.msip,
                translation_cache: TranslationCache::default(),
            };
        }
        harts[active_hart_id] = HartState {
            id: active_hart_id,
            lifecycle: active.lifecycle,
            cpu: active.cpu,
            csrs: active.csrs,
            cycle: active.cycle,
            instret: active.instret,
            reservation: active.reservation,
            mtimecmp: active.mtimecmp,
            msip: active.msip,
            translation_cache: TranslationCache::default(),
        };
        machine.active_hart_id = active_hart_id;
        machine.harts = harts;
        machine.scheduler_quantum_remaining = scheduler_quantum_remaining;

        machine.devices.mtime = decoder.u64()?;
        machine.steps_executed = decoder.u64()?;
        machine.instructions_retired = decoder.u64()?;
        machine.firmware.enabled = decoder.boolean("firmware state")?;
        if !machine.firmware.enabled {
            return Err(CheckpointDecodeError::InvalidField("firmware state"));
        }
        machine.next_host_request_id = decoder.u64()?;

        let request_id = HostRequestId(decoder.u64()?);
        let channel = decoder.u32()?;
        let message = decoder.len_bytes()?.to_vec();
        if channel == 0 || !(MIN_9P_MESSAGE_BYTES..=MAX_9P_MESSAGE_BYTES).contains(&message.len()) {
            return Err(CheckpointDecodeError::InvalidField("host request"));
        }
        let max_response_bytes = decoder.usize("host response byte limit")?;
        if !(MIN_9P_MESSAGE_BYTES..=MAX_9P_MESSAGE_BYTES).contains(&max_response_bytes) {
            return Err(CheckpointDecodeError::InvalidField(
                "host response byte limit",
            ));
        }
        let response_address = decoder.u64()?;
        let request_hart_id = decoder.u32_usize()?;
        if machine
            .devices
            .ram_range_len(response_address, max_response_bytes)
            .is_none()
            || request_id.get() == 0
            || machine.next_host_request_id <= request_id.get()
            || request_hart_id >= hart_count
        {
            return Err(CheckpointDecodeError::InvalidField(
                "host request identity, hart, or response buffer",
            ));
        }
        machine.pending_9p_request = Some(PendingPlan9Request {
            request: Plan9Request {
                id: request_id,
                channel,
                message,
                max_response_bytes,
            },
            response_address,
            hart_id: request_hart_id,
        });

        let populated_pages = decoder.u32_usize()?;
        let total_pages = ram_bytes / PAGE_BYTES;
        if populated_pages > total_pages {
            return Err(CheckpointDecodeError::InvalidField("populated page count"));
        }
        let mut previous_page = None;
        for _ in 0..populated_pages {
            let page = decoder.u32_usize()?;
            if page >= total_pages || previous_page.is_some_and(|previous| page <= previous) {
                return Err(CheckpointDecodeError::InvalidField("RAM page order"));
            }
            let page_bytes = decoder.take(PAGE_BYTES)?;
            if !machine.devices.ram.write_page(page, PAGE_BYTES, page_bytes) {
                return Err(CheckpointDecodeError::InvalidField("RAM page range"));
            }
            previous_page = Some(page);
        }
        if !decoder.is_empty() {
            return Err(CheckpointDecodeError::TrailingBytes);
        }

        machine.state = RunState::Runnable;
        machine.metrics = MachineMetrics::default();
        for hart in &mut machine.harts {
            hart.translation_cache.clear();
        }
        Ok(Self { machine })
    }

    /// Consume the checkpoint without cloning its admitted RAM.
    #[must_use]
    pub fn into_machine(mut self) -> Machine {
        self.machine.translation_cache.clear();
        self.machine
    }
}

struct HartCheckpointRef<'a> {
    lifecycle: HartLifecycle,
    cpu: &'a Cpu,
    csrs: &'a CsrFile,
    cycle: u64,
    instret: u64,
    reservation: Option<(u64, u8)>,
    mtimecmp: u64,
    msip: bool,
}

fn encode_hart(bytes: &mut Vec<u8>, hart: HartCheckpointRef<'_>) {
    bytes.push(match hart.lifecycle {
        HartLifecycle::Started => 1,
        HartLifecycle::Stopped => 0,
    });
    for register in hart.cpu.registers {
        push_u64(bytes, register);
    }
    for register in hart.cpu.floating_registers {
        push_u64(bytes, register);
    }
    push_u64(bytes, hart.cpu.pc);
    bytes.push(hart.cpu.privilege as u8);
    encode_csrs(bytes, hart.csrs);
    push_u64(bytes, hart.cycle);
    push_u64(bytes, hart.instret);
    match hart.reservation {
        Some((address, width)) => {
            bytes.push(1);
            push_u64(bytes, address);
            bytes.push(width);
        }
        None => bytes.push(0),
    }
    push_u64(bytes, hart.mtimecmp);
    bytes.push(u8::from(hart.msip));
}

struct DecodedHart {
    lifecycle: HartLifecycle,
    cpu: Cpu,
    csrs: CsrFile,
    cycle: u64,
    instret: u64,
    reservation: Option<(u64, u8)>,
    mtimecmp: u64,
    msip: bool,
}

fn decode_hart(
    decoder: &mut Decoder<'_>,
    devices: &Devices,
) -> Result<DecodedHart, CheckpointDecodeError> {
    let lifecycle = match decoder.byte()? {
        0 => HartLifecycle::Stopped,
        1 => HartLifecycle::Started,
        _ => return Err(CheckpointDecodeError::InvalidField("hart lifecycle")),
    };
    let mut cpu = Cpu::new();
    for register in &mut cpu.registers {
        *register = decoder.u64()?;
    }
    if cpu.registers[0] != 0 {
        return Err(CheckpointDecodeError::InvalidField("zero register"));
    }
    for register in &mut cpu.floating_registers {
        *register = decoder.u64()?;
    }
    cpu.pc = decoder.u64()?;
    if cpu.pc & 1 != 0 {
        return Err(CheckpointDecodeError::InvalidField("program counter"));
    }
    cpu.privilege = decode_privilege(decoder.byte()?)?;
    let csrs = decode_csrs(decoder)?;
    validate_csrs(&csrs)?;
    let cycle = decoder.u64()?;
    let instret = decoder.u64()?;
    let reservation = match decoder.byte()? {
        0 => None,
        1 => {
            let address = decoder.u64()?;
            let width = decoder.byte()?;
            if !matches!(width, 4 | 8) || devices.ram_range(address, width).is_none() {
                return Err(CheckpointDecodeError::InvalidField("reservation"));
            }
            Some((address, width))
        }
        _ => return Err(CheckpointDecodeError::InvalidField("reservation marker")),
    };
    let mtimecmp = decoder.u64()?;
    let msip = decoder.boolean("software interrupt state")?;
    Ok(DecodedHart {
        lifecycle,
        cpu,
        csrs,
        cycle,
        instret,
        reservation,
        mtimecmp,
        msip,
    })
}

fn encode_csrs(bytes: &mut Vec<u8>, csrs: &CsrFile) {
    bytes.push(csrs.fcsr);
    for value in [
        csrs.mstatus,
        csrs.medeleg,
        csrs.mideleg,
        csrs.mie,
        csrs.mip,
        csrs.mcounteren,
        csrs.scounteren,
        csrs.satp,
        csrs.mtvec,
        csrs.mscratch,
        csrs.mepc,
        csrs.mcause,
        csrs.mtval,
        csrs.stvec,
        csrs.sscratch,
        csrs.sepc,
        csrs.scause,
        csrs.stval,
    ] {
        push_u64(bytes, value);
    }
}

fn decode_csrs(decoder: &mut Decoder<'_>) -> Result<CsrFile, CheckpointDecodeError> {
    Ok(CsrFile {
        fcsr: decoder.byte()?,
        mstatus: decoder.u64()?,
        medeleg: decoder.u64()?,
        mideleg: decoder.u64()?,
        mie: decoder.u64()?,
        mip: decoder.u64()?,
        mcounteren: decoder.u64()?,
        scounteren: decoder.u64()?,
        satp: decoder.u64()?,
        mtvec: decoder.u64()?,
        mscratch: decoder.u64()?,
        mepc: decoder.u64()?,
        mcause: decoder.u64()?,
        mtval: decoder.u64()?,
        stvec: decoder.u64()?,
        sscratch: decoder.u64()?,
        sepc: decoder.u64()?,
        scause: decoder.u64()?,
        stval: decoder.u64()?,
    })
}

fn validate_csrs(csrs: &CsrFile) -> Result<(), CheckpointDecodeError> {
    let satp_mode = csrs.satp >> SATP_MODE_SHIFT;
    if csrs.mstatus & !MSTATUS_WRITABLE != 0
        || csrs.medeleg & !MEDELEG_SUPPORTED != 0
        || csrs.mideleg & !MIDELEG_SUPPORTED != 0
        || csrs.mie & !INTERRUPT_SUPPORTED != 0
        || csrs.mip & !INTERRUPT_SUPPORTED != 0
        || csrs.mcounteren & !0b111 != 0
        || csrs.scounteren & !0b111 != 0
        || !matches!(satp_mode, SATP_MODE_BARE | SATP_MODE_SV39)
        || legal_trap_vector(csrs.mtvec) != csrs.mtvec
        || legal_trap_vector(csrs.stvec) != csrs.stvec
        || csrs.mepc & 1 != 0
        || csrs.sepc & 1 != 0
    {
        return Err(CheckpointDecodeError::InvalidField("CSR state"));
    }
    Ok(())
}

fn decode_privilege(value: u8) -> Result<Privilege, CheckpointDecodeError> {
    match value {
        0 => Ok(Privilege::User),
        1 => Ok(Privilege::Supervisor),
        3 => Ok(Privilege::Machine),
        _ => Err(CheckpointDecodeError::InvalidField("privilege")),
    }
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_len_bytes(target: &mut Vec<u8>, bytes: &[u8]) {
    push_u32(
        target,
        u32::try_from(bytes.len()).expect("checkpoint field length fits u32"),
    );
    target.extend_from_slice(bytes);
}

struct Decoder<'a> {
    remaining: &'a [u8],
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn take(&mut self, bytes: usize) -> Result<&'a [u8], CheckpointDecodeError> {
        let (value, remaining) = self
            .remaining
            .split_at_checked(bytes)
            .ok_or(CheckpointDecodeError::Truncated)?;
        self.remaining = remaining;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, CheckpointDecodeError> {
        Ok(self.take(1)?[0])
    }

    fn boolean(&mut self, field: &'static str) -> Result<bool, CheckpointDecodeError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CheckpointDecodeError::InvalidField(field)),
        }
    }

    fn u32(&mut self) -> Result<u32, CheckpointDecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, CheckpointDecodeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn usize(&mut self, field: &'static str) -> Result<usize, CheckpointDecodeError> {
        usize::try_from(self.u64()?).map_err(|_| CheckpointDecodeError::InvalidField(field))
    }

    fn u32_usize(&mut self) -> Result<usize, CheckpointDecodeError> {
        usize::try_from(self.u32()?)
            .map_err(|_| CheckpointDecodeError::InvalidField("32-bit length"))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CheckpointDecodeError> {
        self.take(N)?
            .try_into()
            .map_err(|_| CheckpointDecodeError::Truncated)
    }

    fn len_bytes(&mut self) -> Result<&'a [u8], CheckpointDecodeError> {
        let bytes = self.u32_usize()?;
        self.take(bytes)
    }

    const fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }
}
