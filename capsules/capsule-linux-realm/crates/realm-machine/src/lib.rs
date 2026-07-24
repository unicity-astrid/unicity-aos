#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

//! A bounded, slice-driven RV64 machine for the AOS Realm Linux backend.
//!
//! This crate is intentionally below the Linux compatibility policy. It owns
//! guest CPU state, admitted RAM, and virtual hardware. The outer Realm owns
//! scheduling, authority, image admission, persistence, and all host effects.

#[cfg(any(target_arch = "wasm32", test))]
mod atomic_ram;
mod checkpoint;
mod fdt;
mod floating;

#[cfg(target_arch = "wasm32")]
use atomic_ram::{AtomicGuestRam, AtomicReservation};
pub use checkpoint::{CheckpointBinding, CheckpointDecodeError, CheckpointDigest};
use fdt::{LinuxFdtConfig, build_linux_fdt};
use rustc_apfloat::{
    Float, FloatConvert, Round, Status as FloatStatus, StatusAnd,
    ieee::{Double, Single},
};
use std::{
    collections::VecDeque,
    fmt,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

/// Machine profile whose future device tree and Linux image are versioned together.
pub const MACHINE_MODEL: &str = "aos-rv64-virt-v1";

/// Guest physical address at which admitted RAM begins.
pub const DRAM_BASE: u64 = 0x8000_0000;

/// Guest physical base of the 16550-compatible serial device.
pub const UART_BASE: u64 = 0x1000_0000;

/// Guest physical base of the SiFive/QEMU-compatible test finisher.
pub const TEST_FINISHER_BASE: u64 = 0x0010_0000;

/// Guest physical base of the deterministic single-hart CLINT profile.
pub const CLINT_BASE: u64 = 0x0200_0000;

/// Standard 2 MiB-aligned entry address for a raw RV64 Linux `Image`.
pub const LINUX_KERNEL_BASE: u64 = DRAM_BASE + 0x20_0000;

/// Physical address of the generated Linux flattened device tree.
pub const LINUX_FDT_BASE: u64 = DRAM_BASE + 0x1000;

/// Hard guest-topology limit for the first deterministic scheduler.
pub const MAX_HARTS: usize = 64;

const UART_SIZE: u64 = 0x100;
const TEST_FINISHER_SIZE: u64 = 0x1000;
const CLINT_SIZE: u64 = 0x1_0000;
const CLINT_MSIP: u64 = 0;
const CLINT_MTIMECMP: u64 = 0x4000;
const CLINT_MTIME: u64 = 0xbff8;
const UART_RECEIVE: u64 = 0;
const UART_TRANSMIT: u64 = 0;
const UART_INTERRUPT_IDENTIFICATION: u64 = 2;
const UART_LINE_STATUS: u64 = 5;
const UART_LINE_STATUS_DATA_READY: u8 = 1;
const UART_LINE_STATUS_TRANSMIT_EMPTY: u8 = (1 << 5) | (1 << 6);
const MIN_RAM_BYTES: usize = 4096;
const MAX_RAM_BYTES: usize = 3 * 1024 * 1024 * 1024;
const MAX_CONSOLE_BYTES: usize = 16 * 1024 * 1024;
const MIN_LINUX_RAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_BOOTARGS_BYTES: usize = 4096;
const LINUX_FDT_MAX_BYTES: usize = 64 * 1024;
const GUEST_PAGE_SHIFT: u32 = 12;
const GUEST_PAGE_MASK: u64 = (1 << GUEST_PAGE_SHIFT) - 1;
const TRANSLATION_CACHE_ENTRIES: usize = 1024;
const HART_SCHEDULER_QUANTUM: u64 = 1024;

const MSTATUS_SIE: u64 = 1 << 1;
const MSTATUS_MIE: u64 = 1 << 3;
const MSTATUS_SPIE: u64 = 1 << 5;
const MSTATUS_MPIE: u64 = 1 << 7;
const MSTATUS_SPP: u64 = 1 << 8;
const MSTATUS_MPP_SHIFT: u32 = 11;
const MSTATUS_MPP: u64 = 0b11 << MSTATUS_MPP_SHIFT;
const MSTATUS_FS_SHIFT: u32 = 13;
const MSTATUS_FS: u64 = 0b11 << MSTATUS_FS_SHIFT;
const MSTATUS_FS_OFF: u64 = 0;
#[cfg(test)]
const MSTATUS_FS_INITIAL: u64 = 0b01 << MSTATUS_FS_SHIFT;
const MSTATUS_FS_DIRTY: u64 = 0b11 << MSTATUS_FS_SHIFT;
const MSTATUS_MPRV: u64 = 1 << 17;
const MSTATUS_SUM: u64 = 1 << 18;
const MSTATUS_MXR: u64 = 1 << 19;
const MSTATUS_UXL_RV64: u64 = 0b10 << 32;
const MSTATUS_SXL_RV64: u64 = 0b10 << 34;
const MSTATUS_SD: u64 = 1 << 63;
const MSTATUS_WRITABLE: u64 = MSTATUS_SIE
    | MSTATUS_MIE
    | MSTATUS_SPIE
    | MSTATUS_MPIE
    | MSTATUS_SPP
    | MSTATUS_MPP
    | MSTATUS_FS
    | MSTATUS_MPRV
    | MSTATUS_SUM
    | MSTATUS_MXR;
const SSTATUS_VISIBLE: u64 = MSTATUS_SIE
    | MSTATUS_SPIE
    | MSTATUS_SPP
    | MSTATUS_FS
    | MSTATUS_SUM
    | MSTATUS_MXR
    | MSTATUS_UXL_RV64
    | MSTATUS_SD;
const SSTATUS_WRITABLE: u64 =
    MSTATUS_SIE | MSTATUS_SPIE | MSTATUS_SPP | MSTATUS_FS | MSTATUS_SUM | MSTATUS_MXR;
const MISA_RV64_IMAFDCSU: u64 = (0b10 << 62)
    | (1 << 0)
    | (1 << 2)
    | (1 << 3)
    | (1 << 5)
    | (1 << 8)
    | (1 << 12)
    | (1 << 18)
    | (1 << 20);

const FFLAGS_NX: u8 = 1 << 0;
const FFLAGS_UF: u8 = 1 << 1;
const FFLAGS_OF: u8 = 1 << 2;
const FFLAGS_DZ: u8 = 1 << 3;
const FFLAGS_NV: u8 = 1 << 4;
const FFLAGS_MASK: u8 = 0x1f;
const FRM_SHIFT: u32 = 5;
const FRM_MASK: u8 = 0b111 << FRM_SHIFT;
const FCSR_MASK: u8 = FRM_MASK | FFLAGS_MASK;
const CANONICAL_NAN_F32: u32 = 0x7fc0_0000;
const CANONICAL_NAN_F64: u64 = 0x7ff8_0000_0000_0000;
const NAN_BOX_F32: u64 = 0xffff_ffff_0000_0000;
const MEDELEG_SUPPORTED: u64 = (1 << 0)
    | (1 << 1)
    | (1 << 2)
    | (1 << 3)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 7)
    | (1 << 8)
    | (1 << 9)
    | (1 << 12)
    | (1 << 13)
    | (1 << 15);

const SATP_MODE_SHIFT: u32 = 60;
const SATP_MODE_BARE: u64 = 0;
const SATP_MODE_SV39: u64 = 8;
const SATP_ASID_MASK: u64 = 0xffff << 44;
const SATP_PPN_MASK: u64 = (1 << 44) - 1;

const PTE_VALID: u64 = 1 << 0;
const PTE_READ: u64 = 1 << 1;
const PTE_WRITE: u64 = 1 << 2;
const PTE_EXECUTE: u64 = 1 << 3;
const PTE_USER: u64 = 1 << 4;
const PTE_ACCESSED: u64 = 1 << 6;
const PTE_DIRTY: u64 = 1 << 7;
const PTE_PPN_MASK: u64 = (1 << 44) - 1;
const PTE_RESERVED_MASK: u64 = !((PTE_PPN_MASK << 10) | 0x3ff);

const CAUSE_ECALL_FROM_USER: u64 = 8;
const CAUSE_ECALL_FROM_SUPERVISOR: u64 = 9;
const CAUSE_ECALL_FROM_MACHINE: u64 = 11;
const CAUSE_INSTRUCTION_PAGE_FAULT: u64 = 12;
const CAUSE_LOAD_PAGE_FAULT: u64 = 13;
const CAUSE_STORE_PAGE_FAULT: u64 = 15;

const INTERRUPT_SUPERVISOR_SOFTWARE: u64 = 1;
const INTERRUPT_MACHINE_SOFTWARE: u64 = 3;
const INTERRUPT_SUPERVISOR_TIMER: u64 = 5;
const INTERRUPT_MACHINE_TIMER: u64 = 7;
const INTERRUPT_SUPERVISOR_EXTERNAL: u64 = 9;
const INTERRUPT_MACHINE_EXTERNAL: u64 = 11;
const INTERRUPT_CAUSE_BIT: u64 = 1 << 63;
const MIP_SSIP: u64 = 1 << INTERRUPT_SUPERVISOR_SOFTWARE;
const MIP_MSIP: u64 = 1 << INTERRUPT_MACHINE_SOFTWARE;
const MIP_STIP: u64 = 1 << INTERRUPT_SUPERVISOR_TIMER;
const MIP_MTIP: u64 = 1 << INTERRUPT_MACHINE_TIMER;
const MIP_SEIP: u64 = 1 << INTERRUPT_SUPERVISOR_EXTERNAL;
const MIP_MEIP: u64 = 1 << INTERRUPT_MACHINE_EXTERNAL;
const INTERRUPT_SUPPORTED: u64 = MIP_SSIP | MIP_MSIP | MIP_STIP | MIP_MTIP | MIP_SEIP | MIP_MEIP;
const MIDELEG_SUPPORTED: u64 = MIP_SSIP | MIP_STIP | MIP_SEIP;

const SBI_EXT_BASE: u64 = 0x10;
const SBI_EXT_TIME: u64 = 0x5449_4d45;
const SBI_EXT_IPI: u64 = 0x0073_5049;
const SBI_EXT_RFENCE: u64 = 0x5246_4e43;
const SBI_EXT_HSM: u64 = 0x0048_534d;
const SBI_EXT_DBCN: u64 = 0x4442_434e;
const SBI_EXT_SRST: u64 = 0x5352_5354;
/// Private AOS 9P transport in the SBI experimental extension range.
///
/// The low 24 bits spell `AOS`; this is not a vendor extension or an assigned
/// standard extension ID. The interface is versioned with [`MACHINE_MODEL`].
pub const SBI_EXT_AOS_9P: u64 = 0x0841_4f53;
/// Maximum complete 9P message accepted in either transport direction.
pub const MAX_9P_MESSAGE_BYTES: usize = 64 * 1024;
const MIN_9P_MESSAGE_BYTES: usize = 7;
const SBI_SPEC_VERSION_3_0: u64 = 3 << 24;
const SBI_AOS_PRIVATE_IMPL_ID: u64 = 0x414f_5300;
const SBI_SUCCESS: u64 = 0;
const SBI_ERR_FAILED: u64 = (-1_i64) as u64;
const SBI_ERR_NOT_SUPPORTED: u64 = (-2_i64) as u64;
const SBI_ERR_INVALID_PARAM: u64 = (-3_i64) as u64;
const SBI_ERR_DENIED: u64 = (-4_i64) as u64;
const SBI_ERR_INVALID_ADDRESS: u64 = (-5_i64) as u64;
const SBI_ERR_ALREADY_AVAILABLE: u64 = (-6_i64) as u64;
const SBI_HART_STATE_STARTED: u64 = 0;
const SBI_HART_STATE_STOPPED: u64 = 1;

/// Explicit resource admission for one virtual machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MachineConfig {
    /// Contiguous guest RAM. It must be page-aligned and remains within the outer
    /// capsule's own Wasm memory limit.
    pub ram_bytes: usize,
    /// Maximum serial output retained for the current machine execution.
    pub max_console_bytes: usize,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            ram_bytes: 64 * 1024,
            max_console_bytes: 64 * 1024,
        }
    }
}

/// Machine construction or image-admission failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MachineError {
    /// RAM is too small, too large, or not aligned to a 4 KiB guest page.
    InvalidRamBytes(usize),
    /// The admitted guest RAM could not be allocated without aborting the worker.
    RamAllocationDenied(usize),
    /// The retained serial-output limit exceeds the hard machine cap.
    InvalidConsoleBytes(usize),
    /// The guest hart count is zero or exceeds the machine-profile maximum.
    InvalidHartCount(usize),
    /// A program image is empty or does not fit in admitted guest RAM.
    InvalidProgramBytes { image: usize, ram: usize },
    /// The admitted RAM cannot contain the kernel, initramfs, and generated FDT
    /// at their versioned machine-profile addresses.
    InvalidLinuxImages {
        kernel: usize,
        initramfs: usize,
        ram: usize,
    },
    /// Linux boot arguments exceed the deterministic FDT admission limit.
    InvalidBootArgsBytes(usize),
}

impl fmt::Display for MachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRamBytes(bytes) => write!(
                f,
                "guest RAM must be 4 KiB aligned and between {MIN_RAM_BYTES} and {MAX_RAM_BYTES} bytes, got {bytes}"
            ),
            Self::RamAllocationDenied(bytes) => {
                write!(f, "guest RAM allocation of {bytes} bytes was denied")
            }
            Self::InvalidConsoleBytes(bytes) => write!(
                f,
                "console limit must not exceed {MAX_CONSOLE_BYTES} bytes, got {bytes}"
            ),
            Self::InvalidHartCount(count) => write!(
                f,
                "guest hart count must be between 1 and {MAX_HARTS}, got {count}"
            ),
            Self::InvalidProgramBytes { image, ram } => {
                write!(
                    f,
                    "guest image is {image} bytes but admitted RAM is {ram} bytes"
                )
            }
            Self::InvalidLinuxImages {
                kernel,
                initramfs,
                ram,
            } => write!(
                f,
                "Linux kernel ({kernel} bytes), initramfs ({initramfs} bytes), and FDT do not fit in {ram} bytes of admitted RAM"
            ),
            Self::InvalidBootArgsBytes(bytes) => write!(
                f,
                "Linux boot arguments must be at most {MAX_BOOTARGS_BYTES} bytes, got {bytes}"
            ),
        }
    }
}

impl std::error::Error for MachineError {}

/// Rejected exact-hart scheduling request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidHartId {
    hart_id: usize,
    hart_count: usize,
}

impl InvalidHartId {
    /// Rejected zero-based hart identity.
    #[must_use]
    pub const fn hart_id(self) -> usize {
        self.hart_id
    }

    /// Admitted number of harts in the machine.
    #[must_use]
    pub const fn hart_count(self) -> usize {
        self.hart_count
    }
}

impl fmt::Display for InvalidHartId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "guest hart {} is outside the admitted topology of {} harts",
            self.hart_id, self.hart_count
        )
    }
}

impl std::error::Error for InvalidHartId {}

/// Reason a machine cannot become a principal-independent prewarm checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointError {
    /// A halted or trapped machine cannot become a boot template.
    NotRunnable,
    /// The machine must be stopped on an uncompleted, typed host request.
    NoPendingHostRequest,
    /// Principal or invocation input must never enter a reusable checkpoint.
    PendingConsoleInput { bytes: usize },
    /// Console bytes must be drained into the checkpoint build receipt first.
    UndrainedConsoleOutput { bytes: usize },
    /// Cross-hart control mailboxes must be acknowledged at an epoch barrier.
    PendingHartControl { hart_id: usize },
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunnable => write!(f, "only a runnable machine can be checkpointed"),
            Self::NoPendingHostRequest => {
                write!(f, "prewarm checkpoint requires a pending host request")
            }
            Self::PendingConsoleInput { bytes } => write!(
                f,
                "prewarm checkpoint contains {bytes} bytes of principal console input"
            ),
            Self::UndrainedConsoleOutput { bytes } => write!(
                f,
                "prewarm checkpoint contains {bytes} bytes of undrained console output"
            ),
            Self::PendingHartControl { hart_id } => write!(
                f,
                "prewarm checkpoint contains unacknowledged control for hart {hart_id}"
            ),
        }
    }
}

impl std::error::Error for CheckpointError {}

/// Exact admitted physical layout for one Linux boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinuxBootLayout {
    /// Raw kernel `Image` entry and load address.
    pub kernel_start: u64,
    /// First byte after the kernel image.
    pub kernel_end: u64,
    /// Initramfs start, absent when no initramfs was supplied.
    pub initrd_start: Option<u64>,
    /// First byte after the initramfs.
    pub initrd_end: Option<u64>,
    /// Generated flattened device-tree address.
    pub fdt_start: u64,
    /// Generated flattened device-tree byte length.
    pub fdt_bytes: usize,
}

/// RISC-V privilege level retained as part of guest architectural state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Privilege {
    /// Unprivileged application execution.
    User = 0,
    /// Linux kernel execution.
    Supervisor = 1,
    /// Firmware and reset execution.
    Machine = 3,
}

impl Privilege {
    const fn from_mpp(value: u64) -> Option<Self> {
        match (value & MSTATUS_MPP) >> MSTATUS_MPP_SHIFT {
            0 => Some(Self::User),
            1 => Some(Self::Supervisor),
            3 => Some(Self::Machine),
            _ => None,
        }
    }
}

/// Implemented control and status registers, named rather than exposed as raw
/// array offsets to machine users.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Csr {
    /// Accrued floating-point exception flags.
    Fflags = 0x001,
    /// Dynamic floating-point rounding mode.
    Frm = 0x002,
    /// Combined floating-point control and status register.
    Fcsr = 0x003,
    /// Supervisor status view of `mstatus`.
    Sstatus = 0x100,
    /// Supervisor interrupt-enable view.
    Sie = 0x104,
    /// Supervisor trap vector.
    Stvec = 0x105,
    /// Supervisor permission for user counter reads.
    Scounteren = 0x106,
    /// Supervisor scratch register.
    Sscratch = 0x140,
    /// Supervisor exception program counter.
    Sepc = 0x141,
    /// Supervisor trap cause.
    Scause = 0x142,
    /// Supervisor trap value.
    Stval = 0x143,
    /// Supervisor interrupt-pending view.
    Sip = 0x144,
    /// Supervisor address translation and protection register. The machine
    /// admits Bare and Sv39 modes.
    Satp = 0x180,
    /// Machine status.
    Mstatus = 0x300,
    /// Machine ISA report.
    Misa = 0x301,
    /// Machine exception delegation.
    Medeleg = 0x302,
    /// Machine interrupt delegation.
    Mideleg = 0x303,
    /// Machine interrupt enable.
    Mie = 0x304,
    /// Machine trap vector.
    Mtvec = 0x305,
    /// Machine permission for lower-privilege counter reads.
    Mcounteren = 0x306,
    /// Machine scratch register.
    Mscratch = 0x340,
    /// Machine exception program counter.
    Mepc = 0x341,
    /// Machine trap cause.
    Mcause = 0x342,
    /// Machine trap value.
    Mtval = 0x343,
    /// Machine interrupt pending.
    Mip = 0x344,
    /// Hardware-thread identifier. The first machine profile has one hart.
    Mhartid = 0xf14,
    /// Read-only cycle counter view.
    Cycle = 0xc00,
    /// Read-only deterministic time counter view.
    Time = 0xc01,
    /// Read-only retired-instruction counter view.
    Instret = 0xc02,
    /// Machine cycle counter.
    Mcycle = 0xb00,
    /// Machine retired-instruction counter.
    Minstret = 0xb02,
}

impl Csr {
    const fn from_address(address: u16) -> Option<Self> {
        Some(match address {
            0x001 => Self::Fflags,
            0x002 => Self::Frm,
            0x003 => Self::Fcsr,
            0x100 => Self::Sstatus,
            0x104 => Self::Sie,
            0x105 => Self::Stvec,
            0x106 => Self::Scounteren,
            0x140 => Self::Sscratch,
            0x141 => Self::Sepc,
            0x142 => Self::Scause,
            0x143 => Self::Stval,
            0x144 => Self::Sip,
            0x180 => Self::Satp,
            0x300 => Self::Mstatus,
            0x301 => Self::Misa,
            0x302 => Self::Medeleg,
            0x303 => Self::Mideleg,
            0x304 => Self::Mie,
            0x305 => Self::Mtvec,
            0x306 => Self::Mcounteren,
            0x340 => Self::Mscratch,
            0x341 => Self::Mepc,
            0x342 => Self::Mcause,
            0x343 => Self::Mtval,
            0x344 => Self::Mip,
            0xb00 => Self::Mcycle,
            0xb02 => Self::Minstret,
            0xc00 => Self::Cycle,
            0xc01 => Self::Time,
            0xc02 => Self::Instret,
            0xf14 => Self::Mhartid,
            _ => return None,
        })
    }

    const fn address(self) -> u16 {
        self as u16
    }
}

/// Stable machine fault descriptor. Architectural faults are consumed by trap
/// entry; resource faults remain visible to the outer Realm scheduler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MachineTrap {
    /// The next instruction address violates the RV64I four-byte alignment.
    InstructionAddressMisaligned { address: u64 },
    /// The next instruction is outside admitted RAM.
    InstructionAccessFault { address: u64 },
    /// The instruction is not implemented by this machine profile.
    IllegalInstruction { pc: u64, instruction: u32 },
    /// A load address is not naturally aligned for its width.
    LoadAddressMisaligned { address: u64, bytes: u8 },
    /// A load address is outside RAM and admitted MMIO.
    LoadAccessFault { address: u64, bytes: u8 },
    /// A store address is not naturally aligned for its width.
    StoreAddressMisaligned { address: u64, bytes: u8 },
    /// A store address is outside RAM and admitted MMIO.
    StoreAccessFault { address: u64, bytes: u8 },
    /// Sv39 could not translate or authorize an instruction fetch.
    InstructionPageFault { address: u64 },
    /// Sv39 could not translate or authorize a load.
    LoadPageFault { address: u64 },
    /// Sv39 could not translate or authorize a store.
    StorePageFault { address: u64 },
    /// Guest execution reached an `ebreak` instruction.
    Breakpoint { pc: u64 },
    /// Serial output exceeded its admitted retained-byte ceiling.
    ConsoleLimit { limit: usize },
}

impl fmt::Display for MachineTrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstructionAddressMisaligned { address } => {
                write!(f, "instruction address {address:#x} is misaligned")
            }
            Self::InstructionAccessFault { address } => {
                write!(f, "instruction access fault at {address:#x}")
            }
            Self::IllegalInstruction { pc, instruction } => {
                write!(f, "illegal instruction {instruction:#010x} at {pc:#x}")
            }
            Self::LoadAddressMisaligned { address, bytes } => {
                write!(f, "{bytes}-byte load address {address:#x} is misaligned")
            }
            Self::LoadAccessFault { address, bytes } => {
                write!(f, "{bytes}-byte load access fault at {address:#x}")
            }
            Self::StoreAddressMisaligned { address, bytes } => {
                write!(f, "{bytes}-byte store address {address:#x} is misaligned")
            }
            Self::StoreAccessFault { address, bytes } => {
                write!(f, "{bytes}-byte store access fault at {address:#x}")
            }
            Self::InstructionPageFault { address } => {
                write!(f, "instruction page fault at {address:#x}")
            }
            Self::LoadPageFault { address } => write!(f, "load page fault at {address:#x}"),
            Self::StorePageFault { address } => write!(f, "store page fault at {address:#x}"),
            Self::Breakpoint { pc } => write!(f, "breakpoint at {pc:#x}"),
            Self::ConsoleLimit { limit } => {
                write!(f, "console output exceeded {limit} bytes")
            }
        }
    }
}

impl std::error::Error for MachineTrap {}

/// Terminal value written by firmware or a test guest to the standard finisher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HaltStatus {
    /// Whether the standard pass value was written.
    pub passed: bool,
    /// Guest-provided failure code, or zero for success.
    pub code: u32,
}

/// Machine-local identity for one host-mediated request.
///
/// IDs remain monotonic across guest image reloads so a late completion cannot
/// accidentally complete the first request made by a replacement guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostRequestId(u64);

impl HostRequestId {
    /// Raw identity for logs and outer scheduler correlation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One complete 9P request copied out of admitted guest RAM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan9Request {
    /// Unique identity required when completing or failing this request.
    pub id: HostRequestId,
    /// Guest-selected mount channel whose FID and tag space is isolated from
    /// every other 9P mount in this machine.
    pub channel: u32,
    /// Complete size-prefixed 9P request message.
    pub message: Vec<u8>,
    /// Maximum complete response message admitted by the guest buffer.
    pub max_response_bytes: usize,
}

/// Transport-level host failure returned to the guest SBI caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRequestFailure {
    /// The admitted host operation failed without a more specific boundary result.
    Failed,
    /// The outer Realm denied the requested host operation.
    Denied,
}

/// Rejected attempt to complete a pending host request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostCompletionError {
    /// The machine has no request awaiting a host response.
    NoPendingRequest,
    /// The response belongs to a different request.
    RequestIdMismatch {
        /// Identity currently awaited by the machine.
        expected: HostRequestId,
        /// Identity supplied by the host.
        actual: HostRequestId,
    },
    /// A successful 9P response is smaller than its mandatory message header.
    InvalidResponseBytes(usize),
    /// The response exceeds the capacity admitted by the guest request.
    ResponseTooLarge {
        /// Supplied response byte length.
        response: usize,
        /// Admitted guest response capacity.
        capacity: usize,
    },
    /// The previously admitted response range is no longer writable guest RAM.
    ResponseAddressUnavailable,
    /// The hart that issued the request is absent from the admitted topology.
    RequestHartUnavailable { hart_id: usize },
}

impl fmt::Display for HostCompletionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPendingRequest => write!(f, "machine has no pending host request"),
            Self::RequestIdMismatch { expected, actual } => write!(
                f,
                "host response request {} does not match pending request {}",
                actual.get(),
                expected.get()
            ),
            Self::InvalidResponseBytes(bytes) => {
                write!(f, "9P response must contain at least 7 bytes, got {bytes}")
            }
            Self::ResponseTooLarge { response, capacity } => write!(
                f,
                "9P response is {response} bytes but guest admitted {capacity} bytes"
            ),
            Self::ResponseAddressUnavailable => {
                write!(f, "pending 9P response buffer is no longer admitted RAM")
            }
            Self::RequestHartUnavailable { hart_id } => {
                write!(f, "pending 9P request hart {hart_id} is unavailable")
            }
        }
    }
}

impl std::error::Error for HostCompletionError {}

/// Result of one bounded scheduling slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SliceOutcome {
    /// The instruction budget ended while the guest remained runnable.
    Yielded,
    /// The guest wrote a terminal value to the standard finisher.
    Halted(HaltStatus),
    /// The guest is paused until the outer Realm completes this request.
    HostRequest(Plan9Request),
    /// The guest crossed a non-architectural Realm resource boundary.
    Trapped(MachineTrap),
}

/// Exact accounting and serial bytes produced by one scheduling slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliceReport {
    /// Result at the end of this slice.
    pub outcome: SliceOutcome,
    /// Successfully interpreted instruction steps charged to this scheduling
    /// slice. Architecturally non-retiring traps such as `ecall` still consume
    /// one bounded step.
    pub steps_executed: u64,
    /// Total charged instruction steps since the last image load.
    pub total_steps_executed: u64,
    /// Instructions retired during this slice.
    pub instructions_retired: u64,
    /// Total instructions retired since the last image load.
    pub total_instructions_retired: u64,
    /// Serial output produced since the previous slice report.
    pub console: Vec<u8>,
}

/// Cumulative hot-path measurements since the last image load.
///
/// These counters describe where the reference interpreter spends semantic
/// work without changing its instruction-fuel contract. They are diagnostic,
/// not guest-visible architectural counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MachineMetrics {
    /// Instruction fetches attempted after interrupt and alignment checks.
    pub instruction_fetches: u64,
    /// Virtual-to-physical translations requested for instruction fetches.
    pub instruction_translations: u64,
    /// Virtual-to-physical translations requested for guest loads.
    pub load_translations: u64,
    /// Virtual-to-physical translations requested for guest stores.
    pub store_translations: u64,
    /// Sv39 page-table walks started after privilege and mode checks.
    pub sv39_walks: u64,
    /// Page-table entries read while walking Sv39 tables.
    pub page_table_entries_read: u64,
    /// Page-table entries written to establish accessed or dirty bits.
    pub page_table_entries_written: u64,
    /// Sv39 translations served by the fixed-size machine-local cache.
    pub translation_cache_hits: u64,
    /// Sv39 translations that required a page-table walk.
    pub translation_cache_misses: u64,
    /// Conservative whole-cache invalidations caused by architectural fences
    /// or accepted address-space changes.
    pub translation_cache_flushes: u64,
}

impl MachineMetrics {
    /// Total virtual-to-physical translation requests of every access type.
    #[must_use]
    pub const fn translations(self) -> u64 {
        self.instruction_translations
            .saturating_add(self.load_translations)
            .saturating_add(self.store_translations)
    }
}

#[derive(Clone, Debug)]
struct Cpu {
    registers: [u64; 32],
    floating_registers: [u64; 32],
    pc: u64,
    privilege: Privilege,
}

impl Cpu {
    fn new() -> Self {
        Self {
            registers: [0; 32],
            floating_registers: [0; 32],
            pc: DRAM_BASE,
            privilege: Privilege::Machine,
        }
    }

    fn reset(&mut self) {
        self.registers.fill(0);
        self.floating_registers.fill(0);
        self.pc = DRAM_BASE;
        self.privilege = Privilege::Machine;
    }

    fn read(&self, register: usize) -> u64 {
        self.registers[register]
    }

    fn write(&mut self, register: usize, value: u64) {
        if register != 0 {
            self.registers[register] = value;
        }
    }

    fn read_float32(&self, register: usize) -> u32 {
        let value = self.floating_registers[register];
        if value >> 32 == u64::from(u32::MAX) {
            value as u32
        } else {
            CANONICAL_NAN_F32
        }
    }

    fn write_float32(&mut self, register: usize, value: u32) {
        self.floating_registers[register] = NAN_BOX_F32 | u64::from(value);
    }

    fn read_float64(&self, register: usize) -> u64 {
        self.floating_registers[register]
    }

    fn write_float64(&mut self, register: usize, value: u64) {
        self.floating_registers[register] = value;
    }
}

#[derive(Clone, Debug, Default)]
struct CsrFile {
    fcsr: u8,
    mstatus: u64,
    medeleg: u64,
    mideleg: u64,
    mie: u64,
    mip: u64,
    mcounteren: u64,
    scounteren: u64,
    satp: u64,
    mtvec: u64,
    mscratch: u64,
    mepc: u64,
    mcause: u64,
    mtval: u64,
    stvec: u64,
    sscratch: u64,
    sepc: u64,
    scause: u64,
    stval: u64,
}

impl CsrFile {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn validate_access(&self, address: u16, privilege: Privilege, write: bool) -> Option<Csr> {
        let csr = Csr::from_address(address)?;
        let required = match (address >> 8) & 0b11 {
            0 => Privilege::User,
            1 => Privilege::Supervisor,
            3 => Privilege::Machine,
            _ => return None,
        };
        if privilege < required || write && ((address >> 10) & 0b11) == 0b11 {
            return None;
        }
        if matches!(csr, Csr::Fflags | Csr::Frm | Csr::Fcsr) && !self.floating_enabled() {
            return None;
        }
        if matches!(csr, Csr::Cycle | Csr::Time | Csr::Instret) {
            let bit = 1 << (address - Csr::Cycle.address());
            if privilege < Privilege::Machine && self.mcounteren & bit == 0
                || privilege == Privilege::User && self.scounteren & bit == 0
            {
                return None;
            }
        }
        Some(csr)
    }

    const fn read(&self, csr: Csr) -> u64 {
        match csr {
            Csr::Fflags => (self.fcsr & FFLAGS_MASK) as u64,
            Csr::Frm => ((self.fcsr & FRM_MASK) >> FRM_SHIFT) as u64,
            Csr::Fcsr => self.fcsr as u64,
            Csr::Sstatus => self.status() & SSTATUS_VISIBLE,
            Csr::Sie => self.mie & self.mideleg,
            Csr::Sip => self.mip & self.mideleg,
            Csr::Mideleg => self.mideleg,
            Csr::Mie => self.mie,
            Csr::Mip => self.mip,
            Csr::Stvec => self.stvec,
            Csr::Scounteren => self.scounteren,
            Csr::Sscratch => self.sscratch,
            Csr::Sepc => self.sepc & !0b1,
            Csr::Scause => self.scause,
            Csr::Stval => self.stval,
            Csr::Satp => self.satp,
            Csr::Mstatus => self.status() | MSTATUS_UXL_RV64 | MSTATUS_SXL_RV64,
            Csr::Misa => MISA_RV64_IMAFDCSU,
            Csr::Medeleg => self.medeleg,
            Csr::Mtvec => self.mtvec,
            Csr::Mcounteren => self.mcounteren,
            Csr::Mscratch => self.mscratch,
            Csr::Mepc => self.mepc & !0b1,
            Csr::Mcause => self.mcause,
            Csr::Mtval => self.mtval,
            Csr::Mhartid => 0,
            Csr::Cycle | Csr::Time | Csr::Instret | Csr::Mcycle | Csr::Minstret => 0,
        }
    }

    fn write(&mut self, csr: Csr, value: u64) {
        match csr {
            Csr::Fflags => {
                self.fcsr = (self.fcsr & !FFLAGS_MASK) | (value as u8 & FFLAGS_MASK);
                self.mark_float_dirty();
            }
            Csr::Frm => {
                self.fcsr = (self.fcsr & !FRM_MASK) | (((value as u8) << FRM_SHIFT) & FRM_MASK);
                self.mark_float_dirty();
            }
            Csr::Fcsr => {
                self.fcsr = value as u8 & FCSR_MASK;
                self.mark_float_dirty();
            }
            Csr::Sstatus => {
                self.mstatus = (self.mstatus & !SSTATUS_WRITABLE) | (value & SSTATUS_WRITABLE);
            }
            Csr::Sie => self.mie = (self.mie & !self.mideleg) | (value & self.mideleg),
            Csr::Sip => {
                let writable = MIP_SSIP & self.mideleg;
                self.mip = (self.mip & !writable) | (value & writable);
            }
            Csr::Mideleg => self.mideleg = value & MIDELEG_SUPPORTED,
            Csr::Mie => self.mie = value & INTERRUPT_SUPPORTED,
            Csr::Mip => {
                let writable = MIP_SSIP | MIP_STIP;
                self.mip = (self.mip & !writable) | (value & writable);
            }
            Csr::Stvec => self.stvec = legal_trap_vector(value),
            Csr::Scounteren => self.scounteren = value & 0b111,
            Csr::Sscratch => self.sscratch = value,
            Csr::Sepc => self.sepc = value & !0b1,
            Csr::Scause => self.scause = value,
            Csr::Stval => self.stval = value,
            Csr::Satp => {
                let mode = value >> SATP_MODE_SHIFT;
                if matches!(mode, SATP_MODE_BARE | SATP_MODE_SV39) {
                    self.satp = (mode << SATP_MODE_SHIFT)
                        | (value & SATP_ASID_MASK)
                        | (value & SATP_PPN_MASK);
                }
            }
            Csr::Mstatus => {
                let mut admitted = value & MSTATUS_WRITABLE;
                if admitted & MSTATUS_MPP == 0b10 << MSTATUS_MPP_SHIFT {
                    admitted &= !MSTATUS_MPP;
                }
                self.mstatus = (self.mstatus & !MSTATUS_WRITABLE) | admitted;
            }
            Csr::Misa => {}
            Csr::Medeleg => self.medeleg = value & MEDELEG_SUPPORTED,
            Csr::Mtvec => self.mtvec = legal_trap_vector(value),
            Csr::Mcounteren => self.mcounteren = value & 0b111,
            Csr::Mscratch => self.mscratch = value,
            Csr::Mepc => self.mepc = value & !0b1,
            Csr::Mcause => self.mcause = value,
            Csr::Mtval => self.mtval = value,
            Csr::Mhartid => {}
            Csr::Cycle | Csr::Time | Csr::Instret => {}
            Csr::Mcycle | Csr::Minstret => {}
        }
    }

    const fn status(&self) -> u64 {
        if self.mstatus & MSTATUS_FS == MSTATUS_FS_DIRTY {
            self.mstatus | MSTATUS_SD
        } else {
            self.mstatus
        }
    }

    const fn floating_enabled(&self) -> bool {
        self.mstatus & MSTATUS_FS != MSTATUS_FS_OFF
    }

    fn mark_float_dirty(&mut self) {
        self.mstatus = (self.mstatus & !MSTATUS_FS) | MSTATUS_FS_DIRTY;
    }

    fn accrue_float_flags(&mut self, flags: u8) {
        self.fcsr |= flags & FFLAGS_MASK;
        self.mark_float_dirty();
    }
}

const fn legal_trap_vector(value: u64) -> u64 {
    match value & 0b11 {
        0 | 1 => value,
        _ => value & !0b11,
    }
}

#[derive(Clone, Debug)]
enum RunState {
    Runnable,
    Halted(HaltStatus),
    Trapped(MachineTrap),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HartLifecycle {
    Started,
    Stopped,
}

const HART_CONTROL_STOPPED: u8 = 0;
const HART_CONTROL_START_CLAIMED: u8 = 1;
const HART_CONTROL_START_PENDING: u8 = 2;
const HART_CONTROL_STARTED: u8 = 3;

#[derive(Debug)]
struct HartControl {
    lifecycle: AtomicU8,
    start_address: AtomicU64,
    start_opaque: AtomicU64,
    start_generation: AtomicU64,
    start_acknowledged: AtomicU64,
    pending_interrupts: AtomicU64,
    fence_generation: AtomicU64,
    fence_acknowledged: AtomicU64,
}

impl Clone for HartControl {
    fn clone(&self) -> Self {
        Self {
            lifecycle: AtomicU8::new(self.lifecycle.load(Ordering::Acquire)),
            start_address: AtomicU64::new(self.start_address.load(Ordering::Acquire)),
            start_opaque: AtomicU64::new(self.start_opaque.load(Ordering::Acquire)),
            start_generation: AtomicU64::new(self.start_generation.load(Ordering::Acquire)),
            start_acknowledged: AtomicU64::new(self.start_acknowledged.load(Ordering::Acquire)),
            pending_interrupts: AtomicU64::new(self.pending_interrupts.load(Ordering::Acquire)),
            fence_generation: AtomicU64::new(self.fence_generation.load(Ordering::Acquire)),
            fence_acknowledged: AtomicU64::new(self.fence_acknowledged.load(Ordering::Acquire)),
        }
    }
}

impl HartControl {
    fn new(lifecycle: HartLifecycle) -> Self {
        Self {
            lifecycle: AtomicU8::new(match lifecycle {
                HartLifecycle::Started => HART_CONTROL_STARTED,
                HartLifecycle::Stopped => HART_CONTROL_STOPPED,
            }),
            start_address: AtomicU64::new(0),
            start_opaque: AtomicU64::new(0),
            start_generation: AtomicU64::new(0),
            start_acknowledged: AtomicU64::new(0),
            pending_interrupts: AtomicU64::new(0),
            fence_generation: AtomicU64::new(0),
            fence_acknowledged: AtomicU64::new(0),
        }
    }

    fn lifecycle(&self) -> HartLifecycle {
        match self.lifecycle.load(Ordering::Acquire) {
            HART_CONTROL_STARTED => HartLifecycle::Started,
            _ => HartLifecycle::Stopped,
        }
    }

    fn request_start(&self, address: u64, opaque: u64) -> Result<u64, ()> {
        self.lifecycle
            .compare_exchange(
                HART_CONTROL_STOPPED,
                HART_CONTROL_START_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| ())?;
        self.start_address.store(address, Ordering::Relaxed);
        self.start_opaque.store(opaque, Ordering::Relaxed);
        let generation = self.start_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.lifecycle
            .store(HART_CONTROL_START_PENDING, Ordering::Release);
        Ok(generation)
    }

    fn pending_start(&self) -> Option<(u64, u64, u64)> {
        (self.lifecycle.load(Ordering::Acquire) == HART_CONTROL_START_PENDING).then(|| {
            (
                self.start_address.load(Ordering::Relaxed),
                self.start_opaque.load(Ordering::Relaxed),
                self.start_generation.load(Ordering::Acquire),
            )
        })
    }

    fn acknowledge_start(&self, generation: u64) {
        self.start_acknowledged.store(generation, Ordering::Release);
        self.lifecycle
            .store(HART_CONTROL_STARTED, Ordering::Release);
    }

    fn stop(&self) {
        self.lifecycle
            .store(HART_CONTROL_STOPPED, Ordering::Release);
    }

    fn raise_interrupt(&self, interrupt: u64) {
        self.pending_interrupts
            .fetch_or(interrupt, Ordering::AcqRel);
    }

    fn take_interrupts(&self) -> u64 {
        self.pending_interrupts.swap(0, Ordering::AcqRel)
    }

    fn request_fence(&self) -> u64 {
        self.fence_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn pending_fence(&self) -> Option<u64> {
        let requested = self.fence_generation.load(Ordering::Acquire);
        let acknowledged = self.fence_acknowledged.load(Ordering::Acquire);
        (requested != acknowledged).then_some(requested)
    }

    fn acknowledge_fence(&self, generation: u64) {
        self.fence_acknowledged.store(generation, Ordering::Release);
    }

    fn is_quiescent(&self) -> bool {
        let lifecycle = self.lifecycle.load(Ordering::Acquire);
        !matches!(
            lifecycle,
            HART_CONTROL_START_CLAIMED | HART_CONTROL_START_PENDING
        ) && self.pending_interrupts.load(Ordering::Acquire) == 0
            && self.fence_generation.load(Ordering::Acquire)
                == self.fence_acknowledged.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct HartState {
    id: usize,
    lifecycle: HartLifecycle,
    cpu: Cpu,
    csrs: CsrFile,
    cycle: u64,
    instret: u64,
    reservation: Option<(u64, u8)>,
    #[cfg(target_arch = "wasm32")]
    reservation_token: Option<AtomicReservation>,
    mtimecmp: u64,
    msip: bool,
    translation_cache: TranslationCache,
}

impl HartState {
    fn new(id: usize, lifecycle: HartLifecycle) -> Self {
        Self {
            id,
            lifecycle,
            cpu: Cpu::new(),
            csrs: CsrFile::default(),
            cycle: 0,
            instret: 0,
            reservation: None,
            #[cfg(target_arch = "wasm32")]
            reservation_token: None,
            mtimecmp: u64::MAX,
            msip: false,
            translation_cache: TranslationCache::default(),
        }
    }
}

#[derive(Clone, Debug)]
struct PendingPlan9Request {
    request: Plan9Request,
    response_address: u64,
    hart_id: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct SbiFirmware {
    enabled: bool,
}

#[derive(Clone, Debug)]
struct GuestRam {
    #[cfg(all(not(target_arch = "wasm32"), not(test)))]
    bytes: Vec<u8>,
    #[cfg(test)]
    chunks: Vec<Option<Box<[u8]>>>,
    #[cfg(target_arch = "wasm32")]
    atomic: AtomicGuestRam,
    len: usize,
}

#[cfg(test)]
const RAM_CHUNK_BYTES: usize = 2 * 1024 * 1024;

impl GuestRam {
    fn zeroed(len: usize) -> Result<Self, MachineError> {
        #[cfg(all(not(target_arch = "wasm32"), not(test)))]
        {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(len)
                .map_err(|_| MachineError::RamAllocationDenied(len))?;
            bytes.resize(len, 0);
            Ok(Self { bytes, len })
        }
        #[cfg(test)]
        {
            let chunk_count = len.div_ceil(RAM_CHUNK_BYTES);
            let mut chunks = Vec::new();
            chunks
                .try_reserve_exact(chunk_count)
                .map_err(|_| MachineError::RamAllocationDenied(len))?;
            chunks.resize_with(chunk_count, || None);
            Ok(Self { chunks, len })
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(Self {
                atomic: AtomicGuestRam::zeroed(len)?,
                len,
            })
        }
    }

    const fn len(&self) -> usize {
        self.len
    }

    fn fill_zero(&mut self) {
        #[cfg(all(not(target_arch = "wasm32"), not(test)))]
        self.bytes.fill(0);
        #[cfg(test)]
        for chunk in &mut self.chunks {
            *chunk = None;
        }
        #[cfg(target_arch = "wasm32")]
        self.atomic.fill_zero();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn byte(&self, index: usize) -> Option<u8> {
        if index >= self.len {
            return None;
        }
        #[cfg(all(not(target_arch = "wasm32"), not(test)))]
        return self.bytes.get(index).copied();
        #[cfg(test)]
        {
            self.chunks.get(index / RAM_CHUNK_BYTES).map(|chunk| {
                chunk
                    .as_deref()
                    .map_or(0, |bytes| bytes[index % RAM_CHUNK_BYTES])
            })
        }
        #[cfg(target_arch = "wasm32")]
        self.atomic.byte(index)
    }

    fn read_value(&self, index: usize, bytes: u8) -> Option<u64> {
        let byte_count = usize::from(bytes);
        let end = index.checked_add(byte_count)?;
        if byte_count == 0 || byte_count > size_of::<u64>() || end > self.len {
            return None;
        }
        #[cfg(target_arch = "wasm32")]
        return self.atomic.read_value(index, bytes);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut value = 0_u64;
            for shift in 0..byte_count {
                value |= u64::from(self.byte(index + shift)?) << (shift * 8);
            }
            Some(value)
        }
    }

    fn write_value(&mut self, index: usize, value: u64, bytes: u8) -> bool {
        let byte_count = usize::from(bytes);
        let Some(end) = index.checked_add(byte_count) else {
            return false;
        };
        if byte_count == 0
            || byte_count > size_of::<u64>()
            || end > self.len
            || !index.is_multiple_of(byte_count)
        {
            return false;
        }
        #[cfg(target_arch = "wasm32")]
        return self.atomic.write_value(index, value, bytes);
        #[cfg(not(target_arch = "wasm32"))]
        {
            for shift in 0..byte_count {
                if !self.set_byte(index + shift, (value >> (shift * 8)) as u8) {
                    return false;
                }
            }
            true
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn load_reserved(&self, index: usize, bytes: u8) -> Option<(u64, AtomicReservation)> {
        self.atomic.load_reserved(index, bytes)
    }

    #[cfg(target_arch = "wasm32")]
    fn store_conditional(
        &self,
        reservation: AtomicReservation,
        index: usize,
        value: u64,
        bytes: u8,
    ) -> bool {
        self.atomic
            .store_conditional(reservation, index, value, bytes)
    }

    #[cfg(target_arch = "wasm32")]
    fn update_value(&self, index: usize, bytes: u8, value: impl FnOnce(u64) -> u64) -> Option<u64> {
        self.atomic.update_value(index, bytes, value)
    }

    #[cfg(test)]
    fn ensure_chunk(&mut self, index: usize) -> bool {
        let Some(slot) = self.chunks.get_mut(index) else {
            return false;
        };
        if slot.is_some() {
            return true;
        }
        let start = index * RAM_CHUNK_BYTES;
        let chunk_len = (self.len - start).min(RAM_CHUNK_BYTES);
        let mut chunk = Vec::new();
        if chunk.try_reserve_exact(chunk_len).is_err() {
            return false;
        }
        chunk.resize(chunk_len, 0);
        *slot = Some(chunk.into_boxed_slice());
        true
    }

    fn set_byte(&mut self, index: usize, value: u8) -> bool {
        if index >= self.len {
            return false;
        }
        #[cfg(all(not(target_arch = "wasm32"), not(test)))]
        {
            self.bytes.get_mut(index).is_some_and(|slot| {
                *slot = value;
                true
            })
        }
        #[cfg(test)]
        {
            if value != 0 && !self.ensure_chunk(index / RAM_CHUNK_BYTES) {
                return false;
            }
            if value == 0 && self.chunks[index / RAM_CHUNK_BYTES].is_none() {
                return true;
            }
            self.chunks
                .get_mut(index / RAM_CHUNK_BYTES)
                .and_then(Option::as_deref_mut)
                .and_then(|chunk| chunk.get_mut(index % RAM_CHUNK_BYTES))
                .is_some_and(|slot| {
                    *slot = value;
                    true
                })
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.atomic.set_byte(index, value)
        }
    }

    fn copy_from_slice(&mut self, start: usize, source: &[u8]) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            return self.atomic.copy_from_slice(start, source);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let Some(end) = start
            .checked_add(source.len())
            .filter(|end| *end <= self.len)
        else {
            return false;
        };
        #[cfg(all(not(target_arch = "wasm32"), not(test)))]
        {
            self.bytes[start..end].copy_from_slice(source);
        }
        #[cfg(test)]
        {
            let mut destination = start;
            let mut source_offset = 0;
            while destination < end {
                let chunk_index = destination / RAM_CHUNK_BYTES;
                let chunk_offset = destination % RAM_CHUNK_BYTES;
                let copy_len = (end - destination).min(RAM_CHUNK_BYTES - chunk_offset);
                let source_slice = &source[source_offset..source_offset + copy_len];
                if source_slice.iter().any(|byte| *byte != 0) {
                    if !self.ensure_chunk(chunk_index) {
                        return false;
                    }
                    self.chunks[chunk_index]
                        .as_deref_mut()
                        .expect("chunk admitted")[chunk_offset..chunk_offset + copy_len]
                        .copy_from_slice(source_slice);
                } else if let Some(chunk) = self.chunks[chunk_index].as_deref_mut() {
                    chunk[chunk_offset..chunk_offset + copy_len].fill(0);
                }
                destination += copy_len;
                source_offset += copy_len;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        true
    }

    fn copy_to_vec(&self, range: std::ops::Range<usize>) -> Option<Vec<u8>> {
        if range.start > range.end || range.end > self.len {
            return None;
        }
        #[cfg(all(not(target_arch = "wasm32"), not(test)))]
        return Some(self.bytes[range].to_vec());
        #[cfg(test)]
        {
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(range.len()).ok()?;
            bytes.resize(range.len(), 0);
            let mut source = range.start;
            let mut destination = 0;
            while source < range.end {
                let chunk_index = source / RAM_CHUNK_BYTES;
                let chunk_offset = source % RAM_CHUNK_BYTES;
                let copy_len = (range.end - source).min(RAM_CHUNK_BYTES - chunk_offset);
                if let Some(chunk) = self.chunks[chunk_index].as_deref() {
                    bytes[destination..destination + copy_len]
                        .copy_from_slice(&chunk[chunk_offset..chunk_offset + copy_len]);
                }
                source += copy_len;
                destination += copy_len;
            }
            Some(bytes)
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.atomic.copy_to_vec(range)
        }
    }

    fn slice(&self, range: std::ops::Range<usize>) -> Option<&[u8]> {
        if range.start > range.end || range.end > self.len {
            return None;
        }
        if range.is_empty() {
            return Some(&[]);
        }
        #[cfg(all(not(target_arch = "wasm32"), not(test)))]
        return self.bytes.get(range);
        #[cfg(test)]
        {
            let chunk_index = range.start / RAM_CHUNK_BYTES;
            let chunk_offset = range.start % RAM_CHUNK_BYTES;
            let length = range.len();
            self.chunks
                .get(chunk_index)
                .and_then(Option::as_deref)
                .and_then(|chunk| chunk.get(chunk_offset..chunk_offset.checked_add(length)?))
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    }

    fn write_page(&mut self, index: usize, page_bytes: usize, bytes: &[u8]) -> bool {
        if bytes.len() != page_bytes {
            return false;
        }
        index
            .checked_mul(page_bytes)
            .is_some_and(|start| self.copy_from_slice(start, bytes))
    }

    fn nonzero_pages(&self, page_bytes: usize) -> Vec<(usize, Vec<u8>)> {
        debug_assert!(page_bytes > 0);
        debug_assert_eq!(self.len % page_bytes, 0);
        #[cfg(all(not(target_arch = "wasm32"), not(test)))]
        return self
            .bytes
            .chunks_exact(page_bytes)
            .enumerate()
            .filter(|(_, page)| page.iter().any(|byte| *byte != 0))
            .map(|(index, page)| (index, page.to_vec()))
            .collect();
        #[cfg(test)]
        {
            let mut pages = Vec::new();
            for (chunk_index, chunk) in self.chunks.iter().enumerate() {
                let Some(chunk) = chunk.as_deref() else {
                    continue;
                };
                let first_page = chunk_index * RAM_CHUNK_BYTES / page_bytes;
                pages.extend(
                    chunk
                        .chunks_exact(page_bytes)
                        .enumerate()
                        .filter(|(_, page)| page.iter().any(|byte| *byte != 0))
                        .map(|(page_index, page)| (first_page + page_index, page.to_vec())),
                );
            }
            pages
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.atomic.nonzero_pages(page_bytes)
        }
    }
}

#[derive(Clone, Debug)]
struct Devices {
    ram: GuestRam,
    mtime: u64,
    console_input: VecDeque<u8>,
    console_output: Vec<u8>,
    max_console_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
struct StepEffect {
    halt: Option<HaltStatus>,
    retired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccessType {
    Instruction,
    Load,
    Store,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TranslationContext {
    satp: u64,
    permission_context: u64,
    privilege: Privilege,
    access: AccessType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TranslationCacheEntry {
    virtual_page: u64,
    physical_page: u64,
    context: TranslationContext,
}

#[derive(Clone, Debug)]
struct TranslationCache {
    entries: [Option<TranslationCacheEntry>; TRANSLATION_CACHE_ENTRIES],
}

impl Default for TranslationCache {
    fn default() -> Self {
        Self {
            entries: [None; TRANSLATION_CACHE_ENTRIES],
        }
    }
}

impl TranslationCache {
    fn lookup(&self, address: u64, context: TranslationContext) -> Option<u64> {
        let virtual_page = address >> GUEST_PAGE_SHIFT;
        let entry = self.entries[Self::index(virtual_page, context)]?;
        (entry.virtual_page == virtual_page && entry.context == context)
            .then_some(entry.physical_page | (address & GUEST_PAGE_MASK))
    }

    fn insert(&mut self, address: u64, physical: u64, context: TranslationContext) {
        let virtual_page = address >> GUEST_PAGE_SHIFT;
        self.entries[Self::index(virtual_page, context)] = Some(TranslationCacheEntry {
            virtual_page,
            physical_page: physical & !GUEST_PAGE_MASK,
            context,
        });
    }

    fn clear(&mut self) {
        self.entries.fill(None);
    }

    fn index(virtual_page: u64, context: TranslationContext) -> usize {
        let access = match context.access {
            AccessType::Instruction => 0_u64,
            AccessType::Load => 1,
            AccessType::Store => 2,
        };
        let mixed = virtual_page
            ^ context.satp.rotate_left(17)
            ^ context.permission_context.rotate_left(31)
            ^ ((context.privilege as u64) << 5)
            ^ (access << 3);
        mixed as usize & (TRANSLATION_CACHE_ENTRIES - 1)
    }
}

impl AccessType {
    const fn page_fault(self, address: u64) -> MachineTrap {
        match self {
            Self::Instruction => MachineTrap::InstructionPageFault { address },
            Self::Load => MachineTrap::LoadPageFault { address },
            Self::Store => MachineTrap::StorePageFault { address },
        }
    }

    const fn access_fault(self, address: u64, bytes: u8) -> MachineTrap {
        match self {
            Self::Instruction => MachineTrap::InstructionAccessFault { address },
            Self::Load => MachineTrap::LoadAccessFault { address, bytes },
            Self::Store => MachineTrap::StoreAccessFault { address, bytes },
        }
    }
}

impl StepEffect {
    const fn retired(halt: Option<HaltStatus>) -> Self {
        Self {
            halt,
            retired: true,
        }
    }

    const fn trapped_architecturally() -> Self {
        Self {
            halt: None,
            retired: false,
        }
    }
}

impl Devices {
    fn new(config: MachineConfig) -> Result<Self, MachineError> {
        Ok(Self {
            ram: GuestRam::zeroed(config.ram_bytes)?,
            mtime: 0,
            console_input: VecDeque::new(),
            console_output: Vec::new(),
            max_console_bytes: config.max_console_bytes,
        })
    }

    fn reset(&mut self) {
        self.ram.fill_zero();
        self.mtime = 0;
        self.console_input.clear();
        self.console_output.clear();
    }

    fn load_program(&mut self, program: &[u8]) -> bool {
        self.ram.copy_from_slice(0, program)
    }

    fn take_new_console(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.console_output)
    }

    fn read(&mut self, address: u64, bytes: u8) -> Result<u64, MachineTrap> {
        if let Some(offset) = address
            .checked_sub(UART_BASE)
            .filter(|offset| *offset < UART_SIZE)
        {
            if bytes != 1 {
                return Err(MachineTrap::LoadAccessFault { address, bytes });
            }
            return Ok(match offset {
                UART_RECEIVE => self.console_input.pop_front().unwrap_or_default() as u64,
                UART_INTERRUPT_IDENTIFICATION => 1,
                UART_LINE_STATUS => {
                    let ready = if self.console_input.is_empty() {
                        0
                    } else {
                        UART_LINE_STATUS_DATA_READY
                    };
                    (UART_LINE_STATUS_TRANSMIT_EMPTY | ready) as u64
                }
                _ => 0,
            });
        }

        let range = self
            .ram_range(address, bytes)
            .ok_or(MachineTrap::LoadAccessFault { address, bytes })?;
        self.ram
            .read_value(range.start, bytes)
            .ok_or(MachineTrap::LoadAccessFault { address, bytes })
    }

    fn write(
        &mut self,
        address: u64,
        value: u64,
        bytes: u8,
    ) -> Result<Option<HaltStatus>, MachineTrap> {
        if let Some(offset) = address
            .checked_sub(UART_BASE)
            .filter(|offset| *offset < UART_SIZE)
        {
            if bytes != 1 || offset != UART_TRANSMIT {
                return Err(MachineTrap::StoreAccessFault { address, bytes });
            }
            self.push_console_output(value as u8)?;
            return Ok(None);
        }

        if (TEST_FINISHER_BASE..TEST_FINISHER_BASE + TEST_FINISHER_SIZE).contains(&address) {
            if bytes != 4 || address != TEST_FINISHER_BASE {
                return Err(MachineTrap::StoreAccessFault { address, bytes });
            }
            let value = value as u32;
            if value == 0x5555 {
                return Ok(Some(HaltStatus {
                    passed: true,
                    code: 0,
                }));
            }
            if value & 0xffff == 0x3333 {
                return Ok(Some(HaltStatus {
                    passed: false,
                    code: value >> 16,
                }));
            }
            return Ok(None);
        }

        let range = self
            .ram_range(address, bytes)
            .ok_or(MachineTrap::StoreAccessFault { address, bytes })?;
        if !self.ram.write_value(range.start, value, bytes) {
            return Err(MachineTrap::StoreAccessFault { address, bytes });
        }
        Ok(None)
    }

    fn ram_range(&self, address: u64, bytes: u8) -> Option<std::ops::Range<usize>> {
        let offset = address.checked_sub(DRAM_BASE)?;
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(usize::from(bytes))?;
        (end <= self.ram.len()).then_some(start..end)
    }

    fn read_ram(&self, address: u64, bytes: u8) -> Option<u64> {
        let range = self.ram_range(address, bytes)?;
        self.ram.read_value(range.start, bytes)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write_ram(&mut self, address: u64, value: u64, bytes: u8) -> bool {
        let Some(range) = self.ram_range(address, bytes) else {
            return false;
        };
        self.ram.write_value(range.start, value, bytes)
    }

    #[cfg(target_arch = "wasm32")]
    fn load_reserved_ram(&self, address: u64, bytes: u8) -> Option<(u64, AtomicReservation)> {
        let range = self.ram_range(address, bytes)?;
        self.ram.load_reserved(range.start, bytes)
    }

    #[cfg(target_arch = "wasm32")]
    fn store_conditional_ram(
        &self,
        reservation: AtomicReservation,
        address: u64,
        value: u64,
        bytes: u8,
    ) -> bool {
        let Some(range) = self.ram_range(address, bytes) else {
            return false;
        };
        self.ram
            .store_conditional(reservation, range.start, value, bytes)
    }

    #[cfg(target_arch = "wasm32")]
    fn update_ram(&self, address: u64, bytes: u8, value: impl FnOnce(u64) -> u64) -> Option<u64> {
        let range = self.ram_range(address, bytes)?;
        self.ram.update_value(range.start, bytes, value)
    }

    fn ram_range_len(&self, address: u64, bytes: usize) -> Option<std::ops::Range<usize>> {
        let offset = address.checked_sub(DRAM_BASE)?;
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(bytes)?;
        (end <= self.ram.len()).then_some(start..end)
    }

    fn write_ram_slice(&mut self, address: u64, bytes: &[u8]) -> bool {
        let Some(range) = self.ram_range_len(address, bytes.len()) else {
            return false;
        };
        self.ram.copy_from_slice(range.start, bytes)
    }

    fn push_console_output(&mut self, byte: u8) -> Result<(), MachineTrap> {
        if self.console_output.len() == self.max_console_bytes {
            return Err(MachineTrap::ConsoleLimit {
                limit: self.max_console_bytes,
            });
        }
        self.console_output.push(byte);
        Ok(())
    }

    fn tick(&mut self) {
        self.mtime = self.mtime.wrapping_add(1);
    }
}

/// An admitted RV64 machine whose execution can only advance in explicit slices.
#[derive(Clone, Debug)]
pub struct Machine {
    config: MachineConfig,
    hart_count: usize,
    active_hart_id: usize,
    harts: Vec<HartState>,
    hart_controls: Vec<HartControl>,
    scheduler_quantum_remaining: u64,
    devices: Devices,
    state: RunState,
    steps_executed: u64,
    instructions_retired: u64,
    firmware: SbiFirmware,
    next_host_request_id: u64,
    pending_9p_request: Option<PendingPlan9Request>,
    metrics: MachineMetrics,
}

impl Deref for Machine {
    type Target = HartState;

    fn deref(&self) -> &Self::Target {
        &self.harts[self.active_hart_id]
    }
}

impl DerefMut for Machine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.harts[self.active_hart_id]
    }
}

/// Opaque, reusable copy of a machine stopped before a host effect completes.
///
/// This in-memory type establishes the checkpoint safety boundary. A durable
/// codec must additionally bind bytes to the machine model, Linux image,
/// distribution generation, and outer artifact signature before those bytes
/// are admitted by a Realm.
#[derive(Clone, Debug)]
pub struct MachineCheckpoint {
    machine: Machine,
}

impl MachineCheckpoint {
    /// Exact RAM envelope encoded by this checkpoint.
    #[must_use]
    pub const fn ram_bytes(&self) -> usize {
        self.machine.config.ram_bytes
    }

    /// Exact retained-console envelope encoded by this checkpoint.
    #[must_use]
    pub const fn max_console_bytes(&self) -> usize {
        self.machine.config.max_console_bytes
    }

    /// Exact logical CPU topology encoded by this checkpoint.
    #[must_use]
    pub const fn hart_count(&self) -> usize {
        self.machine.hart_count
    }

    /// Host request that every restored machine must complete through a newly
    /// admitted principal-scoped provider.
    #[must_use]
    pub fn pending_host_request(&self) -> &Plan9Request {
        &self
            .machine
            .pending_9p_request
            .as_ref()
            .expect("checkpoint constructor requires a pending request")
            .request
    }

    /// Instantiate an isolated machine with the exact suspended guest state.
    #[must_use]
    pub fn restore(&self) -> Machine {
        let mut machine = self.machine.clone();
        machine.translation_cache.clear();
        machine
    }
}

impl Machine {
    /// Admit resources and construct a reset RV64 machine.
    pub fn new(config: MachineConfig) -> Result<Self, MachineError> {
        Self::new_with_harts(config, 1)
    }

    /// Admit resources and construct a reset RV64 machine with an exact guest
    /// topology. Secondary-hart execution is enabled only as its architectural
    /// state and SBI lifecycle are admitted; this constructor does not equate
    /// guest harts with native host workers.
    pub fn new_with_harts(config: MachineConfig, hart_count: usize) -> Result<Self, MachineError> {
        if !(MIN_RAM_BYTES..=MAX_RAM_BYTES).contains(&config.ram_bytes)
            || !config.ram_bytes.is_multiple_of(MIN_RAM_BYTES)
        {
            return Err(MachineError::InvalidRamBytes(config.ram_bytes));
        }
        if config.max_console_bytes > MAX_CONSOLE_BYTES {
            return Err(MachineError::InvalidConsoleBytes(config.max_console_bytes));
        }
        if !(1..=MAX_HARTS).contains(&hart_count) {
            return Err(MachineError::InvalidHartCount(hart_count));
        }
        Ok(Self {
            config,
            hart_count,
            active_hart_id: 0,
            harts: (0..hart_count)
                .map(|hart_id| {
                    HartState::new(
                        hart_id,
                        if hart_id == 0 {
                            HartLifecycle::Started
                        } else {
                            HartLifecycle::Stopped
                        },
                    )
                })
                .collect(),
            hart_controls: (0..hart_count)
                .map(|hart_id| {
                    HartControl::new(if hart_id == 0 {
                        HartLifecycle::Started
                    } else {
                        HartLifecycle::Stopped
                    })
                })
                .collect(),
            scheduler_quantum_remaining: HART_SCHEDULER_QUANTUM,
            devices: Devices::new(config)?,
            state: RunState::Runnable,
            steps_executed: 0,
            instructions_retired: 0,
            firmware: SbiFirmware::default(),
            next_host_request_id: 1,
            pending_9p_request: None,
            metrics: MachineMetrics::default(),
        })
    }

    /// Exact guest hart topology admitted for this machine.
    #[must_use]
    pub const fn hart_count(&self) -> usize {
        self.hart_count
    }

    /// Hart whose architectural state is currently selected by the deterministic
    /// scheduler. This is guest topology, not a native worker index.
    #[must_use]
    pub const fn active_hart_id(&self) -> usize {
        self.active_hart_id
    }

    fn reset_harts(&mut self) {
        self.active_hart_id = 0;
        self.harts = (0..self.hart_count)
            .map(|hart_id| {
                HartState::new(
                    hart_id,
                    if hart_id == 0 {
                        HartLifecycle::Started
                    } else {
                        HartLifecycle::Stopped
                    },
                )
            })
            .collect();
        self.rebuild_hart_controls();
        self.scheduler_quantum_remaining = HART_SCHEDULER_QUANTUM;
    }

    fn rebuild_hart_controls(&mut self) {
        self.hart_controls = self
            .harts
            .iter()
            .map(|hart| HartControl::new(hart.lifecycle))
            .collect();
    }

    fn rebuild_reservation_tokens(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            let devices = &self.devices;
            for hart in &mut self.harts {
                hart.reservation_token = hart.reservation.and_then(|(address, bytes)| {
                    devices
                        .load_reserved_ram(address, bytes)
                        .map(|(_, reservation)| reservation)
                });
            }
        }
    }

    fn hart_lifecycle(&self, hart_id: usize) -> Option<HartLifecycle> {
        self.hart_controls.get(hart_id).map(HartControl::lifecycle)
    }

    fn set_hart_lifecycle(&mut self, hart_id: usize, lifecycle: HartLifecycle) -> bool {
        let (Some(hart), Some(control)) =
            (self.harts.get_mut(hart_id), self.hart_controls.get(hart_id))
        else {
            return false;
        };
        hart.lifecycle = lifecycle;
        match lifecycle {
            HartLifecycle::Started => control
                .lifecycle
                .store(HART_CONTROL_STARTED, Ordering::Release),
            HartLifecycle::Stopped => control.stop(),
        }
        true
    }

    fn switch_active_hart(&mut self, hart_id: usize) -> bool {
        if self.harts.get(hart_id).is_none() {
            return false;
        }
        self.active_hart_id = hart_id;
        true
    }

    fn select_runnable_hart(&mut self) -> bool {
        if self.lifecycle == HartLifecycle::Started && self.scheduler_quantum_remaining > 0 {
            return true;
        }
        for offset in 1..=self.hart_count {
            let candidate = (self.active_hart_id + offset) % self.hart_count;
            if self.hart_lifecycle(candidate) == Some(HartLifecycle::Started) {
                let switched = self.switch_active_hart(candidate);
                debug_assert!(switched);
                self.scheduler_quantum_remaining = HART_SCHEDULER_QUANTUM;
                return true;
            }
        }
        false
    }

    /// Reset the machine and copy a raw RV64 image to [`DRAM_BASE`].
    pub fn load_program(&mut self, program: &[u8]) -> Result<(), MachineError> {
        if program.is_empty() || program.len() > self.config.ram_bytes {
            return Err(MachineError::InvalidProgramBytes {
                image: program.len(),
                ram: self.config.ram_bytes,
            });
        }
        self.devices.reset();
        self.reset_harts();
        if !self.devices.load_program(program) {
            return Err(MachineError::RamAllocationDenied(self.config.ram_bytes));
        }
        self.state = RunState::Runnable;
        self.steps_executed = 0;
        self.instructions_retired = 0;
        self.firmware = SbiFirmware::default();
        self.pending_9p_request = None;
        self.metrics = MachineMetrics::default();
        Ok(())
    }

    /// Admit a raw RV64 Linux `Image`, optional initramfs, and deterministic
    /// device tree, then enter the kernel in Supervisor mode.
    pub fn boot_linux(
        &mut self,
        kernel: &[u8],
        initramfs: &[u8],
        bootargs: &str,
    ) -> Result<LinuxBootLayout, MachineError> {
        let ram_bytes = self.config.ram_bytes;
        let invalid_images = || MachineError::InvalidLinuxImages {
            kernel: kernel.len(),
            initramfs: initramfs.len(),
            ram: ram_bytes,
        };
        if kernel.is_empty() || self.config.ram_bytes < MIN_LINUX_RAM_BYTES {
            return Err(invalid_images());
        }
        if bootargs.len() > MAX_BOOTARGS_BYTES || bootargs.as_bytes().contains(&0) {
            return Err(MachineError::InvalidBootArgsBytes(bootargs.len()));
        }
        let kernel_end = LINUX_KERNEL_BASE
            .checked_add(u64::try_from(kernel.len()).map_err(|_| invalid_images())?)
            .ok_or_else(invalid_images)?;
        let initrd_start = if initramfs.is_empty() {
            None
        } else {
            Some(align_up(kernel_end, 4096).ok_or_else(invalid_images)?)
        };
        let initrd_end = initrd_start
            .map(|start| {
                start
                    .checked_add(u64::try_from(initramfs.len()).map_err(|_| invalid_images())?)
                    .ok_or_else(invalid_images)
            })
            .transpose()?;
        let ram_end = DRAM_BASE
            .checked_add(u64::try_from(self.config.ram_bytes).map_err(|_| invalid_images())?)
            .ok_or_else(invalid_images)?;
        if kernel_end > ram_end || initrd_end.is_some_and(|end| end > ram_end) {
            return Err(invalid_images());
        }

        let fdt = build_linux_fdt(&LinuxFdtConfig {
            dram_base: DRAM_BASE,
            ram_bytes: self.config.ram_bytes as u64,
            hart_count: self.hart_count,
            uart_base: UART_BASE,
            uart_bytes: UART_SIZE,
            bootargs,
            initrd_start,
            initrd_end,
        });
        let fdt_end = LINUX_FDT_BASE
            .checked_add(u64::try_from(fdt.len()).map_err(|_| invalid_images())?)
            .ok_or_else(invalid_images)?;
        if fdt.len() > LINUX_FDT_MAX_BYTES || fdt_end > LINUX_KERNEL_BASE || fdt_end > ram_end {
            return Err(invalid_images());
        }

        self.devices.reset();
        self.reset_harts();
        if !self.devices.write_ram_slice(LINUX_KERNEL_BASE, kernel)
            || !self.devices.write_ram_slice(LINUX_FDT_BASE, &fdt)
            || initrd_start.is_some_and(|start| !self.devices.write_ram_slice(start, initramfs))
        {
            return Err(MachineError::RamAllocationDenied(self.config.ram_bytes));
        }
        self.cpu.pc = LINUX_KERNEL_BASE;
        self.cpu.privilege = Privilege::Supervisor;
        self.cpu.registers[10] = 0;
        self.cpu.registers[11] = LINUX_FDT_BASE;
        self.csrs.medeleg = MEDELEG_SUPPORTED;
        self.csrs.mideleg = MIDELEG_SUPPORTED;
        self.csrs.mcounteren = 0b111;
        self.state = RunState::Runnable;
        self.steps_executed = 0;
        self.instructions_retired = 0;
        self.firmware.enabled = true;
        self.pending_9p_request = None;
        self.metrics = MachineMetrics::default();

        Ok(LinuxBootLayout {
            kernel_start: LINUX_KERNEL_BASE,
            kernel_end,
            initrd_start,
            initrd_end,
            fdt_start: LINUX_FDT_BASE,
            fdt_bytes: fdt.len(),
        })
    }

    /// Add bytes that the guest may consume from the serial receive register.
    pub fn push_console_input(&mut self, bytes: &[u8]) {
        self.devices.console_input.extend(bytes.iter().copied());
    }

    /// Borrow an admitted physical RAM range for image measurement, snapshots,
    /// or differential verification. MMIO is never exposed as memory.
    #[must_use]
    pub fn physical_ram(&self, address: u64, bytes: usize) -> Option<&[u8]> {
        self.devices
            .ram_range_len(address, bytes)
            .and_then(|range| self.devices.ram.slice(range))
    }

    /// Copy a successful 9P response into the buffer admitted by the pending
    /// guest request and make the guest runnable again.
    pub fn complete_9p_request(
        &mut self,
        id: HostRequestId,
        response: &[u8],
    ) -> Result<(), HostCompletionError> {
        let Some(pending) = self.pending_9p_request.as_ref() else {
            return Err(HostCompletionError::NoPendingRequest);
        };
        if pending.request.id != id {
            return Err(HostCompletionError::RequestIdMismatch {
                expected: pending.request.id,
                actual: id,
            });
        }
        if response.len() < MIN_9P_MESSAGE_BYTES {
            return Err(HostCompletionError::InvalidResponseBytes(response.len()));
        }
        if response.len() > pending.request.max_response_bytes {
            return Err(HostCompletionError::ResponseTooLarge {
                response: response.len(),
                capacity: pending.request.max_response_bytes,
            });
        }
        let response_address = pending.response_address;
        let request_hart = pending.hart_id;
        if !self.switch_active_hart(request_hart) {
            return Err(HostCompletionError::RequestHartUnavailable {
                hart_id: request_hart,
            });
        }
        if !self.devices.write_ram_slice(response_address, response) {
            return Err(HostCompletionError::ResponseAddressUnavailable);
        }
        self.cpu.write(10, SBI_SUCCESS);
        self.cpu.write(11, response.len() as u64);
        self.pending_9p_request = None;
        Ok(())
    }

    /// Fail a pending 9P exchange without writing a guest response buffer.
    pub fn fail_9p_request(
        &mut self,
        id: HostRequestId,
        failure: HostRequestFailure,
    ) -> Result<(), HostCompletionError> {
        let Some(pending) = self.pending_9p_request.as_ref() else {
            return Err(HostCompletionError::NoPendingRequest);
        };
        if pending.request.id != id {
            return Err(HostCompletionError::RequestIdMismatch {
                expected: pending.request.id,
                actual: id,
            });
        }
        let request_hart = pending.hart_id;
        if !self.switch_active_hart(request_hart) {
            return Err(HostCompletionError::RequestHartUnavailable {
                hart_id: request_hart,
            });
        }
        self.cpu.write(
            10,
            match failure {
                HostRequestFailure::Failed => SBI_ERR_FAILED,
                HostRequestFailure::Denied => SBI_ERR_DENIED,
            },
        );
        self.cpu.write(11, 0);
        self.pending_9p_request = None;
        Ok(())
    }

    /// Read one architectural integer register. Register zero is always zero.
    #[must_use]
    pub fn register(&self, register: usize) -> Option<u64> {
        self.cpu.registers.get(register).copied()
    }

    fn read_device(&mut self, address: u64, bytes: u8) -> Result<u64, MachineTrap> {
        if let Some(offset) = address
            .checked_sub(CLINT_BASE)
            .filter(|offset| *offset < CLINT_SIZE)
        {
            return match (offset, bytes) {
                (CLINT_MSIP, 4) => Ok(u64::from(self.msip)),
                (CLINT_MTIMECMP, 8) => Ok(self.mtimecmp),
                (CLINT_MTIME, 8) => Ok(self.devices.mtime),
                _ => Err(MachineTrap::LoadAccessFault { address, bytes }),
            };
        }
        self.devices.read(address, bytes)
    }

    fn write_device(
        &mut self,
        address: u64,
        value: u64,
        bytes: u8,
    ) -> Result<Option<HaltStatus>, MachineTrap> {
        if let Some(offset) = address
            .checked_sub(CLINT_BASE)
            .filter(|offset| *offset < CLINT_SIZE)
        {
            return match (offset, bytes) {
                (CLINT_MSIP, 4) => {
                    self.msip = value & 1 != 0;
                    Ok(None)
                }
                (CLINT_MTIMECMP, 8) => {
                    self.mtimecmp = value;
                    Ok(None)
                }
                (CLINT_MTIME, 8) => {
                    self.devices.mtime = value;
                    Ok(None)
                }
                _ => Err(MachineTrap::StoreAccessFault { address, bytes }),
            };
        }
        self.devices.write(address, value, bytes)
    }

    /// Current guest program counter.
    #[must_use]
    pub fn pc(&self) -> u64 {
        self.cpu.pc
    }

    /// Current guest privilege level.
    #[must_use]
    pub fn privilege(&self) -> Privilege {
        self.cpu.privilege
    }

    /// Read an implemented architectural CSR without bypassing the typed set.
    #[must_use]
    pub fn csr(&self, csr: Csr) -> u64 {
        self.read_csr(csr)
    }

    /// Return cumulative reference-interpreter measurements for this image.
    #[must_use]
    pub const fn metrics(&self) -> MachineMetrics {
        self.metrics
    }

    /// Capture a reusable prewarm point at a drained, input-free host
    /// suspension. The pending request is retained and must be completed by a
    /// fresh principal-scoped provider after every restore.
    pub fn checkpoint_host_suspension(&self) -> Result<MachineCheckpoint, CheckpointError> {
        if !matches!(self.state, RunState::Runnable) {
            return Err(CheckpointError::NotRunnable);
        }
        if self.pending_9p_request.is_none() {
            return Err(CheckpointError::NoPendingHostRequest);
        }
        if !self.devices.console_input.is_empty() {
            return Err(CheckpointError::PendingConsoleInput {
                bytes: self.devices.console_input.len(),
            });
        }
        if !self.devices.console_output.is_empty() {
            return Err(CheckpointError::UndrainedConsoleOutput {
                bytes: self.devices.console_output.len(),
            });
        }
        if let Some(hart_id) = self
            .hart_controls
            .iter()
            .position(|control| !control.is_quiescent())
        {
            return Err(CheckpointError::PendingHartControl { hart_id });
        }

        let mut machine = self.clone();
        machine.translation_cache.clear();
        Ok(MachineCheckpoint { machine })
    }

    /// Run at most `instruction_budget` instructions and return control to the Realm.
    pub fn run_slice(&mut self, instruction_budget: u64) -> SliceReport {
        self.run_slice_inner(instruction_budget, true)
    }

    /// Run only one exact admitted hart without invoking the deterministic
    /// round-robin scheduler.
    ///
    /// This is the worker-affine primitive used by a parallel epoch. A stopped
    /// hart yields without consuming a step. Device effects and aggregate
    /// accounting remain machine-wide.
    pub fn run_hart_slice(
        &mut self,
        hart_id: usize,
        instruction_budget: u64,
    ) -> Result<SliceReport, InvalidHartId> {
        if hart_id >= self.hart_count {
            return Err(InvalidHartId {
                hart_id,
                hart_count: self.hart_count,
            });
        }
        self.active_hart_id = hart_id;
        Ok(self.run_slice_inner(instruction_budget, false))
    }

    fn run_slice_inner(&mut self, instruction_budget: u64, scheduled: bool) -> SliceReport {
        let mut steps = 0_u64;
        let mut retired = 0_u64;
        let _ = self.apply_hart_control(self.active_hart_id);
        while steps < instruction_budget
            && matches!(self.state, RunState::Runnable)
            && self.pending_9p_request.is_none()
        {
            if scheduled && !self.select_runnable_hart() {
                break;
            }
            if scheduled {
                let applied = self.apply_hart_control(self.active_hart_id);
                debug_assert!(applied);
            }
            if !scheduled && self.lifecycle != HartLifecycle::Started {
                break;
            }
            match self.step() {
                Ok(effect) => {
                    steps = steps.saturating_add(1);
                    self.steps_executed = self.steps_executed.saturating_add(1);
                    self.cycle = self.cycle.wrapping_add(1);
                    self.devices.tick();
                    if scheduled {
                        self.scheduler_quantum_remaining =
                            self.scheduler_quantum_remaining.saturating_sub(1);
                    }
                    if effect.retired {
                        retired = retired.saturating_add(1);
                        self.instructions_retired = self.instructions_retired.saturating_add(1);
                        self.instret = self.instret.wrapping_add(1);
                    }
                    if let Some(status) = effect.halt {
                        self.state = RunState::Halted(status);
                    }
                }
                Err(trap) => {
                    if let Some((cause, value)) = architectural_exception(&trap) {
                        steps = steps.saturating_add(1);
                        self.steps_executed = self.steps_executed.saturating_add(1);
                        self.cycle = self.cycle.wrapping_add(1);
                        self.devices.tick();
                        if scheduled {
                            self.scheduler_quantum_remaining =
                                self.scheduler_quantum_remaining.saturating_sub(1);
                        }
                        self.take_exception(cause, value, self.cpu.pc);
                        self.cpu.registers[0] = 0;
                    } else {
                        self.state = RunState::Trapped(trap);
                    }
                }
            }
        }

        let outcome = match (&self.pending_9p_request, &self.state) {
            (Some(pending), _) => SliceOutcome::HostRequest(pending.request.clone()),
            (None, RunState::Runnable) => SliceOutcome::Yielded,
            (None, RunState::Halted(status)) => SliceOutcome::Halted(*status),
            (None, RunState::Trapped(trap)) => SliceOutcome::Trapped(trap.clone()),
        };
        SliceReport {
            outcome,
            steps_executed: steps,
            total_steps_executed: self.steps_executed,
            instructions_retired: retired,
            total_instructions_retired: self.instructions_retired,
            console: self.devices.take_new_console(),
        }
    }

    fn step(&mut self) -> Result<StepEffect, MachineTrap> {
        self.refresh_hardware_interrupts();
        if self.take_pending_interrupt() {
            return Ok(StepEffect::trapped_architecturally());
        }
        let pc = self.cpu.pc;
        if pc & 1 != 0 {
            return Err(MachineTrap::InstructionAddressMisaligned { address: pc });
        }
        self.metrics.instruction_fetches = self.metrics.instruction_fetches.saturating_add(1);
        let physical_pc = self.translate(pc, AccessType::Instruction)?;
        let first_half = self
            .read_device(physical_pc, 2)
            .map_err(|_| MachineTrap::InstructionAccessFault { address: pc })?
            as u16;
        if first_half & 0b11 != 0b11 {
            let (next_pc, halt) = self.execute_compressed(first_half, pc)?;
            self.cpu.pc = next_pc;
            self.cpu.registers[0] = 0;
            return Ok(StepEffect::retired(halt));
        }
        let instruction = if pc & GUEST_PAGE_MASK == GUEST_PAGE_MASK - 1 {
            let second_pc = pc.wrapping_add(2);
            let second_physical = self.translate(second_pc, AccessType::Instruction)?;
            let second_half = self
                .read_device(second_physical, 2)
                .map_err(|_| MachineTrap::InstructionAccessFault { address: second_pc })?
                as u32;
            u32::from(first_half) | (second_half << 16)
        } else {
            self.read_device(physical_pc, 4)
                .map_err(|_| MachineTrap::InstructionAccessFault { address: pc })?
                as u32
        };
        let opcode = instruction & 0x7f;
        let rd = ((instruction >> 7) & 0x1f) as usize;
        let funct3 = (instruction >> 12) & 0x7;
        let rs1 = ((instruction >> 15) & 0x1f) as usize;
        let rs2 = ((instruction >> 20) & 0x1f) as usize;
        let funct7 = instruction >> 25;
        let mut next_pc = pc.wrapping_add(4);
        let mut halt = None;

        match opcode {
            0x03 => {
                let address = self.cpu.read(rs1).wrapping_add(immediate_i(instruction));
                let (bytes, signed) = match funct3 {
                    0 => (1, true),
                    1 => (2, true),
                    2 => (4, true),
                    3 => (8, true),
                    4 => (1, false),
                    5 => (2, false),
                    6 => (4, false),
                    _ => return Err(illegal(pc, instruction)),
                };
                ensure_aligned(address, bytes, false)?;
                let physical = self.translate(address, AccessType::Load)?;
                let value = self
                    .read_device(physical, bytes)
                    .map_err(|_| MachineTrap::LoadAccessFault { address, bytes })?;
                let value = if signed {
                    sign_extend(value, u32::from(bytes) * 8)
                } else {
                    value
                };
                self.cpu.write(rd, value);
            }
            0x07 => self.execute_float_load(instruction, rd, rs1, funct3, pc)?,
            0x0f => {
                if funct3 > 1 {
                    return Err(illegal(pc, instruction));
                }
            }
            0x13 => self.execute_op_imm(instruction, rd, rs1, funct3, pc)?,
            0x17 => self
                .cpu
                .write(rd, pc.wrapping_add(immediate_u(instruction))),
            0x1b => self.execute_op_imm_32(instruction, rd, rs1, funct3, pc)?,
            0x23 => {
                let bytes = match funct3 {
                    0 => 1,
                    1 => 2,
                    2 => 4,
                    3 => 8,
                    _ => return Err(illegal(pc, instruction)),
                };
                let address = self.cpu.read(rs1).wrapping_add(immediate_s(instruction));
                ensure_aligned(address, bytes, true)?;
                let physical = self.translate(address, AccessType::Store)?;
                self.invalidate_reservation(physical, bytes);
                let value = self.cpu.read(rs2);
                halt = self
                    .write_device(physical, value, bytes)
                    .map_err(|trap| match trap {
                        MachineTrap::ConsoleLimit { .. } => trap,
                        _ => MachineTrap::StoreAccessFault { address, bytes },
                    })?;
            }
            0x27 => {
                halt = self.execute_float_store(instruction, rs1, rs2, funct3, pc)?;
            }
            0x2f => halt = self.execute_atomic(instruction, rd, rs1, rs2, funct3, pc)?,
            0x33 => self.execute_op(instruction, rd, rs1, rs2, funct3, funct7, pc)?,
            0x37 => self.cpu.write(rd, immediate_u(instruction)),
            0x3b => self.execute_op_32(instruction, rd, rs1, rs2, funct3, funct7, pc)?,
            0x43 | 0x47 | 0x4b | 0x4f => {
                self.execute_float_fused(instruction, opcode, pc)?;
            }
            0x53 => {
                self.execute_float_op(instruction, rd, rs1, rs2, funct3, funct7, pc)?;
            }
            0x63 => {
                let lhs = self.cpu.read(rs1);
                let rhs = self.cpu.read(rs2);
                let take = match funct3 {
                    0 => lhs == rhs,
                    1 => lhs != rhs,
                    4 => (lhs as i64) < (rhs as i64),
                    5 => (lhs as i64) >= (rhs as i64),
                    6 => lhs < rhs,
                    7 => lhs >= rhs,
                    _ => return Err(illegal(pc, instruction)),
                };
                if take {
                    let target = pc.wrapping_add(immediate_b(instruction));
                    ensure_instruction_aligned(target)?;
                    next_pc = target;
                }
            }
            0x67 => {
                if funct3 != 0 {
                    return Err(illegal(pc, instruction));
                }
                let target = self.cpu.read(rs1).wrapping_add(immediate_i(instruction)) & !1;
                ensure_instruction_aligned(target)?;
                self.cpu.write(rd, next_pc);
                next_pc = target;
            }
            0x6f => {
                let target = pc.wrapping_add(immediate_j(instruction));
                ensure_instruction_aligned(target)?;
                self.cpu.write(rd, next_pc);
                next_pc = target;
            }
            0x73 => {
                if funct3 != 0 {
                    self.execute_csr(instruction, rd, rs1, funct3, pc)?;
                } else {
                    match instruction {
                        0x0000_0073 => {
                            if self.cpu.privilege == Privilege::Supervisor && self.firmware.enabled
                            {
                                halt = self.handle_sbi_call()?;
                                self.cpu.pc = next_pc;
                                self.cpu.registers[0] = 0;
                                return Ok(StepEffect {
                                    halt,
                                    retired: false,
                                });
                            }
                            self.take_exception(ecall_cause(self.cpu.privilege), 0, pc);
                            self.cpu.registers[0] = 0;
                            return Ok(StepEffect::trapped_architecturally());
                        }
                        0x0010_0073 => return Err(MachineTrap::Breakpoint { pc }),
                        value if value & 0xfe00_7fff == 0x1200_0073 => {
                            if self.cpu.privilege < Privilege::Supervisor {
                                return Err(illegal(pc, instruction));
                            }
                            self.flush_translation_cache();
                        }
                        0x1050_0073 => {
                            if self.cpu.privilege < Privilege::Supervisor {
                                return Err(illegal(pc, instruction));
                            }
                        }
                        0x1020_0073 => next_pc = self.execute_sret(pc, instruction)?,
                        0x3020_0073 => next_pc = self.execute_mret(pc, instruction)?,
                        _ => return Err(illegal(pc, instruction)),
                    }
                }
            }
            _ => return Err(illegal(pc, instruction)),
        }

        self.cpu.pc = next_pc;
        self.cpu.registers[0] = 0;
        Ok(StepEffect::retired(halt))
    }

    fn execute_compressed(
        &mut self,
        instruction: u16,
        pc: u64,
    ) -> Result<(u64, Option<HaltStatus>), MachineTrap> {
        let quadrant = instruction & 0b11;
        let funct3 = instruction >> 13;
        let rd = usize::from((instruction >> 7) & 0x1f);
        let rs2 = usize::from((instruction >> 2) & 0x1f);
        let rd_prime = usize::from(8 + ((instruction >> 2) & 0x7));
        let rs1_prime = usize::from(8 + ((instruction >> 7) & 0x7));
        let rs2_prime = usize::from(8 + ((instruction >> 2) & 0x7));
        let mut next_pc = pc.wrapping_add(2);
        let mut halt = None;

        match (quadrant, funct3) {
            (0b00, 0b000) => {
                let immediate = compressed_addi4spn_immediate(instruction);
                if immediate == 0 {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                let value = self.cpu.read(2).wrapping_add(immediate);
                self.cpu.write(rd_prime, value);
            }
            (0b00, 0b001) => {
                if !self.csrs.floating_enabled() {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                let address = self
                    .cpu
                    .read(rs1_prime)
                    .wrapping_add(compressed_double_immediate(instruction));
                ensure_aligned(address, 8, false)?;
                let physical = self.translate(address, AccessType::Load)?;
                let value = self
                    .read_device(physical, 8)
                    .map_err(|_| MachineTrap::LoadAccessFault { address, bytes: 8 })?;
                self.cpu.write_float64(rd_prime, value);
                self.csrs.mark_float_dirty();
            }
            (0b00, 0b010) => {
                let address = self
                    .cpu
                    .read(rs1_prime)
                    .wrapping_add(compressed_word_immediate(instruction));
                ensure_aligned(address, 4, false)?;
                let physical = self.translate(address, AccessType::Load)?;
                let value = self
                    .read_device(physical, 4)
                    .map_err(|_| MachineTrap::LoadAccessFault { address, bytes: 4 })?;
                self.cpu.write(rd_prime, sign_extend(value, 32));
            }
            (0b00, 0b011) => {
                let address = self
                    .cpu
                    .read(rs1_prime)
                    .wrapping_add(compressed_double_immediate(instruction));
                ensure_aligned(address, 8, false)?;
                let physical = self.translate(address, AccessType::Load)?;
                let value = self
                    .read_device(physical, 8)
                    .map_err(|_| MachineTrap::LoadAccessFault { address, bytes: 8 })?;
                self.cpu.write(rd_prime, value);
            }
            (0b00, 0b101) => {
                if !self.csrs.floating_enabled() {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                let address = self
                    .cpu
                    .read(rs1_prime)
                    .wrapping_add(compressed_double_immediate(instruction));
                ensure_aligned(address, 8, true)?;
                let physical = self.translate(address, AccessType::Store)?;
                self.invalidate_reservation(physical, 8);
                let value = self.cpu.read_float64(rs2_prime);
                halt = self
                    .write_device(physical, value, 8)
                    .map_err(|trap| map_store_fault(trap, address, 8))?;
                self.csrs.mark_float_dirty();
            }
            (0b00, 0b110) | (0b00, 0b111) => {
                let bytes = if funct3 == 0b110 { 4 } else { 8 };
                let immediate = if bytes == 4 {
                    compressed_word_immediate(instruction)
                } else {
                    compressed_double_immediate(instruction)
                };
                let address = self.cpu.read(rs1_prime).wrapping_add(immediate);
                ensure_aligned(address, bytes, true)?;
                let physical = self.translate(address, AccessType::Store)?;
                self.invalidate_reservation(physical, bytes);
                let value = self.cpu.read(rs2_prime);
                halt = self
                    .write_device(physical, value, bytes)
                    .map_err(|trap| map_store_fault(trap, address, bytes))?;
            }
            (0b01, 0b000) => {
                let value = self
                    .cpu
                    .read(rd)
                    .wrapping_add(compressed_immediate(instruction));
                self.cpu.write(rd, value);
            }
            (0b01, 0b001) => {
                if rd == 0 {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                let value = (self.cpu.read(rd) as u32)
                    .wrapping_add(compressed_immediate(instruction) as u32);
                self.cpu.write(rd, sign_extend(u64::from(value), 32));
            }
            (0b01, 0b010) => self.cpu.write(rd, compressed_immediate(instruction)),
            (0b01, 0b011) if rd == 2 => {
                let immediate = compressed_addi16sp_immediate(instruction);
                if immediate == 0 {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                let value = self.cpu.read(2).wrapping_add(immediate);
                self.cpu.write(2, value);
            }
            (0b01, 0b011) => {
                let immediate = compressed_lui_immediate(instruction);
                if rd == 0 || immediate == 0 {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                self.cpu.write(rd, immediate);
            }
            (0b01, 0b100) => {
                let subop = (instruction >> 10) & 0b11;
                let lhs = self.cpu.read(rs1_prime);
                match subop {
                    0b00 => self.cpu.write(
                        rs1_prime,
                        lhs.wrapping_shr(u32::from(compressed_shift(instruction))),
                    ),
                    0b01 => self.cpu.write(
                        rs1_prime,
                        ((lhs as i64) >> compressed_shift(instruction)) as u64,
                    ),
                    0b10 => self
                        .cpu
                        .write(rs1_prime, lhs & compressed_immediate(instruction)),
                    0b11 => {
                        let rhs = self.cpu.read(rs2_prime);
                        let operation = ((instruction >> 12) & 1, (instruction >> 5) & 0b11);
                        let value = match operation {
                            (0, 0) => lhs.wrapping_sub(rhs),
                            (0, 1) => lhs ^ rhs,
                            (0, 2) => lhs | rhs,
                            (0, 3) => lhs & rhs,
                            (1, 0) => {
                                sign_extend(u64::from((lhs as u32).wrapping_sub(rhs as u32)), 32)
                            }
                            (1, 1) => {
                                sign_extend(u64::from((lhs as u32).wrapping_add(rhs as u32)), 32)
                            }
                            _ => return Err(illegal(pc, u32::from(instruction))),
                        };
                        self.cpu.write(rs1_prime, value);
                    }
                    _ => unreachable!(),
                }
            }
            (0b01, 0b101) => {
                next_pc = pc.wrapping_add(compressed_jump_immediate(instruction));
                ensure_instruction_aligned(next_pc)?;
            }
            (0b01, 0b110) | (0b01, 0b111) => {
                let zero = self.cpu.read(rs1_prime) == 0;
                if zero == (funct3 == 0b110) {
                    next_pc = pc.wrapping_add(compressed_branch_immediate(instruction));
                    ensure_instruction_aligned(next_pc)?;
                }
            }
            (0b10, 0b000) => {
                let value = self
                    .cpu
                    .read(rd)
                    .wrapping_shl(u32::from(compressed_shift(instruction)));
                self.cpu.write(rd, value);
            }
            (0b10, 0b001) => {
                if !self.csrs.floating_enabled() {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                let address = self
                    .cpu
                    .read(2)
                    .wrapping_add(compressed_double_sp_immediate(instruction));
                ensure_aligned(address, 8, false)?;
                let physical = self.translate(address, AccessType::Load)?;
                let value = self
                    .read_device(physical, 8)
                    .map_err(|_| MachineTrap::LoadAccessFault { address, bytes: 8 })?;
                self.cpu.write_float64(rd, value);
                self.csrs.mark_float_dirty();
            }
            (0b10, 0b010) | (0b10, 0b011) => {
                if rd == 0 {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                let bytes = if funct3 == 0b010 { 4 } else { 8 };
                let immediate = if bytes == 4 {
                    compressed_word_sp_immediate(instruction)
                } else {
                    compressed_double_sp_immediate(instruction)
                };
                let address = self.cpu.read(2).wrapping_add(immediate);
                ensure_aligned(address, bytes, false)?;
                let physical = self.translate(address, AccessType::Load)?;
                let value = self
                    .read_device(physical, bytes)
                    .map_err(|_| MachineTrap::LoadAccessFault { address, bytes })?;
                self.cpu.write(
                    rd,
                    if bytes == 4 {
                        sign_extend(value, 32)
                    } else {
                        value
                    },
                );
            }
            (0b10, 0b100) => {
                let selector = (instruction >> 12) & 1;
                match (selector, rd, rs2) {
                    (0, 0, 0) => return Err(illegal(pc, u32::from(instruction))),
                    (0, _, 0) => {
                        next_pc = self.cpu.read(rd) & !1;
                        ensure_instruction_aligned(next_pc)?;
                    }
                    (0, _, _) => {
                        let value = self.cpu.read(rs2);
                        self.cpu.write(rd, value);
                    }
                    (1, 0, 0) => return Err(MachineTrap::Breakpoint { pc }),
                    (1, _, 0) => {
                        next_pc = self.cpu.read(rd) & !1;
                        ensure_instruction_aligned(next_pc)?;
                        self.cpu.write(1, pc.wrapping_add(2));
                    }
                    (1, _, _) => {
                        let value = self.cpu.read(rd).wrapping_add(self.cpu.read(rs2));
                        self.cpu.write(rd, value);
                    }
                    _ => unreachable!(),
                }
            }
            (0b10, 0b101) => {
                if !self.csrs.floating_enabled() {
                    return Err(illegal(pc, u32::from(instruction)));
                }
                let address = self
                    .cpu
                    .read(2)
                    .wrapping_add(compressed_double_store_sp_immediate(instruction));
                ensure_aligned(address, 8, true)?;
                let physical = self.translate(address, AccessType::Store)?;
                self.invalidate_reservation(physical, 8);
                let value = self.cpu.read_float64(rs2);
                halt = self
                    .write_device(physical, value, 8)
                    .map_err(|trap| map_store_fault(trap, address, 8))?;
                self.csrs.mark_float_dirty();
            }
            (0b10, 0b110) | (0b10, 0b111) => {
                let bytes = if funct3 == 0b110 { 4 } else { 8 };
                let immediate = if bytes == 4 {
                    compressed_word_store_sp_immediate(instruction)
                } else {
                    compressed_double_store_sp_immediate(instruction)
                };
                let address = self.cpu.read(2).wrapping_add(immediate);
                ensure_aligned(address, bytes, true)?;
                let physical = self.translate(address, AccessType::Store)?;
                self.invalidate_reservation(physical, bytes);
                let value = self.cpu.read(rs2);
                halt = self
                    .write_device(physical, value, bytes)
                    .map_err(|trap| map_store_fault(trap, address, bytes))?;
            }
            _ => return Err(illegal(pc, u32::from(instruction))),
        }

        Ok((next_pc, halt))
    }

    fn translate(&mut self, address: u64, access: AccessType) -> Result<u64, MachineTrap> {
        match access {
            AccessType::Instruction => {
                self.metrics.instruction_translations =
                    self.metrics.instruction_translations.saturating_add(1);
            }
            AccessType::Load => {
                self.metrics.load_translations = self.metrics.load_translations.saturating_add(1);
            }
            AccessType::Store => {
                self.metrics.store_translations = self.metrics.store_translations.saturating_add(1);
            }
        }
        let effective_privilege = if access != AccessType::Instruction
            && self.cpu.privilege == Privilege::Machine
            && self.csrs.mstatus & MSTATUS_MPRV != 0
        {
            Privilege::from_mpp(self.csrs.mstatus).unwrap_or(Privilege::User)
        } else {
            self.cpu.privilege
        };
        let mode = self.csrs.satp >> SATP_MODE_SHIFT;
        if effective_privilege == Privilege::Machine || mode == SATP_MODE_BARE {
            return Ok(address);
        }
        if mode != SATP_MODE_SV39 || !is_sv39_canonical(address) {
            return Err(access.page_fault(address));
        }

        let context = TranslationContext {
            satp: self.csrs.satp,
            permission_context: self.csrs.mstatus & (MSTATUS_SUM | MSTATUS_MXR),
            privilege: effective_privilege,
            access,
        };
        if let Some(physical) = self.translation_cache.lookup(address, context) {
            self.metrics.translation_cache_hits =
                self.metrics.translation_cache_hits.saturating_add(1);
            return Ok(physical);
        }
        self.metrics.translation_cache_misses =
            self.metrics.translation_cache_misses.saturating_add(1);
        self.metrics.sv39_walks = self.metrics.sv39_walks.saturating_add(1);

        let vpn = [
            (address >> 12) & 0x1ff,
            (address >> 21) & 0x1ff,
            (address >> 30) & 0x1ff,
        ];
        let mut table = (self.csrs.satp & SATP_PPN_MASK) << 12;
        for level in (0..=2).rev() {
            let pte_address = table
                .checked_add(vpn[level] * 8)
                .ok_or_else(|| access.access_fault(address, 8))?;
            self.metrics.page_table_entries_read =
                self.metrics.page_table_entries_read.saturating_add(1);
            let pte = self
                .devices
                .read_ram(pte_address, 8)
                .ok_or_else(|| access.access_fault(address, 8))?;
            if pte & PTE_RESERVED_MASK != 0
                || pte & PTE_VALID == 0
                || pte & PTE_READ == 0 && pte & PTE_WRITE != 0
            {
                return Err(access.page_fault(address));
            }
            if pte & (PTE_READ | PTE_EXECUTE) == 0 {
                table = (pte >> 10 & PTE_PPN_MASK) << 12;
                continue;
            }

            let pte_ppn = (pte >> 10) & PTE_PPN_MASK;
            let lower_ppn_bits = match level {
                0 => 0,
                1 => 0x1ff,
                2 => 0x3_ffff,
                _ => unreachable!("Sv39 has exactly three levels"),
            };
            if pte_ppn & lower_ppn_bits != 0
                || !self.page_permissions_allow(pte, access, effective_privilege)
            {
                return Err(access.page_fault(address));
            }

            let required_ad = PTE_ACCESSED
                | if access == AccessType::Store {
                    PTE_DIRTY
                } else {
                    0
                };
            if pte & required_ad != required_ad {
                self.metrics.page_table_entries_written =
                    self.metrics.page_table_entries_written.saturating_add(1);
                #[cfg(target_arch = "wasm32")]
                if self
                    .devices
                    .update_ram(pte_address, 8, |value| value | required_ad)
                    .is_none()
                {
                    return Err(access.access_fault(address, 8));
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    if !self.devices.write_ram(pte_address, pte | required_ad, 8) {
                        return Err(access.access_fault(address, 8));
                    }
                }
            }

            let mut physical_ppn = pte_ppn;
            if level >= 1 {
                physical_ppn = (physical_ppn & !0x1ff) | vpn[0];
            }
            if level == 2 {
                physical_ppn = (physical_ppn & !0x3_ffff) | (vpn[1] << 9) | vpn[0];
            }
            let physical = (physical_ppn << GUEST_PAGE_SHIFT) | (address & GUEST_PAGE_MASK);
            self.translation_cache.insert(address, physical, context);
            return Ok(physical);
        }
        Err(access.page_fault(address))
    }

    fn page_permissions_allow(&self, pte: u64, access: AccessType, privilege: Privilege) -> bool {
        let user_page = pte & PTE_USER != 0;
        if privilege == Privilege::User && !user_page {
            return false;
        }
        if privilege == Privilege::Supervisor
            && user_page
            && (access == AccessType::Instruction || self.csrs.mstatus & MSTATUS_SUM == 0)
        {
            return false;
        }
        match access {
            AccessType::Instruction => pte & PTE_EXECUTE != 0,
            AccessType::Load => {
                pte & PTE_READ != 0
                    || self.csrs.mstatus & MSTATUS_MXR != 0 && pte & PTE_EXECUTE != 0
            }
            AccessType::Store => pte & PTE_WRITE != 0,
        }
    }

    fn execute_atomic(
        &mut self,
        instruction: u32,
        rd: usize,
        rs1: usize,
        rs2: usize,
        funct3: u32,
        pc: u64,
    ) -> Result<Option<HaltStatus>, MachineTrap> {
        let bytes = match funct3 {
            2 => 4,
            3 => 8,
            _ => return Err(illegal(pc, instruction)),
        };
        let operation = instruction >> 27;
        let virtual_address = self.cpu.read(rs1);
        if operation == 0x02 {
            if rs2 != 0 {
                return Err(illegal(pc, instruction));
            }
            ensure_aligned(virtual_address, bytes, false)?;
            let physical = self.translate(virtual_address, AccessType::Load)?;
            #[cfg(target_arch = "wasm32")]
            let (value, reservation_token) = self
                .devices
                .load_reserved_ram(physical, bytes)
                .ok_or(MachineTrap::LoadAccessFault {
                    address: virtual_address,
                    bytes,
                })?;
            #[cfg(not(target_arch = "wasm32"))]
            let value =
                self.read_device(physical, bytes)
                    .map_err(|_| MachineTrap::LoadAccessFault {
                        address: virtual_address,
                        bytes,
                    })?;
            self.reservation = Some((physical, bytes));
            #[cfg(target_arch = "wasm32")]
            {
                self.reservation_token = Some(reservation_token);
            }
            self.cpu.write(
                rd,
                if bytes == 4 {
                    sign_extend(value, 32)
                } else {
                    value
                },
            );
            return Ok(None);
        }

        ensure_aligned(virtual_address, bytes, true)?;
        let physical = self.translate(virtual_address, AccessType::Store)?;
        if operation == 0x03 {
            let address_matches = self.reservation == Some((physical, bytes));
            self.reservation = None;
            let value = self.cpu.read(rs2);
            #[cfg(target_arch = "wasm32")]
            let succeeds = self.reservation_token.take().is_some_and(|reservation| {
                address_matches
                    && self
                        .devices
                        .store_conditional_ram(reservation, physical, value, bytes)
            });
            #[cfg(not(target_arch = "wasm32"))]
            let succeeds = address_matches;
            if !succeeds {
                self.cpu.write(rd, 1);
                return Ok(None);
            }
            #[cfg(not(target_arch = "wasm32"))]
            let halt = self
                .write_device(physical, value, bytes)
                .map_err(|trap| map_store_fault(trap, virtual_address, bytes))?;
            self.cpu.write(rd, 0);
            #[cfg(target_arch = "wasm32")]
            return Ok(None);
            #[cfg(not(target_arch = "wasm32"))]
            return Ok(halt);
        }

        let rhs = self.cpu.read(rs2);
        atomic_result(operation, 0, rhs, bytes).ok_or_else(|| illegal(pc, instruction))?;
        self.invalidate_reservation(physical, bytes);
        #[cfg(target_arch = "wasm32")]
        let old = self
            .devices
            .update_ram(physical, bytes, |old| {
                atomic_result(operation, old, rhs, bytes).expect("atomic operation validated")
            })
            .ok_or(MachineTrap::StoreAccessFault {
                address: virtual_address,
                bytes,
            })?;
        #[cfg(not(target_arch = "wasm32"))]
        let old = self
            .read_device(physical, bytes)
            .map_err(|_| MachineTrap::StoreAccessFault {
                address: virtual_address,
                bytes,
            })?;
        #[cfg(not(target_arch = "wasm32"))]
        let value = atomic_result(operation, old, rhs, bytes).expect("atomic operation validated");
        #[cfg(not(target_arch = "wasm32"))]
        let halt = self
            .write_device(physical, value, bytes)
            .map_err(|trap| map_store_fault(trap, virtual_address, bytes))?;
        self.cpu.write(
            rd,
            if bytes == 4 {
                sign_extend(old, 32)
            } else {
                old
            },
        );
        #[cfg(target_arch = "wasm32")]
        return Ok(None);
        #[cfg(not(target_arch = "wasm32"))]
        Ok(halt)
    }

    fn invalidate_reservation(&mut self, address: u64, bytes: u8) {
        let overlaps = |reservation: Option<(u64, u8)>| {
            reservation.is_some_and(|(reserved, reserved_bytes)| {
                let end = address.saturating_add(u64::from(bytes));
                let reserved_end = reserved.saturating_add(u64::from(reserved_bytes));
                address < reserved_end && reserved < end
            })
        };
        for hart in &mut self.harts {
            if overlaps(hart.reservation) {
                hart.reservation = None;
                #[cfg(target_arch = "wasm32")]
                {
                    hart.reservation_token = None;
                }
            }
        }
    }

    fn execute_csr(
        &mut self,
        instruction: u32,
        rd: usize,
        rs1: usize,
        funct3: u32,
        pc: u64,
    ) -> Result<(), MachineTrap> {
        let source = if funct3 & 0b100 == 0 {
            self.cpu.read(rs1)
        } else {
            rs1 as u64
        };
        let operation = funct3 & 0b011;
        let reads = operation != 1 || rd != 0;
        let writes = operation == 1 || rs1 != 0;
        if operation == 0 {
            return Err(illegal(pc, instruction));
        }

        let address = (instruction >> 20) as u16;
        let csr = self
            .csrs
            .validate_access(address, self.cpu.privilege, writes)
            .ok_or_else(|| illegal(pc, instruction))?;
        let old = if reads || operation != 1 {
            self.read_csr(csr)
        } else {
            0
        };
        if writes {
            let value = match operation {
                1 => source,
                2 => old | source,
                3 => old & !source,
                _ => unreachable!("validated CSR operation"),
            };
            self.write_csr(csr, value);
        }
        if reads {
            self.cpu.write(rd, old);
        }
        Ok(())
    }

    fn read_csr(&self, csr: Csr) -> u64 {
        match csr {
            Csr::Cycle | Csr::Mcycle => self.cycle,
            Csr::Time => self.devices.mtime,
            Csr::Instret | Csr::Minstret => self.instret,
            Csr::Mhartid => self.active_hart_id as u64,
            _ => self.csrs.read(csr),
        }
    }

    fn write_csr(&mut self, csr: Csr, value: u64) {
        match csr {
            Csr::Mcycle => self.cycle = value,
            Csr::Minstret => self.instret = value,
            Csr::Satp => {
                let previous = self.csrs.satp;
                self.csrs.write(csr, value);
                if self.csrs.satp != previous {
                    self.flush_translation_cache();
                }
            }
            _ => self.csrs.write(csr, value),
        }
    }

    fn flush_translation_cache(&mut self) {
        self.translation_cache.clear();
        self.metrics.translation_cache_flushes =
            self.metrics.translation_cache_flushes.saturating_add(1);
    }

    fn refresh_hardware_interrupts(&mut self) {
        let mut hardware = 0;
        if self.msip {
            hardware |= MIP_MSIP;
        }
        if self.devices.mtime >= self.mtimecmp {
            hardware |= MIP_MTIP;
        }
        if self.firmware.enabled && hardware & MIP_MTIP != 0 {
            hardware = (hardware & !MIP_MTIP) | MIP_STIP;
        }
        let hardware_mask = MIP_MSIP | MIP_MTIP | if self.firmware.enabled { MIP_STIP } else { 0 };
        self.csrs.mip = (self.csrs.mip & !hardware_mask) | hardware;
    }

    fn apply_hart_control(&mut self, hart_id: usize) -> bool {
        let Some(control) = self.hart_controls.get(hart_id) else {
            return false;
        };
        let pending_start = control.pending_start();
        let pending_interrupts = control.take_interrupts();
        let pending_fence = control.pending_fence();

        let hart = &mut self.harts[hart_id];
        if let Some((start_address, opaque, generation)) = pending_start {
            hart.lifecycle = HartLifecycle::Started;
            hart.cpu.reset();
            hart.cpu.pc = start_address;
            hart.cpu.privilege = Privilege::Supervisor;
            hart.cpu.write(10, hart_id as u64);
            hart.cpu.write(11, opaque);
            hart.csrs.reset();
            hart.csrs.medeleg = MEDELEG_SUPPORTED;
            hart.csrs.mideleg = MIDELEG_SUPPORTED;
            hart.csrs.mcounteren = 0b111;
            hart.cycle = 0;
            hart.instret = 0;
            hart.reservation = None;
            #[cfg(target_arch = "wasm32")]
            {
                hart.reservation_token = None;
            }
            hart.mtimecmp = u64::MAX;
            hart.msip = false;
            hart.translation_cache.clear();
            control.acknowledge_start(generation);
        }
        hart.csrs.mip |= pending_interrupts;
        if let Some(generation) = pending_fence {
            hart.translation_cache.clear();
            self.metrics.translation_cache_flushes =
                self.metrics.translation_cache_flushes.saturating_add(1);
            control.acknowledge_fence(generation);
        }
        true
    }

    fn sbi_hart_start(&mut self, arguments: [u64; 6]) -> (u64, u64) {
        let Ok(hart_id) = usize::try_from(arguments[0]) else {
            return (SBI_ERR_INVALID_PARAM, 0);
        };
        if hart_id >= self.hart_count {
            return (SBI_ERR_INVALID_PARAM, 0);
        }
        if self.hart_lifecycle(hart_id) != Some(HartLifecycle::Stopped) {
            return (SBI_ERR_ALREADY_AVAILABLE, 0);
        }
        let start_address = arguments[1];
        if start_address & 1 != 0 || self.devices.ram_range(start_address, 2).is_none() {
            return (SBI_ERR_INVALID_ADDRESS, 0);
        }
        let control = &self.hart_controls[hart_id];
        if control.request_start(start_address, arguments[2]).is_err() {
            return (SBI_ERR_ALREADY_AVAILABLE, 0);
        }
        let applied = self.apply_hart_control(hart_id);
        debug_assert!(applied);
        (SBI_SUCCESS, 0)
    }

    fn sbi_hart_status(&self, hart_id: u64) -> (u64, u64) {
        let Ok(hart_id) = usize::try_from(hart_id) else {
            return (SBI_ERR_INVALID_PARAM, 0);
        };
        match self.hart_lifecycle(hart_id) {
            Some(HartLifecycle::Started) => (SBI_SUCCESS, SBI_HART_STATE_STARTED),
            Some(HartLifecycle::Stopped) => (SBI_SUCCESS, SBI_HART_STATE_STOPPED),
            None => (SBI_ERR_INVALID_PARAM, 0),
        }
    }

    fn selected_started_harts(
        &self,
        hart_mask: u64,
        hart_mask_base: u64,
    ) -> Result<Vec<usize>, u64> {
        if hart_mask_base == u64::MAX {
            return Ok((0..self.hart_count)
                .filter(|hart_id| self.hart_lifecycle(*hart_id) == Some(HartLifecycle::Started))
                .collect());
        }
        let Ok(base) = usize::try_from(hart_mask_base) else {
            return Err(SBI_ERR_INVALID_PARAM);
        };
        let mut selected = Vec::new();
        for bit in 0..u64::BITS {
            if hart_mask & (1_u64 << bit) == 0 {
                continue;
            }
            let Some(hart_id) = base.checked_add(bit as usize) else {
                return Err(SBI_ERR_INVALID_PARAM);
            };
            if self.hart_lifecycle(hart_id) != Some(HartLifecycle::Started) {
                return Err(SBI_ERR_INVALID_PARAM);
            }
            selected.push(hart_id);
        }
        Ok(selected)
    }

    fn sbi_send_ipi(&mut self, arguments: [u64; 6]) -> (u64, u64) {
        let selected = match self.selected_started_harts(arguments[0], arguments[1]) {
            Ok(selected) => selected,
            Err(error) => return (error, 0),
        };
        for hart_id in selected {
            self.hart_controls[hart_id].raise_interrupt(MIP_SSIP);
            let applied = self.apply_hart_control(hart_id);
            debug_assert!(applied);
        }
        (SBI_SUCCESS, 0)
    }

    fn sbi_remote_fence(&mut self, function: u64, arguments: [u64; 6]) -> (u64, u64) {
        if !matches!(function, 0..=2) {
            return (SBI_ERR_NOT_SUPPORTED, 0);
        }
        let selected = match self.selected_started_harts(arguments[0], arguments[1]) {
            Ok(selected) => selected,
            Err(error) => return (error, 0),
        };
        if function == 0 {
            return (SBI_SUCCESS, 0);
        }
        for hart_id in selected {
            self.hart_controls[hart_id].request_fence();
            let applied = self.apply_hart_control(hart_id);
            debug_assert!(applied);
        }
        (SBI_SUCCESS, 0)
    }

    fn handle_sbi_call(&mut self) -> Result<Option<HaltStatus>, MachineTrap> {
        let extension = self.cpu.read(17);
        let function = self.cpu.read(16);
        let arguments = [
            self.cpu.read(10),
            self.cpu.read(11),
            self.cpu.read(12),
            self.cpu.read(13),
            self.cpu.read(14),
            self.cpu.read(15),
        ];
        let mut halt = None;
        let response = match (extension, function) {
            (SBI_EXT_BASE, 0) => Some((SBI_SUCCESS, SBI_SPEC_VERSION_3_0)),
            (SBI_EXT_BASE, 1) => Some((SBI_SUCCESS, SBI_AOS_PRIVATE_IMPL_ID)),
            (SBI_EXT_BASE, 2) => Some((SBI_SUCCESS, 1)),
            (SBI_EXT_BASE, 3) => Some((
                SBI_SUCCESS,
                u64::from(matches!(
                    arguments[0],
                    SBI_EXT_BASE
                        | SBI_EXT_TIME
                        | SBI_EXT_IPI
                        | SBI_EXT_RFENCE
                        | SBI_EXT_HSM
                        | SBI_EXT_DBCN
                        | SBI_EXT_SRST
                        | SBI_EXT_AOS_9P
                )),
            )),
            (SBI_EXT_BASE, 4..=6) => Some((SBI_SUCCESS, 0)),
            (SBI_EXT_TIME, 0) => {
                self.mtimecmp = arguments[0];
                self.csrs.mip &= !MIP_STIP;
                Some((SBI_SUCCESS, 0))
            }
            (SBI_EXT_IPI, 0) => Some(self.sbi_send_ipi(arguments)),
            (SBI_EXT_RFENCE, function) => Some(self.sbi_remote_fence(function, arguments)),
            (SBI_EXT_HSM, 0) => Some(self.sbi_hart_start(arguments)),
            (SBI_EXT_HSM, 1) => {
                let stopped = self.set_hart_lifecycle(self.active_hart_id, HartLifecycle::Stopped);
                debug_assert!(stopped);
                None
            }
            (SBI_EXT_HSM, 2) => Some(self.sbi_hart_status(arguments[0])),
            (SBI_EXT_DBCN, 0) => Some(self.sbi_debug_console_write(arguments)?),
            (SBI_EXT_DBCN, 1) => Some(self.sbi_debug_console_read(arguments)),
            (SBI_EXT_DBCN, 2) => {
                self.devices.push_console_output(arguments[0] as u8)?;
                Some((SBI_SUCCESS, 0))
            }
            (SBI_EXT_SRST, 0) if arguments[0] <= 2 && arguments[1] <= 1 => {
                halt = Some(HaltStatus {
                    passed: arguments[0] == 0 && arguments[1] == 0,
                    code: ((arguments[0] as u32) << 16) | arguments[1] as u32,
                });
                Some((SBI_SUCCESS, 0))
            }
            (SBI_EXT_SRST, 0) => Some((SBI_ERR_INVALID_PARAM, 0)),
            (SBI_EXT_AOS_9P, 0) => self.sbi_9p_exchange(arguments),
            _ => Some((SBI_ERR_NOT_SUPPORTED, 0)),
        };
        if let Some((error, value)) = response {
            self.cpu.write(10, error);
            self.cpu.write(11, value);
        }
        Ok(halt)
    }

    fn sbi_9p_exchange(&mut self, arguments: [u64; 6]) -> Option<(u64, u64)> {
        if self.pending_9p_request.is_some() {
            return Some((SBI_ERR_ALREADY_AVAILABLE, 0));
        }
        let Ok(channel) = u32::try_from(arguments[4]) else {
            return Some((SBI_ERR_INVALID_PARAM, 0));
        };
        if channel == 0 || arguments[5] != 0 {
            return Some((SBI_ERR_INVALID_PARAM, 0));
        }
        let (Ok(request_bytes), Ok(response_bytes)) =
            (usize::try_from(arguments[1]), usize::try_from(arguments[3]))
        else {
            return Some((SBI_ERR_INVALID_PARAM, 0));
        };
        if !(MIN_9P_MESSAGE_BYTES..=MAX_9P_MESSAGE_BYTES).contains(&request_bytes)
            || !(MIN_9P_MESSAGE_BYTES..=MAX_9P_MESSAGE_BYTES).contains(&response_bytes)
        {
            return Some((SBI_ERR_INVALID_PARAM, 0));
        }
        let Some(request_range) = self.devices.ram_range_len(arguments[0], request_bytes) else {
            return Some((SBI_ERR_INVALID_ADDRESS, 0));
        };
        if self
            .devices
            .ram_range_len(arguments[2], response_bytes)
            .is_none()
        {
            return Some((SBI_ERR_INVALID_ADDRESS, 0));
        }
        let Some(next_id) = self.next_host_request_id.checked_add(1) else {
            return Some((SBI_ERR_FAILED, 0));
        };
        let id = HostRequestId(self.next_host_request_id);
        self.next_host_request_id = next_id;
        self.pending_9p_request = Some(PendingPlan9Request {
            request: Plan9Request {
                id,
                channel,
                message: self
                    .devices
                    .ram
                    .copy_to_vec(request_range)
                    .expect("request range validated"),
                max_response_bytes: response_bytes,
            },
            response_address: arguments[2],
            hart_id: self.active_hart_id,
        });
        None
    }

    fn sbi_debug_console_write(&mut self, arguments: [u64; 6]) -> Result<(u64, u64), MachineTrap> {
        let Ok(bytes) = usize::try_from(arguments[0]) else {
            return Ok((SBI_ERR_INVALID_ADDRESS, 0));
        };
        if arguments[2] != 0 {
            return Ok((SBI_ERR_INVALID_ADDRESS, 0));
        }
        let Some(range) = self.devices.ram_range_len(arguments[1], bytes) else {
            return Ok((SBI_ERR_INVALID_ADDRESS, 0));
        };
        let remaining = self
            .devices
            .max_console_bytes
            .saturating_sub(self.devices.console_output.len());
        let written = bytes.min(remaining);
        let output = self
            .devices
            .ram
            .copy_to_vec(range.start..range.start + written)
            .expect("console range validated");
        for byte in output {
            self.devices.push_console_output(byte)?;
        }
        Ok((SBI_SUCCESS, written as u64))
    }

    fn sbi_debug_console_read(&mut self, arguments: [u64; 6]) -> (u64, u64) {
        let Ok(bytes) = usize::try_from(arguments[0]) else {
            return (SBI_ERR_INVALID_ADDRESS, 0);
        };
        if arguments[2] != 0 {
            return (SBI_ERR_INVALID_ADDRESS, 0);
        }
        let Some(range) = self.devices.ram_range_len(arguments[1], bytes) else {
            return (SBI_ERR_INVALID_ADDRESS, 0);
        };
        let read = bytes.min(self.devices.console_input.len());
        for offset in 0..read {
            let byte = self
                .devices
                .console_input
                .pop_front()
                .expect("length checked console input");
            if !self.devices.ram.set_byte(range.start + offset, byte) {
                return (SBI_ERR_FAILED, offset as u64);
            }
        }
        (SBI_SUCCESS, read as u64)
    }

    fn take_pending_interrupt(&mut self) -> bool {
        let pending = self.csrs.mie & self.csrs.mip;
        for cause in [
            INTERRUPT_MACHINE_EXTERNAL,
            INTERRUPT_MACHINE_SOFTWARE,
            INTERRUPT_MACHINE_TIMER,
            INTERRUPT_SUPERVISOR_EXTERNAL,
            INTERRUPT_SUPERVISOR_SOFTWARE,
            INTERRUPT_SUPERVISOR_TIMER,
        ] {
            let mask = 1 << cause;
            if pending & mask == 0 {
                continue;
            }
            let delegated =
                self.cpu.privilege != Privilege::Machine && self.csrs.mideleg & mask != 0;
            let enabled = if delegated {
                self.cpu.privilege < Privilege::Supervisor
                    || self.cpu.privilege == Privilege::Supervisor
                        && self.csrs.mstatus & MSTATUS_SIE != 0
            } else {
                self.cpu.privilege < Privilege::Machine
                    || self.cpu.privilege == Privilege::Machine
                        && self.csrs.mstatus & MSTATUS_MIE != 0
            };
            if enabled {
                self.take_interrupt(cause, delegated);
                return true;
            }
        }
        false
    }

    fn take_interrupt(&mut self, cause: u64, delegated: bool) {
        let origin = self.cpu.privilege;
        let pc = self.cpu.pc;
        self.reservation = None;
        #[cfg(target_arch = "wasm32")]
        {
            self.reservation_token = None;
        }
        if delegated {
            self.csrs.sepc = pc & !0b1;
            self.csrs.scause = INTERRUPT_CAUSE_BIT | cause;
            self.csrs.stval = 0;
            self.csrs.mstatus = if origin == Privilege::User {
                self.csrs.mstatus & !MSTATUS_SPP
            } else {
                self.csrs.mstatus | MSTATUS_SPP
            };
            self.csrs.mstatus = if self.csrs.mstatus & MSTATUS_SIE == 0 {
                self.csrs.mstatus & !MSTATUS_SPIE
            } else {
                self.csrs.mstatus | MSTATUS_SPIE
            };
            self.csrs.mstatus &= !MSTATUS_SIE;
            self.cpu.privilege = Privilege::Supervisor;
            self.cpu.pc = interrupt_vector(self.csrs.stvec, cause);
        } else {
            self.csrs.mepc = pc & !0b1;
            self.csrs.mcause = INTERRUPT_CAUSE_BIT | cause;
            self.csrs.mtval = 0;
            self.csrs.mstatus =
                (self.csrs.mstatus & !MSTATUS_MPP) | ((origin as u64) << MSTATUS_MPP_SHIFT);
            self.csrs.mstatus = if self.csrs.mstatus & MSTATUS_MIE == 0 {
                self.csrs.mstatus & !MSTATUS_MPIE
            } else {
                self.csrs.mstatus | MSTATUS_MPIE
            };
            self.csrs.mstatus &= !MSTATUS_MIE;
            self.cpu.privilege = Privilege::Machine;
            self.cpu.pc = interrupt_vector(self.csrs.mtvec, cause);
        }
    }

    fn take_exception(&mut self, cause: u64, value: u64, pc: u64) {
        let origin = self.cpu.privilege;
        self.reservation = None;
        #[cfg(target_arch = "wasm32")]
        {
            self.reservation_token = None;
        }
        if origin != Privilege::Machine && self.csrs.medeleg & (1 << cause) != 0 {
            self.csrs.sepc = pc & !0b1;
            self.csrs.scause = cause;
            self.csrs.stval = value;
            self.csrs.mstatus = if origin == Privilege::User {
                self.csrs.mstatus & !MSTATUS_SPP
            } else {
                self.csrs.mstatus | MSTATUS_SPP
            };
            self.csrs.mstatus = if self.csrs.mstatus & MSTATUS_SIE == 0 {
                self.csrs.mstatus & !MSTATUS_SPIE
            } else {
                self.csrs.mstatus | MSTATUS_SPIE
            };
            self.csrs.mstatus &= !MSTATUS_SIE;
            self.cpu.privilege = Privilege::Supervisor;
            self.cpu.pc = self.csrs.stvec & !0b11;
        } else {
            self.csrs.mepc = pc & !0b1;
            self.csrs.mcause = cause;
            self.csrs.mtval = value;
            self.csrs.mstatus =
                (self.csrs.mstatus & !MSTATUS_MPP) | ((origin as u64) << MSTATUS_MPP_SHIFT);
            self.csrs.mstatus = if self.csrs.mstatus & MSTATUS_MIE == 0 {
                self.csrs.mstatus & !MSTATUS_MPIE
            } else {
                self.csrs.mstatus | MSTATUS_MPIE
            };
            self.csrs.mstatus &= !MSTATUS_MIE;
            self.cpu.privilege = Privilege::Machine;
            self.cpu.pc = self.csrs.mtvec & !0b11;
        }
    }

    fn execute_mret(&mut self, pc: u64, instruction: u32) -> Result<u64, MachineTrap> {
        if self.cpu.privilege != Privilege::Machine {
            return Err(illegal(pc, instruction));
        }
        let target = self.csrs.read(Csr::Mepc);
        ensure_instruction_aligned(target)?;
        let privilege =
            Privilege::from_mpp(self.csrs.mstatus).ok_or_else(|| illegal(pc, instruction))?;
        self.csrs.mstatus = if self.csrs.mstatus & MSTATUS_MPIE == 0 {
            self.csrs.mstatus & !MSTATUS_MIE
        } else {
            self.csrs.mstatus | MSTATUS_MIE
        };
        self.csrs.mstatus |= MSTATUS_MPIE;
        self.csrs.mstatus &= !MSTATUS_MPP;
        if privilege != Privilege::Machine {
            self.csrs.mstatus &= !MSTATUS_MPRV;
        }
        self.cpu.privilege = privilege;
        Ok(target)
    }

    fn execute_sret(&mut self, pc: u64, instruction: u32) -> Result<u64, MachineTrap> {
        if self.cpu.privilege < Privilege::Supervisor {
            return Err(illegal(pc, instruction));
        }
        let target = self.csrs.read(Csr::Sepc);
        ensure_instruction_aligned(target)?;
        let privilege = if self.csrs.mstatus & MSTATUS_SPP == 0 {
            Privilege::User
        } else {
            Privilege::Supervisor
        };
        self.csrs.mstatus = if self.csrs.mstatus & MSTATUS_SPIE == 0 {
            self.csrs.mstatus & !MSTATUS_SIE
        } else {
            self.csrs.mstatus | MSTATUS_SIE
        };
        self.csrs.mstatus |= MSTATUS_SPIE;
        self.csrs.mstatus &= !MSTATUS_SPP;
        if privilege != Privilege::Machine {
            self.csrs.mstatus &= !MSTATUS_MPRV;
        }
        self.cpu.privilege = privilege;
        Ok(target)
    }

    fn execute_op_imm(
        &mut self,
        instruction: u32,
        rd: usize,
        rs1: usize,
        funct3: u32,
        pc: u64,
    ) -> Result<(), MachineTrap> {
        let lhs = self.cpu.read(rs1);
        let immediate = immediate_i(instruction);
        let value = match funct3 {
            0 => lhs.wrapping_add(immediate),
            2 => u64::from((lhs as i64) < (immediate as i64)),
            3 => u64::from(lhs < immediate),
            4 => lhs ^ immediate,
            6 => lhs | immediate,
            7 => lhs & immediate,
            1 if instruction >> 26 == 0 => lhs.wrapping_shl((instruction >> 20) & 0x3f),
            5 if instruction >> 26 == 0 => lhs.wrapping_shr((instruction >> 20) & 0x3f),
            5 if instruction >> 26 == 0x10 => ((lhs as i64) >> ((instruction >> 20) & 0x3f)) as u64,
            _ => return Err(illegal(pc, instruction)),
        };
        self.cpu.write(rd, value);
        Ok(())
    }

    fn execute_op_imm_32(
        &mut self,
        instruction: u32,
        rd: usize,
        rs1: usize,
        funct3: u32,
        pc: u64,
    ) -> Result<(), MachineTrap> {
        let lhs = self.cpu.read(rs1) as u32;
        let value = match funct3 {
            0 => lhs.wrapping_add(immediate_i(instruction) as u32),
            1 if instruction >> 25 == 0 => lhs.wrapping_shl((instruction >> 20) & 0x1f),
            5 if instruction >> 25 == 0 => lhs.wrapping_shr((instruction >> 20) & 0x1f),
            5 if instruction >> 25 == 0x20 => ((lhs as i32) >> ((instruction >> 20) & 0x1f)) as u32,
            _ => return Err(illegal(pc, instruction)),
        };
        self.cpu.write(rd, sign_extend(u64::from(value), 32));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_op(
        &mut self,
        instruction: u32,
        rd: usize,
        rs1: usize,
        rs2: usize,
        funct3: u32,
        funct7: u32,
        pc: u64,
    ) -> Result<(), MachineTrap> {
        let lhs = self.cpu.read(rs1);
        let rhs = self.cpu.read(rs2);
        let value = match (funct7, funct3) {
            (0x00, 0) => lhs.wrapping_add(rhs),
            (0x20, 0) => lhs.wrapping_sub(rhs),
            (0x00, 1) => lhs.wrapping_shl((rhs & 0x3f) as u32),
            (0x00, 2) => u64::from((lhs as i64) < (rhs as i64)),
            (0x00, 3) => u64::from(lhs < rhs),
            (0x00, 4) => lhs ^ rhs,
            (0x00, 5) => lhs.wrapping_shr((rhs & 0x3f) as u32),
            (0x20, 5) => ((lhs as i64) >> (rhs & 0x3f)) as u64,
            (0x00, 6) => lhs | rhs,
            (0x00, 7) => lhs & rhs,
            (0x01, 0) => lhs.wrapping_mul(rhs),
            (0x01, 1) => (((lhs as i64 as i128) * (rhs as i64 as i128)) >> 64) as u64,
            (0x01, 2) => (((lhs as i64 as i128) * (rhs as i128)) >> 64) as u64,
            (0x01, 3) => ((u128::from(lhs) * u128::from(rhs)) >> 64) as u64,
            (0x01, 4) => signed_divide(lhs, rhs),
            (0x01, 5) => lhs.checked_div(rhs).unwrap_or(u64::MAX),
            (0x01, 6) => signed_remainder(lhs, rhs),
            (0x01, 7) => {
                if rhs == 0 {
                    lhs
                } else {
                    lhs % rhs
                }
            }
            _ => return Err(illegal(pc, instruction)),
        };
        self.cpu.write(rd, value);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_op_32(
        &mut self,
        instruction: u32,
        rd: usize,
        rs1: usize,
        rs2: usize,
        funct3: u32,
        funct7: u32,
        pc: u64,
    ) -> Result<(), MachineTrap> {
        let lhs = self.cpu.read(rs1) as u32;
        let rhs = self.cpu.read(rs2) as u32;
        let value = match (funct7, funct3) {
            (0x00, 0) => lhs.wrapping_add(rhs),
            (0x20, 0) => lhs.wrapping_sub(rhs),
            (0x00, 1) => lhs.wrapping_shl(rhs & 0x1f),
            (0x00, 5) => lhs.wrapping_shr(rhs & 0x1f),
            (0x20, 5) => ((lhs as i32) >> (rhs & 0x1f)) as u32,
            (0x01, 0) => lhs.wrapping_mul(rhs),
            (0x01, 4) => signed_divide_32(lhs, rhs),
            (0x01, 5) => lhs.checked_div(rhs).unwrap_or(u32::MAX),
            (0x01, 6) => signed_remainder_32(lhs, rhs),
            (0x01, 7) => {
                if rhs == 0 {
                    lhs
                } else {
                    lhs % rhs
                }
            }
            _ => return Err(illegal(pc, instruction)),
        };
        self.cpu.write(rd, sign_extend(u64::from(value), 32));
        Ok(())
    }
}

fn compressed_immediate(instruction: u16) -> u64 {
    let value = u64::from((instruction >> 2) & 0x1f) | (u64::from(instruction >> 12) << 5);
    sign_extend(value, 6)
}

fn compressed_shift(instruction: u16) -> u8 {
    (((instruction >> 2) & 0x1f) | (((instruction >> 12) & 1) << 5)) as u8
}

fn compressed_addi4spn_immediate(instruction: u16) -> u64 {
    (u64::from((instruction >> 7) & 0xf) << 6)
        | (u64::from((instruction >> 11) & 0x3) << 4)
        | (u64::from((instruction >> 5) & 1) << 3)
        | (u64::from((instruction >> 6) & 1) << 2)
}

fn compressed_word_immediate(instruction: u16) -> u64 {
    (u64::from((instruction >> 10) & 0x7) << 3)
        | (u64::from((instruction >> 6) & 1) << 2)
        | (u64::from((instruction >> 5) & 1) << 6)
}

fn compressed_double_immediate(instruction: u16) -> u64 {
    (u64::from((instruction >> 10) & 0x7) << 3) | (u64::from((instruction >> 5) & 0x3) << 6)
}

fn compressed_addi16sp_immediate(instruction: u16) -> u64 {
    let value = (u64::from((instruction >> 12) & 1) << 9)
        | (u64::from((instruction >> 6) & 1) << 4)
        | (u64::from((instruction >> 5) & 1) << 6)
        | (u64::from((instruction >> 3) & 0x3) << 7)
        | (u64::from((instruction >> 2) & 1) << 5);
    sign_extend(value, 10)
}

fn compressed_lui_immediate(instruction: u16) -> u64 {
    compressed_immediate(instruction).wrapping_shl(12)
}

fn compressed_jump_immediate(instruction: u16) -> u64 {
    let value = (u64::from((instruction >> 12) & 1) << 11)
        | (u64::from((instruction >> 11) & 1) << 4)
        | (u64::from((instruction >> 9) & 0x3) << 8)
        | (u64::from((instruction >> 8) & 1) << 10)
        | (u64::from((instruction >> 7) & 1) << 6)
        | (u64::from((instruction >> 6) & 1) << 7)
        | (u64::from((instruction >> 3) & 0x7) << 1)
        | (u64::from((instruction >> 2) & 1) << 5);
    sign_extend(value, 12)
}

fn compressed_branch_immediate(instruction: u16) -> u64 {
    let value = (u64::from((instruction >> 12) & 1) << 8)
        | (u64::from((instruction >> 10) & 0x3) << 3)
        | (u64::from((instruction >> 5) & 0x3) << 6)
        | (u64::from((instruction >> 3) & 0x3) << 1)
        | (u64::from((instruction >> 2) & 1) << 5);
    sign_extend(value, 9)
}

fn compressed_word_sp_immediate(instruction: u16) -> u64 {
    (u64::from((instruction >> 12) & 1) << 5)
        | (u64::from((instruction >> 4) & 0x7) << 2)
        | (u64::from((instruction >> 2) & 0x3) << 6)
}

fn compressed_double_sp_immediate(instruction: u16) -> u64 {
    (u64::from((instruction >> 12) & 1) << 5)
        | (u64::from((instruction >> 5) & 0x3) << 3)
        | (u64::from((instruction >> 2) & 0x7) << 6)
}

fn compressed_word_store_sp_immediate(instruction: u16) -> u64 {
    (u64::from((instruction >> 9) & 0xf) << 2) | (u64::from((instruction >> 7) & 0x3) << 6)
}

fn compressed_double_store_sp_immediate(instruction: u16) -> u64 {
    (u64::from((instruction >> 10) & 0x7) << 3) | (u64::from((instruction >> 7) & 0x7) << 6)
}

fn ensure_aligned(address: u64, bytes: u8, store: bool) -> Result<(), MachineTrap> {
    if address.is_multiple_of(u64::from(bytes)) {
        return Ok(());
    }
    if store {
        Err(MachineTrap::StoreAddressMisaligned { address, bytes })
    } else {
        Err(MachineTrap::LoadAddressMisaligned { address, bytes })
    }
}

fn ensure_instruction_aligned(address: u64) -> Result<(), MachineTrap> {
    if address.is_multiple_of(2) {
        Ok(())
    } else {
        Err(MachineTrap::InstructionAddressMisaligned { address })
    }
}

fn illegal(pc: u64, instruction: u32) -> MachineTrap {
    MachineTrap::IllegalInstruction { pc, instruction }
}

fn architectural_exception(trap: &MachineTrap) -> Option<(u64, u64)> {
    Some(match *trap {
        MachineTrap::InstructionAddressMisaligned { address } => (0, address),
        MachineTrap::InstructionAccessFault { address } => (1, address),
        MachineTrap::IllegalInstruction { instruction, .. } => (2, u64::from(instruction)),
        MachineTrap::Breakpoint { .. } => (3, 0),
        MachineTrap::LoadAddressMisaligned { address, .. } => (4, address),
        MachineTrap::LoadAccessFault { address, .. } => (5, address),
        MachineTrap::StoreAddressMisaligned { address, .. } => (6, address),
        MachineTrap::StoreAccessFault { address, .. } => (7, address),
        MachineTrap::InstructionPageFault { address } => (CAUSE_INSTRUCTION_PAGE_FAULT, address),
        MachineTrap::LoadPageFault { address } => (CAUSE_LOAD_PAGE_FAULT, address),
        MachineTrap::StorePageFault { address } => (CAUSE_STORE_PAGE_FAULT, address),
        MachineTrap::ConsoleLimit { .. } => return None,
    })
}

const fn interrupt_vector(vector: u64, cause: u64) -> u64 {
    let base = vector & !0b11;
    if vector & 0b11 == 1 {
        base.wrapping_add(cause * 4)
    } else {
        base
    }
}

fn map_store_fault(trap: MachineTrap, address: u64, bytes: u8) -> MachineTrap {
    match trap {
        MachineTrap::ConsoleLimit { .. } => trap,
        _ => MachineTrap::StoreAccessFault { address, bytes },
    }
}

fn atomic_result(operation: u32, old: u64, rhs: u64, bytes: u8) -> Option<u64> {
    if bytes == 4 {
        let old = old as u32;
        let rhs = rhs as u32;
        Some(u64::from(match operation {
            0x00 => old.wrapping_add(rhs),
            0x01 => rhs,
            0x04 => old ^ rhs,
            0x08 => old | rhs,
            0x0c => old & rhs,
            0x10 => u32::from_ne_bytes((old as i32).min(rhs as i32).to_ne_bytes()),
            0x14 => u32::from_ne_bytes((old as i32).max(rhs as i32).to_ne_bytes()),
            0x18 => old.min(rhs),
            0x1c => old.max(rhs),
            _ => return None,
        }))
    } else {
        Some(match operation {
            0x00 => old.wrapping_add(rhs),
            0x01 => rhs,
            0x04 => old ^ rhs,
            0x08 => old | rhs,
            0x0c => old & rhs,
            0x10 => (old as i64).min(rhs as i64) as u64,
            0x14 => (old as i64).max(rhs as i64) as u64,
            0x18 => old.min(rhs),
            0x1c => old.max(rhs),
            _ => return None,
        })
    }
}

fn signed_divide(lhs: u64, rhs: u64) -> u64 {
    let lhs = lhs as i64;
    let rhs = rhs as i64;
    if rhs == 0 {
        u64::MAX
    } else if lhs == i64::MIN && rhs == -1 {
        lhs as u64
    } else {
        (lhs / rhs) as u64
    }
}

fn signed_remainder(lhs: u64, rhs: u64) -> u64 {
    let lhs = lhs as i64;
    let rhs = rhs as i64;
    if rhs == 0 {
        lhs as u64
    } else if lhs == i64::MIN && rhs == -1 {
        0
    } else {
        (lhs % rhs) as u64
    }
}

fn signed_divide_32(lhs: u32, rhs: u32) -> u32 {
    let lhs = lhs as i32;
    let rhs = rhs as i32;
    if rhs == 0 {
        u32::MAX
    } else if lhs == i32::MIN && rhs == -1 {
        lhs as u32
    } else {
        (lhs / rhs) as u32
    }
}

fn signed_remainder_32(lhs: u32, rhs: u32) -> u32 {
    let lhs = lhs as i32;
    let rhs = rhs as i32;
    if rhs == 0 {
        lhs as u32
    } else if lhs == i32::MIN && rhs == -1 {
        0
    } else {
        (lhs % rhs) as u32
    }
}

const fn is_sv39_canonical(address: u64) -> bool {
    let sign = (address >> 38) & 1;
    let upper = address >> 39;
    if sign == 0 {
        upper == 0
    } else {
        upper == (1 << 25) - 1
    }
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
}

const fn ecall_cause(privilege: Privilege) -> u64 {
    match privilege {
        Privilege::User => CAUSE_ECALL_FROM_USER,
        Privilege::Supervisor => CAUSE_ECALL_FROM_SUPERVISOR,
        Privilege::Machine => CAUSE_ECALL_FROM_MACHINE,
    }
}

fn sign_extend(value: u64, bits: u32) -> u64 {
    let shift = 64 - bits;
    ((value << shift) as i64 >> shift) as u64
}

fn immediate_i(instruction: u32) -> u64 {
    sign_extend(u64::from(instruction >> 20), 12)
}

fn immediate_s(instruction: u32) -> u64 {
    let value = ((instruction >> 7) & 0x1f) | (((instruction >> 25) & 0x7f) << 5);
    sign_extend(u64::from(value), 12)
}

fn immediate_b(instruction: u32) -> u64 {
    let value = (((instruction >> 8) & 0x0f) << 1)
        | (((instruction >> 25) & 0x3f) << 5)
        | (((instruction >> 7) & 1) << 11)
        | (((instruction >> 31) & 1) << 12);
    sign_extend(u64::from(value), 13)
}

fn immediate_u(instruction: u32) -> u64 {
    sign_extend(u64::from(instruction & 0xffff_f000), 32)
}

fn immediate_j(instruction: u32) -> u64 {
    let value = (((instruction >> 21) & 0x03ff) << 1)
        | (((instruction >> 20) & 1) << 11)
        | (((instruction >> 12) & 0xff) << 12)
        | (((instruction >> 31) & 1) << 20);
    sign_extend(u64::from(value), 21)
}

const fn encode_lui(rd: u32, immediate: u32) -> u32 {
    (immediate << 12) | (rd << 7) | 0x37
}

const fn encode_auipc(rd: u32, immediate: u32) -> u32 {
    (immediate << 12) | (rd << 7) | 0x17
}

const fn encode_addi(rd: u32, rs1: u32, immediate: u32) -> u32 {
    ((immediate & 0x0fff) << 20) | (rs1 << 15) | (rd << 7) | 0x13
}

const fn encode_slli(rd: u32, rs1: u32, shift: u32) -> u32 {
    ((shift & 0x3f) << 20) | (rs1 << 15) | (1 << 12) | (rd << 7) | 0x13
}

const fn encode_csr(rd: u32, csr: Csr, rs1: u32, funct3: u32) -> u32 {
    ((csr.address() as u32) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x73
}

const fn encode_csrr(rd: u32, csr: Csr) -> u32 {
    encode_csr(rd, csr, 0, 2)
}

const fn encode_csrw(csr: Csr, rs1: u32) -> u32 {
    encode_csr(0, csr, rs1, 1)
}

const fn encode_store(rs1: u32, rs2: u32, immediate: u32, funct3: u32) -> u32 {
    (((immediate >> 5) & 0x7f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | ((immediate & 0x1f) << 7)
        | 0x23
}

const fn encode_putc(byte: u8) -> [u32; 2] {
    [encode_addi(6, 0, byte as u32), encode_store(5, 6, 0, 0)]
}

const SMOKE_WORDS: [u32; 23] = {
    let a = encode_putc(b'A');
    let o = encode_putc(b'O');
    let s = encode_putc(b'S');
    let space = encode_putc(b' ');
    let r = encode_putc(b'R');
    let v = encode_putc(b'V');
    let six = encode_putc(b'6');
    let four = encode_putc(b'4');
    let newline = encode_putc(b'\n');
    [
        encode_lui(5, 0x1_0000),
        a[0],
        a[1],
        o[0],
        o[1],
        s[0],
        s[1],
        space[0],
        space[1],
        r[0],
        r[1],
        v[0],
        v[1],
        six[0],
        six[1],
        four[0],
        four[1],
        newline[0],
        newline[1],
        encode_lui(7, 0x100),
        encode_lui(8, 0x5),
        encode_addi(8, 8, 0x555),
        encode_store(7, 8, 0, 2),
    ]
};

const fn words_to_smoke_bytes(words: [u32; 23]) -> [u8; 92] {
    let mut bytes = [0_u8; 92];
    let mut word = 0;
    while word < words.len() {
        let encoded = words[word].to_le_bytes();
        bytes[word * 4] = encoded[0];
        bytes[word * 4 + 1] = encoded[1];
        bytes[word * 4 + 2] = encoded[2];
        bytes[word * 4 + 3] = encoded[3];
        word += 1;
    }
    bytes
}

/// Auditable RV64I probe that prints `AOS RV64` and halts through the standard
/// finisher. It proves the ISA/device/scheduling path without claiming Linux.
pub const RV64_SMOKE_PROGRAM: [u8; 92] = words_to_smoke_bytes(SMOKE_WORDS);

const SUPERVISOR_ENTRY_OFFSET: u32 = 12 * 4;
const SUPERVISOR_HANDLER_OFFSET: u32 = 24 * 4;

// Reset firmware (words 0..12) installs the S-mode vector and ECALL delegation,
// writes MPP=S, then enters the supervisor payload with MRET. The payload
// (12..24) prints S, raises ECALL, resumes to print R and a newline, then halts.
// Its delegated handler (24..31) prints T, advances sepc, and returns with SRET.
const SUPERVISOR_WORDS: [u32; 31] = [
    encode_auipc(5, 0),
    encode_addi(6, 5, SUPERVISOR_HANDLER_OFFSET),
    encode_csrw(Csr::Stvec, 6),
    encode_addi(7, 0, 1),
    encode_slli(7, 7, CAUSE_ECALL_FROM_SUPERVISOR as u32),
    encode_csrw(Csr::Medeleg, 7),
    encode_addi(28, 5, SUPERVISOR_ENTRY_OFFSET),
    encode_csrw(Csr::Mepc, 28),
    encode_lui(29, 1),
    encode_addi(29, 29, 0x800),
    encode_csrw(Csr::Mstatus, 29),
    0x3020_0073,
    encode_lui(5, 0x1_0000),
    encode_addi(6, 0, b'S' as u32),
    encode_store(5, 6, 0, 0),
    0x0000_0073,
    encode_addi(6, 0, b'R' as u32),
    encode_store(5, 6, 0, 0),
    encode_addi(6, 0, b'\n' as u32),
    encode_store(5, 6, 0, 0),
    encode_lui(7, 0x100),
    encode_lui(8, 0x5),
    encode_addi(8, 8, 0x555),
    encode_store(7, 8, 0, 2),
    encode_lui(5, 0x1_0000),
    encode_addi(6, 0, b'T' as u32),
    encode_store(5, 6, 0, 0),
    encode_csrr(7, Csr::Sepc),
    encode_addi(7, 7, 4),
    encode_csrw(Csr::Sepc, 7),
    0x1020_0073,
];

const fn words_to_supervisor_bytes(words: [u32; 31]) -> [u8; 124] {
    let mut bytes = [0_u8; 124];
    let mut word = 0;
    while word < words.len() {
        let encoded = words[word].to_le_bytes();
        bytes[word * 4] = encoded[0];
        bytes[word * 4 + 1] = encoded[1];
        bytes[word * 4 + 2] = encoded[2];
        bytes[word * 4 + 3] = encoded[3];
        word += 1;
    }
    bytes
}

/// Auditable M-to-S transition probe. It takes a delegated supervisor ECALL,
/// returns with `sret`, prints `STR\n`, and halts from Supervisor mode.
pub const RV64_SUPERVISOR_PROGRAM: [u8; 124] = words_to_supervisor_bytes(SUPERVISOR_WORDS);

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(console_bytes: usize) -> Machine {
        Machine::new(MachineConfig {
            ram_bytes: 4096,
            max_console_bytes: console_bytes,
        })
        .expect("valid test machine")
    }

    fn words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    fn halfwords(halfwords: &[u16]) -> Vec<u8> {
        halfwords
            .iter()
            .flat_map(|halfword| halfword.to_le_bytes())
            .collect()
    }

    #[test]
    fn admitted_max_ram_is_demand_zero_and_exact_across_chunks() {
        let mut ram = GuestRam::zeroed(MAX_RAM_BYTES).expect("admit maximum guest RAM");
        assert_eq!(ram.len(), MAX_RAM_BYTES);
        assert_eq!(ram.chunks.len(), MAX_RAM_BYTES / RAM_CHUNK_BYTES);
        assert!(ram.chunks.iter().all(Option::is_none));
        assert_eq!(ram.byte(0), Some(0));
        assert_eq!(ram.byte(MAX_RAM_BYTES - 1), Some(0));
        assert!(ram.nonzero_pages(4096).is_empty());

        let start = RAM_CHUNK_BYTES - 2;
        assert!(ram.copy_from_slice(start, &[1, 2, 3, 4]));
        assert_eq!(ram.chunks.iter().filter(|chunk| chunk.is_some()).count(), 2);
        assert_eq!(ram.copy_to_vec(start..start + 4), Some(vec![1, 2, 3, 4]));
        let populated = ram.nonzero_pages(4096);
        assert_eq!(
            populated
                .iter()
                .map(|(index, _)| *index)
                .collect::<Vec<_>>(),
            vec![511, 512]
        );

        ram.fill_zero();
        assert!(ram.chunks.iter().all(Option::is_none));
        assert_eq!(ram.copy_to_vec(start..start + 4), Some(vec![0; 4]));
        assert!(!ram.copy_from_slice(MAX_RAM_BYTES - 1, &[1, 2]));
        assert_eq!(ram.byte(MAX_RAM_BYTES), None);
    }

    fn paged_machine() -> Machine {
        let mut machine = Machine::new(MachineConfig {
            ram_bytes: 64 * 1024,
            max_console_bytes: 64,
        })
        .expect("valid paged machine");
        machine
            .load_program(&[0, 0, 0, 0])
            .expect("load reset marker");
        machine
    }

    fn install_4k_mapping(
        machine: &mut Machine,
        virtual_address: u64,
        physical: u64,
        flags: u64,
    ) -> u64 {
        let root = DRAM_BASE + 0x1000;
        let level_one = DRAM_BASE + 0x2000;
        let level_zero = DRAM_BASE + 0x3000;
        let vpn = [
            virtual_address >> 12 & 0x1ff,
            virtual_address >> 21 & 0x1ff,
            virtual_address >> 30 & 0x1ff,
        ];
        assert!(machine.devices.write_ram(
            root + vpn[2] * 8,
            ((level_one >> 12) << 10) | PTE_VALID,
            8,
        ));
        assert!(machine.devices.write_ram(
            level_one + vpn[1] * 8,
            ((level_zero >> 12) << 10) | PTE_VALID,
            8,
        ));
        let leaf_address = level_zero + vpn[0] * 8;
        assert!(
            machine
                .devices
                .write_ram(leaf_address, ((physical >> 12) << 10) | flags, 8,)
        );
        machine.csrs.write(
            Csr::Satp,
            (SATP_MODE_SV39 << SATP_MODE_SHIFT) | (root >> 12),
        );
        leaf_address
    }

    const fn encode_load(rd: u32, rs1: u32, immediate: u32, funct3: u32) -> u32 {
        ((immediate & 0xfff) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x03
    }

    const fn encode_op(rd: u32, rs1: u32, rs2: u32, funct3: u32, funct7: u32) -> u32 {
        (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x33
    }

    const fn encode_op_32(rd: u32, rs1: u32, rs2: u32, funct3: u32, funct7: u32) -> u32 {
        (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x3b
    }

    const fn encode_atomic(rd: u32, rs1: u32, rs2: u32, funct3: u32, operation: u32) -> u32 {
        (operation << 27) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x2f
    }

    const fn encode_float_load(rd: u32, rs1: u32, immediate: u32, funct3: u32) -> u32 {
        ((immediate & 0xfff) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x07
    }

    const fn encode_float_store(rs1: u32, rs2: u32, immediate: u32, funct3: u32) -> u32 {
        (((immediate >> 5) & 0x7f) << 25)
            | (rs2 << 20)
            | (rs1 << 15)
            | (funct3 << 12)
            | ((immediate & 0x1f) << 7)
            | 0x27
    }

    const fn encode_float_op(rd: u32, rs1: u32, rs2: u32, funct3: u32, funct7: u32) -> u32 {
        (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x53
    }

    const fn encode_float_fused(
        opcode: u32,
        rd: u32,
        rs1: u32,
        rs2: u32,
        rs3: u32,
        format: u32,
        rounding: u32,
    ) -> u32 {
        (rs3 << 27)
            | (format << 25)
            | (rs2 << 20)
            | (rs1 << 15)
            | (rounding << 12)
            | (rd << 7)
            | opcode
    }

    #[test]
    fn smoke_program_runs_in_slices_and_halts_exactly() {
        let mut machine = machine(64);
        machine
            .load_program(&RV64_SMOKE_PROGRAM)
            .expect("load smoke program");

        let first = machine.run_slice(3);
        assert_eq!(first.outcome, SliceOutcome::Yielded);
        assert_eq!(first.steps_executed, 3);
        assert_eq!(first.instructions_retired, 3);
        assert_eq!(first.console, b"A");

        let final_report = machine.run_slice(64);
        assert_eq!(
            final_report.outcome,
            SliceOutcome::Halted(HaltStatus {
                passed: true,
                code: 0,
            })
        );
        assert_eq!(final_report.console, b"OS RV64\n");
        assert_eq!(final_report.total_steps_executed, 23);
        assert_eq!(final_report.total_instructions_retired, 23);

        let repeated = machine.run_slice(64);
        assert_eq!(repeated.outcome, final_report.outcome);
        assert_eq!(repeated.instructions_retired, 0);
        assert!(repeated.console.is_empty());

        assert_eq!(
            machine.metrics(),
            MachineMetrics {
                instruction_fetches: 23,
                instruction_translations: 23,
                store_translations: 10,
                ..MachineMetrics::default()
            }
        );
        assert_eq!(machine.metrics().translations(), 33);
        machine
            .load_program(&RV64_SMOKE_PROGRAM)
            .expect("reload smoke program");
        assert_eq!(machine.metrics(), MachineMetrics::default());
    }

    #[test]
    fn supervisor_probe_delegates_ecall_and_returns_without_retiring_it() {
        let mut machine = machine(64);
        machine
            .load_program(&RV64_SUPERVISOR_PROGRAM)
            .expect("load supervisor program");

        let trapped = machine.run_slice(16);
        assert_eq!(trapped.outcome, SliceOutcome::Yielded);
        assert_eq!(trapped.steps_executed, 16);
        assert_eq!(trapped.instructions_retired, 15);
        assert_eq!(trapped.console, b"S");
        assert_eq!(machine.privilege(), Privilege::Supervisor);
        assert_eq!(
            machine.pc(),
            DRAM_BASE + u64::from(SUPERVISOR_HANDLER_OFFSET)
        );
        assert_eq!(machine.csr(Csr::Scause), CAUSE_ECALL_FROM_SUPERVISOR);
        assert_eq!(machine.csr(Csr::Sepc), DRAM_BASE + 15 * 4);
        assert_eq!(machine.csr(Csr::Stval), 0);
        assert_eq!(machine.csr(Csr::Medeleg), 1 << CAUSE_ECALL_FROM_SUPERVISOR);
        assert_eq!(
            machine.csr(Csr::Mepc),
            DRAM_BASE + SUPERVISOR_ENTRY_OFFSET as u64
        );
        assert_ne!(machine.csr(Csr::Sstatus) & MSTATUS_SPP, 0);

        let halted = machine.run_slice(64);
        assert_eq!(
            halted.outcome,
            SliceOutcome::Halted(HaltStatus {
                passed: true,
                code: 0,
            })
        );
        assert_eq!(halted.console, b"TR\n");
        assert_eq!(halted.total_steps_executed, 31);
        assert_eq!(halted.total_instructions_retired, 30);
        assert_eq!(machine.privilege(), Privilege::Supervisor);
        assert_eq!(machine.csr(Csr::Sepc), DRAM_BASE + 16 * 4);
        assert_eq!(machine.csr(Csr::Sstatus) & MSTATUS_SPP, 0);
        assert_ne!(machine.csr(Csr::Sstatus) & MSTATUS_SPIE, 0);
    }

    #[test]
    fn csr_access_checks_privilege_and_read_only_write_intent() {
        let mut rv = machine(8);
        let program = words(&[encode_csrr(5, Csr::Mhartid), encode_csrw(Csr::Mhartid, 5)]);
        rv.load_program(&program).expect("load program");

        let report = rv.run_slice(2);
        assert_eq!(rv.register(5), Some(0));
        assert_eq!(report.instructions_retired, 1);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(rv.csr(Csr::Mcause), 2);
        assert_eq!(rv.csr(Csr::Mepc), DRAM_BASE + 4);
        assert_eq!(rv.csr(Csr::Mtval), u64::from(encode_csrw(Csr::Mhartid, 5)));

        let mut supervisor = machine(8);
        supervisor
            .load_program(&RV64_SUPERVISOR_PROGRAM)
            .expect("load supervisor program");
        let entered = supervisor.run_slice(12);
        assert_eq!(entered.outcome, SliceOutcome::Yielded);
        assert_eq!(supervisor.privilege(), Privilege::Supervisor);
        let forbidden = encode_csrw(Csr::Mstatus, 0);
        let offset = usize::try_from(supervisor.pc() - DRAM_BASE).expect("RAM offset");
        assert!(
            supervisor
                .devices
                .ram
                .copy_from_slice(offset, &forbidden.to_le_bytes())
        );
        let report = supervisor.run_slice(1);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 0);
        assert_eq!(supervisor.privilege(), Privilege::Machine);
        assert_eq!(supervisor.csr(Csr::Mcause), 2);
        assert_eq!(
            supervisor.csr(Csr::Mepc),
            DRAM_BASE + u64::from(SUPERVISOR_ENTRY_OFFSET)
        );
        assert_eq!(supervisor.csr(Csr::Mtval), u64::from(forbidden));
    }

    #[test]
    fn all_six_zicsr_operations_use_old_values_and_exact_write_intent() {
        let mut rv = machine(8);
        let program = words(&[
            encode_addi(5, 0, 0x0f),
            encode_csr(6, Csr::Mscratch, 5, 1),
            encode_addi(7, 0, 0x30),
            encode_csr(8, Csr::Mscratch, 7, 2),
            encode_addi(9, 0, 0x0c),
            encode_csr(10, Csr::Mscratch, 9, 3),
            encode_csr(11, Csr::Mscratch, 5, 5),
            encode_csr(12, Csr::Mscratch, 2, 6),
            encode_csr(13, Csr::Mscratch, 1, 7),
            encode_csrr(14, Csr::Mscratch),
            0x0010_0073,
        ]);
        rv.load_program(&program).expect("load CSR program");

        let report = rv.run_slice(11);
        assert_eq!(report.instructions_retired, 10);
        assert_eq!(rv.register(6), Some(0));
        assert_eq!(rv.register(8), Some(0x0f));
        assert_eq!(rv.register(10), Some(0x3f));
        assert_eq!(rv.register(11), Some(0x33));
        assert_eq!(rv.register(12), Some(5));
        assert_eq!(rv.register(13), Some(7));
        assert_eq!(rv.register(14), Some(6));
        assert_eq!(rv.csr(Csr::Mscratch), 6);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(rv.csr(Csr::Mcause), 3);
        assert_eq!(rv.csr(Csr::Mepc), DRAM_BASE + 10 * 4);
    }

    #[test]
    fn xret_instructions_fail_closed_below_their_privilege() {
        let mut supervisor = machine(8);
        supervisor
            .load_program(&RV64_SUPERVISOR_PROGRAM)
            .expect("load supervisor program");
        assert_eq!(supervisor.run_slice(12).outcome, SliceOutcome::Yielded);
        let mret = 0x3020_0073_u32;
        let offset = usize::try_from(supervisor.pc() - DRAM_BASE).expect("RAM offset");
        assert!(
            supervisor
                .devices
                .ram
                .copy_from_slice(offset, &mret.to_le_bytes())
        );
        assert_eq!(supervisor.run_slice(1).outcome, SliceOutcome::Yielded);
        assert_eq!(supervisor.privilege(), Privilege::Machine);
        assert_eq!(supervisor.csr(Csr::Mcause), 2);
        assert_eq!(supervisor.csr(Csr::Mtval), u64::from(mret));

        let mut user = machine(8);
        let program = words(&[
            encode_auipc(5, 0),
            encode_addi(5, 5, 5 * 4),
            encode_csrw(Csr::Mepc, 5),
            encode_csrw(Csr::Mstatus, 0),
            0x3020_0073,
            0x1020_0073,
        ]);
        user.load_program(&program).expect("load user transition");
        let report = user.run_slice(6);
        assert_eq!(user.privilege(), Privilege::Machine);
        assert_eq!(report.instructions_retired, 5);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(user.csr(Csr::Mcause), 2);
        assert_eq!(user.csr(Csr::Mepc), DRAM_BASE + 5 * 4);
    }

    #[test]
    fn reserved_mpp_encoding_is_warl_coerced_to_user() {
        let mut rv = machine(8);
        let program = words(&[
            encode_lui(5, 1),
            encode_csrw(Csr::Mstatus, 5),
            encode_csrr(6, Csr::Mstatus),
            0x0010_0073,
        ]);
        rv.load_program(&program).expect("load WARL program");
        let report = rv.run_slice(3);

        assert_eq!(report.instructions_retired, 3);
        assert_eq!(rv.register(6).expect("x6") & MSTATUS_MPP, 0);
        assert_eq!(rv.csr(Csr::Mstatus) & MSTATUS_MPP, 0);
    }

    #[test]
    fn sret_to_lower_privilege_clears_mprv() {
        let mut rv = machine(8);
        let program = words(&[
            encode_auipc(5, 0),
            encode_addi(5, 5, 7 * 4),
            encode_csrw(Csr::Sepc, 5),
            encode_lui(6, 0x20),
            encode_addi(6, 6, MSTATUS_SPP as u32),
            encode_csrw(Csr::Mstatus, 6),
            0x1020_0073,
            0x0010_0073,
        ]);
        rv.load_program(&program).expect("load SRET program");
        let report = rv.run_slice(7);

        assert_eq!(rv.privilege(), Privilege::Supervisor);
        assert_eq!(rv.csr(Csr::Mstatus) & MSTATUS_MPRV, 0);
        assert_eq!(report.instructions_retired, 7);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
    }

    #[test]
    fn repeated_architectural_traps_remain_slice_bounded() {
        let mut machine = machine(8);
        machine
            .load_program(&0x0000_0073_u32.to_le_bytes())
            .expect("load ecall loop");
        machine.csrs.mtvec = DRAM_BASE;

        let report = machine.run_slice(7);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.steps_executed, 7);
        assert_eq!(report.instructions_retired, 0);
        assert_eq!(machine.pc(), DRAM_BASE);
        assert_eq!(machine.csr(Csr::Mcause), CAUSE_ECALL_FROM_MACHINE);
        assert_eq!(machine.csr(Csr::Mepc), DRAM_BASE);
    }

    #[test]
    fn zero_register_cannot_be_modified() {
        let mut machine = machine(8);
        let program = words(&[encode_addi(0, 0, 42), 0x0010_0073]);
        machine.load_program(&program).expect("load program");

        let report = machine.run_slice(2);
        assert_eq!(machine.register(0), Some(0));
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 1);
        assert_eq!(machine.csr(Csr::Mcause), 3);
        assert_eq!(machine.csr(Csr::Mepc), DRAM_BASE + 4);
    }

    #[test]
    fn invalid_instruction_traps_without_retiring() {
        let mut machine = machine(8);
        machine
            .load_program(&0xffff_ffff_u32.to_le_bytes())
            .expect("load program");

        let report = machine.run_slice(1);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 0);
        assert_eq!(machine.csr(Csr::Mcause), 2);
        assert_eq!(machine.csr(Csr::Mepc), DRAM_BASE);
        assert_eq!(machine.csr(Csr::Mtval), 0xffff_ffff);
    }

    #[test]
    fn ialign_16_accepts_halfword_jump_targets_and_rejects_odd_fetches() {
        let mut machine = machine(8);
        machine
            .load_program(&0x0020_00ef_u32.to_le_bytes())
            .expect("load program");

        let report = machine.run_slice(1);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 1);
        assert_eq!(machine.register(1), Some(DRAM_BASE + 4));
        assert_eq!(machine.pc(), DRAM_BASE + 2);

        machine.cpu.pc = DRAM_BASE + 1;
        let odd = machine.run_slice(1);
        assert_eq!(odd.instructions_retired, 0);
        assert_eq!(machine.csr(Csr::Mcause), 0);
        assert_eq!(machine.csr(Csr::Mtval), DRAM_BASE + 1);
    }

    #[test]
    fn console_limit_is_a_guest_trap_not_an_outer_allocation() {
        let mut machine = machine(1);
        machine
            .load_program(&RV64_SMOKE_PROGRAM)
            .expect("load smoke program");

        let report = machine.run_slice(16);
        assert_eq!(
            report.outcome,
            SliceOutcome::Trapped(MachineTrap::ConsoleLimit { limit: 1 })
        );
        assert_eq!(report.console, b"A");
        assert_eq!(report.instructions_retired, 4);
    }

    #[test]
    fn misaligned_store_is_rejected_before_memory_access() {
        let mut machine = machine(8);
        let program = words(&[
            (5 << 7) | 0x17, // auipc x5, 0: current DRAM address
            encode_addi(5, 5, 1),
            encode_addi(6, 0, 42),
            encode_store(5, 6, 0, 2),
        ]);
        machine.load_program(&program).expect("load program");

        let report = machine.run_slice(4);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 3);
        assert_eq!(machine.csr(Csr::Mcause), 6);
        assert_eq!(machine.csr(Csr::Mtval), DRAM_BASE + 1);
    }

    #[test]
    fn image_and_resource_admission_fail_closed() {
        assert_eq!(
            Machine::new(MachineConfig {
                ram_bytes: 4095,
                max_console_bytes: 1,
            })
            .expect_err("unaligned RAM must fail"),
            MachineError::InvalidRamBytes(4095)
        );
        let config = MachineConfig {
            ram_bytes: 4096,
            max_console_bytes: 1,
        };
        assert_eq!(
            Machine::new_with_harts(config, 0).expect_err("zero harts must fail"),
            MachineError::InvalidHartCount(0)
        );
        assert_eq!(
            Machine::new_with_harts(config, MAX_HARTS + 1)
                .expect_err("unrepresentable hart count must fail"),
            MachineError::InvalidHartCount(MAX_HARTS + 1)
        );
        let admitted =
            Machine::new_with_harts(config, MAX_HARTS).expect("maximum hart topology is admitted");
        assert_eq!(admitted.hart_count(), MAX_HARTS);
        assert_eq!(machine(1).hart_count(), 1);
        let mut machine = machine(8);
        assert_eq!(
            machine.load_program(&[]),
            Err(MachineError::InvalidProgramBytes {
                image: 0,
                ram: 4096,
            })
        );
    }

    #[test]
    fn deterministic_scheduler_preserves_exact_per_hart_architectural_state() {
        let mut machine = Machine::new_with_harts(
            MachineConfig {
                ram_bytes: 4096,
                max_console_bytes: 8,
            },
            2,
        )
        .expect("admit two harts");
        machine
            .load_program(&words(&[
                encode_csrr(5, Csr::Mhartid),
                encode_addi(6, 5, 10),
            ]))
            .expect("load shared hart probe");
        assert!(machine.set_hart_lifecycle(1, HartLifecycle::Started));

        machine.scheduler_quantum_remaining = 1;
        assert_eq!(machine.run_slice(1).instructions_retired, 1);
        assert_eq!(machine.active_hart_id(), 0);
        assert_eq!(machine.register(5), Some(0));

        assert_eq!(machine.run_slice(1).instructions_retired, 1);
        assert_eq!(machine.active_hart_id(), 1);
        assert_eq!(machine.csr(Csr::Mhartid), 1);
        assert_eq!(machine.register(5), Some(1));
        assert_eq!(machine.instret, 1);
        let hart_zero = &machine.harts[0];
        assert_eq!(hart_zero.cpu.registers[5], 0);
        assert_eq!(hart_zero.instret, 1);

        machine.mtimecmp = 11;
        machine.msip = true;
        machine.csrs.sscratch = 0x1111;
        assert_eq!(machine.run_slice(1).instructions_retired, 1);
        assert_eq!(machine.register(6), Some(11));
        machine.scheduler_quantum_remaining = 0;
        assert_eq!(machine.run_slice(1).instructions_retired, 1);
        assert_eq!(machine.active_hart_id(), 0);
        assert_eq!(machine.register(6), Some(10));
        assert_eq!(machine.mtimecmp, u64::MAX);
        assert!(!machine.msip);
        assert_eq!(machine.csrs.sscratch, 0);
        let hart_one = &machine.harts[1];
        assert_eq!(hart_one.cpu.registers[6], 11);
        assert_eq!(hart_one.mtimecmp, 11);
        assert!(hart_one.msip);
        assert_eq!(hart_one.csrs.sscratch, 0x1111);
        assert_eq!(machine.instructions_retired, 4);
    }

    #[test]
    fn hart_start_mailbox_has_one_winner_and_publishes_one_payload() {
        let control = std::sync::Arc::new(HartControl::new(HartLifecycle::Stopped));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles = (0..8_u64)
            .map(|worker| {
                let control = std::sync::Arc::clone(&control);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    (
                        worker,
                        control.request_start(DRAM_BASE + worker * 2, worker),
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("start claimant"))
            .collect::<Vec<_>>();
        let (winner, generation) = results
            .iter()
            .find_map(|(worker, result)| {
                result
                    .as_ref()
                    .ok()
                    .map(|generation| (*worker, *generation))
            })
            .expect("one start claimant");
        assert_eq!(
            results.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        assert_eq!(
            control.pending_start(),
            Some((DRAM_BASE + winner * 2, winner, generation))
        );
        control.acknowledge_start(generation);
        assert_eq!(control.lifecycle(), HartLifecycle::Started);
        assert!(control.is_quiescent());
    }

    #[test]
    fn hart_interrupt_and_fence_mailboxes_coalesce_without_loss() {
        let control = std::sync::Arc::new(HartControl::new(HartLifecycle::Started));
        let handles = (0..8_u32)
            .map(|worker| {
                let control = std::sync::Arc::clone(&control);
                std::thread::spawn(move || {
                    control.raise_interrupt(1_u64 << worker);
                    control.request_fence()
                })
            })
            .collect::<Vec<_>>();
        let generations = handles
            .into_iter()
            .map(|handle| handle.join().expect("control publisher"))
            .collect::<Vec<_>>();
        assert_eq!(control.take_interrupts(), 0xff);
        assert_eq!(control.pending_fence(), Some(8));
        assert_eq!(
            generations
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            (1..=8).collect()
        );
        assert!(!control.is_quiescent());
        control.acknowledge_fence(8);
        assert!(control.is_quiescent());
    }

    #[test]
    fn exact_hart_slice_never_runs_or_rotates_to_another_hart() {
        let mut machine = Machine::new_with_harts(
            MachineConfig {
                ram_bytes: 4096,
                max_console_bytes: 8,
            },
            2,
        )
        .expect("admit two harts");
        machine
            .load_program(&words(&[
                encode_csrr(5, Csr::Mhartid),
                encode_addi(6, 5, 10),
            ]))
            .expect("load shared hart probe");

        let stopped = machine.run_hart_slice(1, 2).expect("admitted hart");
        assert_eq!(stopped.steps_executed, 0);
        assert_eq!(stopped.instructions_retired, 0);
        assert_eq!(machine.active_hart_id(), 1);
        assert_eq!(machine.register(5), Some(0));

        assert!(machine.set_hart_lifecycle(1, HartLifecycle::Started));
        let secondary = machine.run_hart_slice(1, 2).expect("started hart");
        assert_eq!(secondary.steps_executed, 2);
        assert_eq!(secondary.instructions_retired, 2);
        assert_eq!(machine.active_hart_id(), 1);
        assert_eq!(machine.register(5), Some(1));
        assert_eq!(machine.register(6), Some(11));
        assert_eq!(machine.harts[0].cpu.pc, DRAM_BASE);
        assert_eq!(machine.harts[0].instret, 0);

        assert_eq!(
            machine.run_hart_slice(2, 1),
            Err(InvalidHartId {
                hart_id: 2,
                hart_count: 2,
            })
        );
    }

    #[test]
    fn a_store_invalidates_every_harts_overlapping_reservation() {
        let mut machine = Machine::new_with_harts(
            MachineConfig {
                ram_bytes: 4096,
                max_console_bytes: 8,
            },
            2,
        )
        .expect("admit two harts");
        machine.reservation = Some((DRAM_BASE + 8, 8));
        machine.harts[1].reservation = Some((DRAM_BASE + 12, 4));

        machine.invalidate_reservation(DRAM_BASE + 10, 4);

        assert_eq!(machine.reservation, None);
        assert_eq!(machine.harts[1].reservation, None);
    }

    #[test]
    fn console_input_is_read_through_uart_registers() {
        let mut machine = machine(8);
        let program = words(&[
            encode_lui(5, 0x1_0000),
            0x0052_c303, // lbu x6, 5(x5): UART line status
            0x0002_c383, // lbu x7, 0(x5): UART receive byte
            0x0010_0073,
        ]);
        machine.load_program(&program).expect("load program");
        machine.push_console_input(b"Z");

        let report = machine.run_slice(4);
        assert_eq!(machine.register(6), Some(0x61));
        assert_eq!(machine.register(7), Some(u64::from(b'Z')));
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(machine.csr(Csr::Mcause), 3);
    }

    #[test]
    fn sv39_translates_fetch_load_and_store_and_sets_ad_bits() {
        let mut machine = paged_machine();
        let virtual_address = 0x4000_0000;
        let physical = DRAM_BASE + 0x4000;
        let program = words(&[
            encode_addi(5, 0, 42),
            encode_auipc(6, 0),
            encode_store(6, 5, 0x100, 2),
            encode_load(7, 6, 0x100, 6),
        ]);
        assert!(machine.devices.write_ram(physical, 0, 8));
        for (offset, byte) in program.iter().copied().enumerate() {
            assert!(
                machine
                    .devices
                    .write_ram(physical + offset as u64, u64::from(byte), 1)
            );
        }
        let leaf = install_4k_mapping(
            &mut machine,
            virtual_address,
            physical,
            PTE_VALID | PTE_READ | PTE_WRITE | PTE_EXECUTE,
        );
        machine.cpu.pc = virtual_address;
        machine.cpu.privilege = Privilege::Supervisor;

        let report = machine.run_slice(4);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 4);
        assert_eq!(machine.register(7), Some(42));
        assert_eq!(machine.devices.read_ram(physical + 0x104, 4), Some(42));
        let pte = machine.devices.read_ram(leaf, 8).expect("leaf PTE");
        assert_eq!(pte & (PTE_ACCESSED | PTE_DIRTY), PTE_ACCESSED | PTE_DIRTY);
        assert_eq!(
            machine.metrics(),
            MachineMetrics {
                instruction_fetches: 4,
                instruction_translations: 4,
                load_translations: 1,
                store_translations: 1,
                sv39_walks: 3,
                page_table_entries_read: 9,
                page_table_entries_written: 2,
                translation_cache_hits: 3,
                translation_cache_misses: 3,
                translation_cache_flushes: 0,
            }
        );
    }

    #[test]
    fn sv39_permission_fault_is_delegated_with_virtual_stval() {
        let mut machine = paged_machine();
        let virtual_address = 0x4000_0000;
        let physical = DRAM_BASE + 0x4000;
        let load = encode_load(7, 6, 0, 3);
        assert!(machine.devices.write_ram(physical, u64::from(load), 4));
        assert!(machine.devices.write_ram(physical + 0x100, 0xfeed_face, 8));
        let leaf = install_4k_mapping(
            &mut machine,
            virtual_address,
            physical,
            PTE_VALID | PTE_EXECUTE,
        );
        machine.cpu.pc = virtual_address;
        machine.cpu.registers[6] = virtual_address + 0x100;
        machine.cpu.privilege = Privilege::Supervisor;
        machine.csrs.medeleg = 1 << CAUSE_LOAD_PAGE_FAULT;
        machine.csrs.stvec = virtual_address + 0x200;

        let report = machine.run_slice(1);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 0);
        assert_eq!(machine.privilege(), Privilege::Supervisor);
        assert_eq!(machine.pc(), virtual_address + 0x200);
        assert_eq!(machine.csr(Csr::Scause), CAUSE_LOAD_PAGE_FAULT);
        assert_eq!(machine.csr(Csr::Sepc), virtual_address);
        assert_eq!(machine.csr(Csr::Stval), virtual_address + 0x100);
        assert_ne!(
            machine.devices.read_ram(leaf, 8).expect("leaf PTE") & PTE_ACCESSED,
            0
        );

        machine.cpu.pc = virtual_address;
        machine.cpu.privilege = Privilege::Supervisor;
        machine.csrs.mstatus |= MSTATUS_MXR;
        let retried = machine.run_slice(1);
        assert_eq!(retried.instructions_retired, 1);
        assert_eq!(machine.register(7), Some(0xfeed_face));
    }

    #[test]
    fn sv39_superpages_canonicality_and_mprv_are_exact() {
        let mut machine = paged_machine();
        let virtual_address = 0x4000_0000;
        let root = DRAM_BASE + 0x1000;
        let level_one = DRAM_BASE + 0x2000;
        let vpn2 = virtual_address >> 30 & 0x1ff;
        let vpn1 = virtual_address >> 21 & 0x1ff;
        assert!(machine.devices.write_ram(
            root + vpn2 * 8,
            ((level_one >> 12) << 10) | PTE_VALID,
            8,
        ));
        assert!(machine.devices.write_ram(
            level_one + vpn1 * 8,
            ((DRAM_BASE >> 12) << 10) | PTE_VALID | PTE_READ | PTE_WRITE,
            8,
        ));
        machine.csrs.write(
            Csr::Satp,
            (SATP_MODE_SV39 << SATP_MODE_SHIFT) | (root >> 12),
        );
        machine.cpu.privilege = Privilege::Supervisor;
        assert_eq!(
            machine.translate(virtual_address + 0x1234, AccessType::Load),
            Ok(DRAM_BASE + 0x1234)
        );
        assert_eq!(
            machine.translate(1 << 39, AccessType::Load),
            Err(MachineTrap::LoadPageFault { address: 1 << 39 })
        );

        machine.cpu.privilege = Privilege::Machine;
        machine.csrs.mstatus = MSTATUS_MPRV | (1 << MSTATUS_MPP_SHIFT);
        assert_eq!(
            machine.translate(virtual_address + 0x2345, AccessType::Load),
            Ok(DRAM_BASE + 0x2345)
        );
    }

    #[test]
    fn sfence_vma_is_privileged_and_conservatively_flushes_the_translation_cache() {
        let mut machine = paged_machine();
        let virtual_address = 0x4000_0000;
        let first_physical = DRAM_BASE + 0x4000;
        let second_physical = DRAM_BASE + 0x5000;
        let sfence_vma = 0x1200_0073_u32;
        assert!(
            machine
                .devices
                .write_ram(first_physical, u64::from(sfence_vma), 4)
        );
        assert!(
            machine
                .devices
                .write_ram(second_physical, u64::from(sfence_vma), 4)
        );
        let leaf = install_4k_mapping(
            &mut machine,
            virtual_address,
            first_physical,
            PTE_VALID | PTE_READ | PTE_EXECUTE,
        );
        machine.cpu.pc = virtual_address;
        machine.cpu.privilege = Privilege::Supervisor;

        assert_eq!(
            machine.translate(virtual_address, AccessType::Instruction),
            Ok(first_physical)
        );
        assert!(machine.devices.write_ram(
            leaf,
            ((second_physical >> GUEST_PAGE_SHIFT) << 10)
                | PTE_VALID
                | PTE_READ
                | PTE_EXECUTE
                | PTE_ACCESSED,
            8,
        ));
        assert_eq!(
            machine.translate(virtual_address, AccessType::Instruction),
            Ok(first_physical),
            "page-table changes remain cached until an architectural fence"
        );

        let report = machine.run_slice(1);
        assert_eq!(report.instructions_retired, 1);
        assert_eq!(machine.pc(), virtual_address + 4);
        assert_eq!(machine.metrics().translation_cache_flushes, 1);
        assert_eq!(
            machine.translate(virtual_address, AccessType::Instruction),
            Ok(second_physical)
        );

        let pte = machine.devices.read_ram(leaf, 8).expect("leaf PTE");
        assert!(machine.devices.write_ram(leaf, pte | PTE_USER, 8));
        machine.cpu.pc = virtual_address;
        machine.cpu.privilege = Privilege::User;
        let forbidden = machine.run_slice(1);
        assert_eq!(forbidden.instructions_retired, 0);
        assert_eq!(machine.csr(Csr::Mcause), 2);
        assert_eq!(machine.csr(Csr::Mtval), u64::from(sfence_vma));
    }

    #[test]
    fn sv39_sum_mxr_and_user_permissions_form_a_closed_matrix() {
        let mut machine = paged_machine();
        let user_read = PTE_VALID | PTE_USER | PTE_READ;
        let supervisor_read = PTE_VALID | PTE_READ;
        let execute_only = PTE_VALID | PTE_EXECUTE;

        assert!(!machine.page_permissions_allow(
            user_read,
            AccessType::Load,
            Privilege::Supervisor,
        ));
        machine.csrs.mstatus |= MSTATUS_SUM;
        assert!(
            machine.page_permissions_allow(user_read, AccessType::Load, Privilege::Supervisor,)
        );
        assert!(!machine.page_permissions_allow(
            user_read | PTE_EXECUTE,
            AccessType::Instruction,
            Privilege::Supervisor,
        ));
        assert!(!machine.page_permissions_allow(
            supervisor_read,
            AccessType::Load,
            Privilege::User,
        ));
        assert!(!machine.page_permissions_allow(
            execute_only,
            AccessType::Load,
            Privilege::Supervisor,
        ));
        machine.csrs.mstatus |= MSTATUS_MXR;
        assert!(machine.page_permissions_allow(
            execute_only,
            AccessType::Load,
            Privilege::Supervisor,
        ));
    }

    #[test]
    fn rv64m_operations_cover_high_halves_and_division_edges() {
        let cases = [
            (0, 7, 9, 63),
            (1, u64::MAX - 1, 3, u64::MAX),
            (2, u64::MAX - 1, 3, u64::MAX),
            (3, u64::MAX, u64::MAX, u64::MAX - 1),
            (4, (-20_i64) as u64, 3, (-6_i64) as u64),
            (5, 20, 3, 6),
            (6, (-20_i64) as u64, 3, (-2_i64) as u64),
            (7, 20, 3, 2),
            (4, i64::MIN as u64, (-1_i64) as u64, i64::MIN as u64),
            (4, 7, 0, u64::MAX),
            (6, 7, 0, 7),
        ];
        for (funct3, lhs, rhs, expected) in cases {
            let mut machine = machine(8);
            machine
                .load_program(&encode_op(7, 5, 6, funct3, 1).to_le_bytes())
                .expect("load M operation");
            machine.cpu.registers[5] = lhs;
            machine.cpu.registers[6] = rhs;
            assert_eq!(machine.run_slice(1).instructions_retired, 1);
            assert_eq!(machine.register(7), Some(expected), "funct3={funct3}");
        }

        let word_cases = [
            (0, 0xffff_ffff, 2, u64::MAX - 1),
            (4, i32::MIN as u32, u32::MAX, 0xffff_ffff_8000_0000),
            (5, 12, 5, 2),
            (6, (-12_i32) as u32, 5, (-2_i64) as u64),
            (7, 12, 5, 2),
        ];
        for (funct3, lhs, rhs, expected) in word_cases {
            let mut machine = machine(8);
            machine
                .load_program(&encode_op_32(7, 5, 6, funct3, 1).to_le_bytes())
                .expect("load M word operation");
            machine.cpu.registers[5] = u64::from(lhs);
            machine.cpu.registers[6] = u64::from(rhs);
            assert_eq!(machine.run_slice(1).instructions_retired, 1);
            assert_eq!(machine.register(7), Some(expected), "funct3={funct3}");
        }
    }

    #[test]
    fn rv64c_load_store_and_mixed_width_fetch_preserve_exact_pc_and_values() {
        let mut machine = machine(8);
        let mut program = halfwords(&[
            0x0800, // c.addi4spn x8, sp, 16
            0xc004, // c.sw x9, 0(x8)
            0x4008, // c.lw x10, 0(x8)
            0xe408, // c.sd x10, 8(x8)
            0x640c, // c.ld x11, 8(x8)
        ]);
        program.extend_from_slice(&encode_addi(12, 11, 1).to_le_bytes());
        machine
            .load_program(&program)
            .expect("load mixed RV64C program");
        machine.cpu.registers[2] = DRAM_BASE + 0x100;
        machine.cpu.registers[9] = 0xffff_ffff_8000_0007;

        let report = machine.run_slice(6);
        assert_eq!(report.instructions_retired, 6);
        assert_eq!(machine.register(8), Some(DRAM_BASE + 0x110));
        assert_eq!(machine.register(10), Some(0xffff_ffff_8000_0007));
        assert_eq!(machine.register(11), Some(0xffff_ffff_8000_0007));
        assert_eq!(machine.register(12), Some(0xffff_ffff_8000_0008));
        assert_eq!(machine.pc(), DRAM_BASE + 14);
    }

    #[test]
    fn rv64c_integer_alu_forms_cover_immediates_registers_and_words() {
        let immediate_cases: [(u16, usize, u64, u64); 5] = [
            (0x0285, 5, 41, 42),                             // c.addi x5, 1
            (0x2285, 5, 0x7fff_ffff, 0xffff_ffff_8000_0000), // c.addiw x5, 1
            (0x537d, 6, 0, u64::MAX),                        // c.li x6, -1
            (0x6285, 5, 0, 4096),                            // c.lui x5, 1
            (0x0286, 5, 21, 42),                             // c.slli x5, 1
        ];
        for (instruction, register, initial, expected) in immediate_cases {
            let mut machine = machine(8);
            machine
                .load_program(&instruction.to_le_bytes())
                .expect("load compressed immediate operation");
            machine.cpu.registers[register] = initial;
            assert_eq!(machine.run_slice(1).instructions_retired, 1);
            assert_eq!(machine.register(register), Some(expected));
        }

        let mut stack = machine(8);
        stack
            .load_program(&0x6141_u16.to_le_bytes())
            .expect("load c.addi16sp");
        stack.cpu.registers[2] = DRAM_BASE + 0x100;
        assert_eq!(stack.run_slice(1).instructions_retired, 1);
        assert_eq!(stack.register(2), Some(DRAM_BASE + 0x110));

        let register_cases: [(u16, usize, u64, u64, u64); 9] = [
            (0x8005, 8, 9, 0, 4),                               // c.srli x8, 1
            (0x8405, 8, u64::MAX - 2, 0, u64::MAX - 1),         // c.srai x8, 1
            (0x987d, 8, 0x55, 0, 0x55),                         // c.andi x8, -1
            (0x8c05, 8, 9, 4, 5),                               // c.sub x8, x9
            (0x8c25, 8, 9, 3, 10),                              // c.xor x8, x9
            (0x8c45, 8, 8, 3, 11),                              // c.or x8, x9
            (0x8c65, 8, 11, 3, 3),                              // c.and x8, x9
            (0x9c05, 8, 0x8000_0000, 1, 0x0000_0000_7fff_ffff), // c.subw
            (0x9c25, 8, 0x7fff_ffff, 1, 0xffff_ffff_8000_0000), // c.addw
        ];
        for (instruction, rd, lhs, rhs, expected) in register_cases {
            let mut machine = machine(8);
            machine
                .load_program(&instruction.to_le_bytes())
                .expect("load compressed register operation");
            machine.cpu.registers[rd] = lhs;
            machine.cpu.registers[9] = rhs;
            assert_eq!(machine.run_slice(1).instructions_retired, 1);
            assert_eq!(machine.register(rd), Some(expected));
        }
    }

    #[test]
    fn rv64c_control_transfer_link_and_branch_forms_use_two_byte_ialign() {
        let mut jump = machine(8);
        jump.load_program(&0xa829_u16.to_le_bytes())
            .expect("load c.j");
        assert_eq!(jump.run_slice(1).instructions_retired, 1);
        assert_eq!(jump.pc(), DRAM_BASE + 26);

        let mut equal = machine(8);
        equal
            .load_program(&0xcc01_u16.to_le_bytes())
            .expect("load c.beqz");
        equal.cpu.registers[8] = 0;
        assert_eq!(equal.run_slice(1).instructions_retired, 1);
        assert_eq!(equal.pc(), DRAM_BASE + 24);

        let mut not_equal = machine(8);
        not_equal
            .load_program(&0xe819_u16.to_le_bytes())
            .expect("load c.bnez");
        not_equal.cpu.registers[8] = 1;
        assert_eq!(not_equal.run_slice(1).instructions_retired, 1);
        assert_eq!(not_equal.pc(), DRAM_BASE + 22);

        let mut link = machine(8);
        link.load_program(&0x9282_u16.to_le_bytes())
            .expect("load c.jalr");
        link.cpu.registers[5] = DRAM_BASE + 0x100;
        assert_eq!(link.run_slice(1).instructions_retired, 1);
        assert_eq!(link.register(1), Some(DRAM_BASE + 2));
        assert_eq!(link.pc(), DRAM_BASE + 0x100);
    }

    #[test]
    fn rv64c_fetch_crosses_a_page_only_after_admitting_both_halfwords() {
        let mut machine = Machine::new(MachineConfig {
            ram_bytes: 8192,
            max_console_bytes: 8,
        })
        .expect("valid two-page machine");
        machine
            .load_program(&vec![0; 4098])
            .expect("load two pages");
        let instruction = encode_addi(5, 0, 42);
        assert!(
            machine
                .devices
                .write_ram(DRAM_BASE + 4094, u64::from(instruction & 0xffff), 2,)
        );
        assert!(
            machine
                .devices
                .write_ram(DRAM_BASE + 4096, u64::from(instruction >> 16), 2,)
        );
        machine.cpu.pc = DRAM_BASE + 4094;

        let report = machine.run_slice(1);
        assert_eq!(report.instructions_retired, 1);
        assert_eq!(machine.register(5), Some(42));
        assert_eq!(machine.pc(), DRAM_BASE + 4098);
        assert_eq!(machine.metrics().instruction_translations, 2);
    }

    #[test]
    fn rv64c_rejects_compact_float_while_fs_is_off_and_reserved_encodings() {
        for instruction in [0x0000_u16, 0x2000, 0xa000] {
            let mut machine = machine(8);
            machine
                .load_program(&instruction.to_le_bytes())
                .expect("load reserved compressed instruction");
            let report = machine.run_slice(1);
            assert_eq!(report.instructions_retired, 0);
            assert_eq!(machine.csr(Csr::Mcause), 2);
            assert_eq!(machine.csr(Csr::Mtval), u64::from(instruction));
        }
    }

    #[test]
    fn rv64fd_is_gated_by_fs_and_fcsr_access_marks_state_dirty() {
        let fadd_s = encode_float_op(3, 1, 2, 0, 0x00);
        let mut disabled = machine(8);
        disabled
            .load_program(&fadd_s.to_le_bytes())
            .expect("load gated FADD.S");
        disabled.cpu.write_float32(1, 1.5_f32.to_bits());
        disabled.cpu.write_float32(2, 2.5_f32.to_bits());
        let report = disabled.run_slice(1);
        assert_eq!(report.instructions_retired, 0);
        assert_eq!(disabled.csr(Csr::Mcause), 2);
        assert_eq!(disabled.csr(Csr::Mtval), u64::from(fadd_s));

        let mut csr_disabled = machine(8);
        let read_fcsr = encode_csrr(5, Csr::Fcsr);
        csr_disabled
            .load_program(&read_fcsr.to_le_bytes())
            .expect("load gated FCSR read");
        assert_eq!(csr_disabled.run_slice(1).instructions_retired, 0);
        assert_eq!(csr_disabled.csr(Csr::Mcause), 2);

        let mut enabled = machine(8);
        enabled
            .load_program(&words(&[
                encode_csrw(Csr::Frm, 5),
                encode_csrr(6, Csr::Fcsr),
            ]))
            .expect("load FCSR program");
        enabled.csrs.mstatus = MSTATUS_FS_INITIAL;
        enabled.cpu.registers[5] = 3;
        assert_eq!(enabled.run_slice(2).instructions_retired, 2);
        assert_eq!(enabled.register(6), Some(3 << FRM_SHIFT));
        assert_eq!(enabled.csrs.fcsr, 3 << FRM_SHIFT);
        assert_eq!(enabled.csrs.mstatus & MSTATUS_FS, MSTATUS_FS_DIRTY);
        assert_ne!(enabled.csr(Csr::Mstatus) & MSTATUS_SD, 0);
    }

    #[test]
    fn rv64fd_arithmetic_sqrt_fma_nan_boxing_and_flags_are_deterministic() {
        let mut machine = machine(8);
        machine
            .load_program(&words(&[
                encode_float_op(3, 1, 2, 0, 0x00),          // fadd.s f3, f1, f2
                encode_float_op(4, 3, 0, 0, 0x2c),          // fsqrt.s f4, f3
                encode_float_fused(0x43, 8, 5, 6, 7, 1, 0), // fmadd.d f8, f5, f6, f7
                encode_float_op(11, 9, 10, 0, 0x00),        // fadd.s f11, f9, f10
                encode_float_op(14, 12, 13, 0, 0x0c),       // fdiv.s f14, f12, f13
                encode_float_op(16, 15, 0, 0, 0x2c),        // fsqrt.s f16, f15
            ]))
            .expect("load RV64FD arithmetic program");
        machine.csrs.mstatus = MSTATUS_FS_INITIAL;
        machine.cpu.write_float32(1, 1.5_f32.to_bits());
        machine.cpu.write_float32(2, 2.5_f32.to_bits());
        machine.cpu.write_float64(5, 2.0_f64.to_bits());
        machine.cpu.write_float64(6, 3.0_f64.to_bits());
        machine.cpu.write_float64(7, 4.0_f64.to_bits());
        machine.cpu.floating_registers[9] = u64::from(1.0_f32.to_bits());
        machine.cpu.write_float32(10, 1.0_f32.to_bits());
        machine.cpu.write_float32(12, 1.0_f32.to_bits());
        machine.cpu.write_float32(13, 0.0_f32.to_bits());
        machine.cpu.write_float32(15, (-1.0_f32).to_bits());

        assert_eq!(machine.run_slice(6).instructions_retired, 6);
        assert_eq!(machine.cpu.read_float32(3), 4.0_f32.to_bits());
        assert_eq!(machine.cpu.read_float32(4), 2.0_f32.to_bits());
        assert_eq!(machine.cpu.read_float64(8), 10.0_f64.to_bits());
        assert_eq!(machine.cpu.read_float32(11), CANONICAL_NAN_F32);
        assert_eq!(machine.cpu.read_float32(14), f32::INFINITY.to_bits());
        assert_eq!(machine.cpu.read_float32(16), CANONICAL_NAN_F32);
        assert_eq!(machine.csrs.fcsr & FFLAGS_MASK, FFLAGS_DZ | FFLAGS_NV);
    }

    #[test]
    fn rv64fd_rounding_conversion_comparison_and_signed_zero_edges_are_exact() {
        let mut edges = machine(8);
        edges
            .load_program(&words(&[
                encode_float_op(5, 1, 2, 0, 0x14),  // fmin.s f5, f1, f2
                encode_float_op(6, 1, 2, 1, 0x14),  // fmax.s f6, f1, f2
                encode_float_op(7, 3, 4, 2, 0x50),  // feq.s x7, f3, f4
                encode_float_op(8, 3, 4, 1, 0x50),  // flt.s x8, f3, f4
                encode_float_op(9, 3, 0, 0, 0x60),  // fcvt.w.s x9, f3
                encode_float_op(10, 3, 1, 0, 0x60), // fcvt.wu.s x10, f3
            ]))
            .expect("load RV64FD edge program");
        edges.csrs.mstatus = MSTATUS_FS_INITIAL;
        edges.cpu.write_float32(1, (-0.0_f32).to_bits());
        edges.cpu.write_float32(2, 0.0_f32.to_bits());
        edges.cpu.write_float32(3, CANONICAL_NAN_F32);
        edges.cpu.write_float32(4, 1.0_f32.to_bits());

        assert_eq!(edges.run_slice(6).instructions_retired, 6);
        assert_eq!(edges.cpu.read_float32(5), (-0.0_f32).to_bits());
        assert_eq!(edges.cpu.read_float32(6), 0.0_f32.to_bits());
        assert_eq!(edges.register(7), Some(0));
        assert_eq!(edges.register(8), Some(0));
        assert_eq!(edges.register(9), Some(0x0000_0000_7fff_ffff));
        assert_eq!(edges.register(10), Some(u64::MAX));
        assert_eq!(edges.csrs.fcsr & FFLAGS_MASK, FFLAGS_NV);

        let mut invalid_dynamic_round = machine(8);
        let instruction = encode_float_op(3, 1, 2, 0b111, 0x00);
        invalid_dynamic_round
            .load_program(&instruction.to_le_bytes())
            .expect("load dynamically rounded FADD.S");
        invalid_dynamic_round.csrs.mstatus = MSTATUS_FS_INITIAL;
        invalid_dynamic_round.csrs.fcsr = 0b101 << FRM_SHIFT;
        assert_eq!(invalid_dynamic_round.run_slice(1).instructions_retired, 0);
        assert_eq!(invalid_dynamic_round.csr(Csr::Mcause), 2);
        assert_eq!(
            invalid_dynamic_round.csr(Csr::Mtval),
            u64::from(instruction)
        );
    }

    #[test]
    fn rv64fd_standard_and_compact_memory_forms_preserve_exact_bits() {
        let address = DRAM_BASE + 0x100;
        let mut standard = machine(8);
        standard
            .load_program(&words(&[
                encode_float_store(5, 1, 0, 2),    // fsw f1, 0(x5)
                encode_float_load(2, 5, 0, 2),     // flw f2, 0(x5)
                encode_float_op(6, 2, 0, 0, 0x70), // fmv.x.w x6, f2
            ]))
            .expect("load standard float memory program");
        standard.csrs.mstatus = MSTATUS_FS_INITIAL;
        standard.cpu.registers[5] = address;
        standard.cpu.write_float32(1, (-3.5_f32).to_bits());
        assert_eq!(standard.run_slice(3).instructions_retired, 3);
        assert_eq!(
            standard.register(6),
            Some(sign_extend(u64::from((-3.5_f32).to_bits()), 32))
        );

        let mut compact = machine(8);
        let mut program = halfwords(&[
            0xa000, // c.fsd f8, 0(x8)
            0x2004, // c.fld f9, 0(x8)
        ]);
        program.extend_from_slice(&encode_float_op(7, 9, 0, 0, 0x71).to_le_bytes());
        compact
            .load_program(&program)
            .expect("load compact double memory program");
        compact.csrs.mstatus = MSTATUS_FS_INITIAL;
        compact.cpu.registers[8] = address;
        compact.cpu.write_float64(8, (-13.25_f64).to_bits());
        assert_eq!(compact.run_slice(3).instructions_retired, 3);
        assert_eq!(compact.register(7), Some((-13.25_f64).to_bits()));
        assert_eq!(compact.pc(), DRAM_BASE + 8);
    }

    #[test]
    fn rv64a_lr_sc_and_amo_are_single_hart_atomic_and_sign_extend_words() {
        let mut rv = machine(8);
        let address = DRAM_BASE + 0x100;
        let program = words(&[
            encode_atomic(5, 6, 0, 3, 0x02),
            encode_addi(7, 0, 1),
            encode_atomic(8, 6, 7, 3, 0x03),
            encode_atomic(9, 6, 7, 3, 0x00),
        ]);
        rv.load_program(&program).expect("load atomic program");
        rv.cpu.registers[6] = address;
        assert!(rv.devices.write_ram(address, 41, 8));

        let report = rv.run_slice(4);
        assert_eq!(report.instructions_retired, 4);
        assert_eq!(rv.register(5), Some(41));
        assert_eq!(rv.register(8), Some(0));
        assert_eq!(rv.register(9), Some(1));
        assert_eq!(rv.devices.read_ram(address, 8), Some(2));

        let mut invalidated = machine(8);
        let program = words(&[
            encode_atomic(5, 6, 0, 3, 0x02),
            encode_store(6, 7, 0, 3),
            encode_atomic(8, 6, 9, 3, 0x03),
        ]);
        invalidated
            .load_program(&program)
            .expect("load invalidation program");
        invalidated.cpu.registers[6] = address;
        invalidated.cpu.registers[7] = 7;
        invalidated.cpu.registers[9] = 9;
        assert!(invalidated.devices.write_ram(address, 41, 8));
        assert_eq!(invalidated.run_slice(3).instructions_retired, 3);
        assert_eq!(invalidated.register(8), Some(1));
        assert_eq!(invalidated.devices.read_ram(address, 8), Some(7));

        let mut word = machine(8);
        word.load_program(&encode_atomic(5, 6, 7, 2, 0x01).to_le_bytes())
            .expect("load amoswap.w");
        word.cpu.registers[6] = address;
        word.cpu.registers[7] = 1;
        assert!(word.devices.write_ram(address, 0x8000_0000, 4));
        assert_eq!(word.run_slice(1).instructions_retired, 1);
        assert_eq!(word.register(5), Some(0xffff_ffff_8000_0000));
        assert_eq!(word.devices.read_ram(address, 4), Some(1));

        let amo_cases = [
            (0x00, 5, 3, 8),
            (0x01, 5, 3, 3),
            (0x04, 5, 3, 6),
            (0x08, 5, 3, 7),
            (0x0c, 5, 3, 1),
            (0x10, (-5_i64) as u64, 3, (-5_i64) as u64),
            (0x14, (-5_i64) as u64, 3, 3),
            (0x18, 5, 3, 3),
            (0x1c, 5, 3, 5),
        ];
        for (operation, old, rhs, expected) in amo_cases {
            assert_eq!(atomic_result(operation, old, rhs, 8), Some(expected));
        }
    }

    #[test]
    fn counters_are_deterministic_and_counteren_gates_lower_privilege() {
        let mut rv = machine(8);
        let program = words(&[
            encode_csrr(5, Csr::Cycle),
            encode_csrr(6, Csr::Time),
            encode_csrr(7, Csr::Instret),
        ]);
        rv.load_program(&program).expect("load counter reads");
        let report = rv.run_slice(3);
        assert_eq!(report.instructions_retired, 3);
        assert_eq!(rv.register(5), Some(0));
        assert_eq!(rv.register(6), Some(1));
        assert_eq!(rv.register(7), Some(2));
        assert_eq!(rv.csr(Csr::Mcycle), 3);
        assert_eq!(rv.csr(Csr::Time), 3);
        assert_eq!(rv.csr(Csr::Minstret), 3);

        assert_eq!(
            rv.csrs
                .validate_access(Csr::Time.address(), Privilege::Supervisor, false),
            None
        );
        rv.csrs.write(Csr::Mcounteren, 0b111);
        assert_eq!(
            rv.csrs
                .validate_access(Csr::Time.address(), Privilege::Supervisor, false),
            Some(Csr::Time)
        );
        assert_eq!(
            rv.csrs
                .validate_access(Csr::Time.address(), Privilege::User, false),
            None
        );
        rv.csrs.write(Csr::Scounteren, 0b010);
        assert_eq!(
            rv.csrs
                .validate_access(Csr::Time.address(), Privilege::User, false),
            Some(Csr::Time)
        );

        let mut writable = machine(8);
        writable
            .load_program(&words(&[
                encode_addi(5, 0, 100),
                encode_csrw(Csr::Mcycle, 5),
                encode_csrr(6, Csr::Mcycle),
            ]))
            .expect("load writable counter program");
        let report = writable.run_slice(3);
        assert_eq!(report.total_steps_executed, 3);
        assert_eq!(report.total_instructions_retired, 3);
        assert_eq!(writable.register(6), Some(101));
        assert_eq!(writable.csr(Csr::Mcycle), 102);
    }

    #[test]
    fn deterministic_clint_timer_enters_vectored_machine_interrupt() {
        let mut machine = machine(8);
        machine
            .load_program(&words(&[
                encode_addi(5, 5, 1),
                encode_addi(5, 5, 1),
                encode_addi(5, 5, 1),
            ]))
            .expect("load timer program");
        machine.mtimecmp = 2;
        machine.csrs.mie = MIP_MTIP;
        machine.csrs.mstatus = MSTATUS_MIE;
        machine.csrs.mtvec = (DRAM_BASE + 0x100) | 1;

        let report = machine.run_slice(3);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 2);
        assert_eq!(machine.register(5), Some(2));
        assert_eq!(
            machine.csr(Csr::Mcause),
            INTERRUPT_CAUSE_BIT | INTERRUPT_MACHINE_TIMER
        );
        assert_eq!(machine.csr(Csr::Mepc), DRAM_BASE + 8);
        assert_eq!(
            machine.pc(),
            DRAM_BASE + 0x100 + INTERRUPT_MACHINE_TIMER * 4
        );
        assert_eq!(machine.csr(Csr::Mtval), 0);
    }

    #[test]
    fn delegated_supervisor_interrupt_obeys_global_enable_and_wfi_is_bounded() {
        let mut machine = machine(8);
        machine
            .load_program(&0x1050_0073_u32.to_le_bytes())
            .expect("load WFI");
        machine.cpu.privilege = Privilege::Supervisor;
        machine.csrs.mideleg = MIP_STIP;
        machine.csrs.mie = MIP_STIP;
        machine.csrs.mip = MIP_STIP;
        machine.csrs.stvec = DRAM_BASE + 0x100;

        let waiting = machine.run_slice(1);
        assert_eq!(waiting.instructions_retired, 1);
        assert_eq!(machine.pc(), DRAM_BASE + 4);

        machine.cpu.pc = DRAM_BASE;
        machine.csrs.mstatus |= MSTATUS_SIE;
        let interrupted = machine.run_slice(1);
        assert_eq!(interrupted.instructions_retired, 0);
        assert_eq!(
            machine.csr(Csr::Scause),
            INTERRUPT_CAUSE_BIT | INTERRUPT_SUPERVISOR_TIMER
        );
        assert_eq!(machine.csr(Csr::Sepc), DRAM_BASE);
        assert_eq!(machine.pc(), DRAM_BASE + 0x100);
        assert_eq!(machine.privilege(), Privilege::Supervisor);
    }

    #[test]
    fn clint_mmio_is_width_checked_and_drives_hardware_pending_bits() {
        let mut machine = machine(8);
        assert_eq!(machine.read_device(CLINT_BASE + CLINT_MTIME, 8), Ok(0));
        assert_eq!(
            machine.write_device(CLINT_BASE + CLINT_MTIMECMP, 3, 8),
            Ok(None)
        );
        assert_eq!(
            machine.write_device(CLINT_BASE + CLINT_MSIP, 1, 4),
            Ok(None)
        );
        machine.refresh_hardware_interrupts();
        assert_eq!(machine.csr(Csr::Mip) & (MIP_MSIP | MIP_MTIP), MIP_MSIP);
        machine.devices.tick();
        machine.devices.tick();
        machine.devices.tick();
        machine.refresh_hardware_interrupts();
        assert_eq!(
            machine.csr(Csr::Mip) & (MIP_MSIP | MIP_MTIP),
            MIP_MSIP | MIP_MTIP
        );
        assert_eq!(
            machine.read_device(CLINT_BASE + CLINT_MTIME, 4),
            Err(MachineTrap::LoadAccessFault {
                address: CLINT_BASE + CLINT_MTIME,
                bytes: 4,
            })
        );
    }

    #[test]
    fn linux_boot_places_images_builds_fdt_and_enters_exact_s_mode_state() {
        let mut machine = Machine::new(MachineConfig {
            ram_bytes: MIN_LINUX_RAM_BYTES,
            max_console_bytes: 64,
        })
        .expect("valid Linux machine");
        let kernel = words(&[0x0000_0073; 4]);
        let initramfs = b"070701aos-initramfs";
        let layout = machine
            .boot_linux(&kernel, initramfs, "earlycon=sbi console=ttyS0 init=/init")
            .expect("admit Linux boot");

        assert_eq!(layout.kernel_start, LINUX_KERNEL_BASE);
        assert_eq!(layout.kernel_end, LINUX_KERNEL_BASE + kernel.len() as u64);
        assert_eq!(layout.fdt_start, LINUX_FDT_BASE);
        assert!(layout.fdt_bytes > 256);
        assert_eq!(machine.pc(), LINUX_KERNEL_BASE);
        assert_eq!(machine.privilege(), Privilege::Supervisor);
        assert_eq!(machine.register(10), Some(0));
        assert_eq!(machine.register(11), Some(LINUX_FDT_BASE));
        assert_eq!(machine.csr(Csr::Satp), 0);
        assert_eq!(machine.csr(Csr::Mcounteren), 0b111);
        assert_eq!(machine.csr(Csr::Mideleg), MIDELEG_SUPPORTED);
        assert_eq!(
            machine
                .devices
                .ram_range_len(LINUX_FDT_BASE, 4)
                .and_then(|range| machine.devices.ram.copy_to_vec(range)),
            Some(vec![0xd0, 0x0d, 0xfe, 0xed])
        );
        assert_eq!(
            machine
                .devices
                .ram_range_len(layout.initrd_start.expect("initrd start"), initramfs.len())
                .and_then(|range| machine.devices.ram.copy_to_vec(range)),
            Some(initramfs.to_vec())
        );
    }

    #[test]
    fn sbi_3_base_dbcn_time_and_reset_are_bounded_platform_services() {
        let mut machine = Machine::new(MachineConfig {
            ram_bytes: MIN_LINUX_RAM_BYTES,
            max_console_bytes: 64,
        })
        .expect("valid Linux machine");
        machine
            .boot_linux(&words(&[0x0000_0073; 5]), &[], "earlycon=sbi")
            .expect("admit Linux boot");

        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_BASE;
        let base = machine.run_slice(1);
        assert_eq!(base.instructions_retired, 0);
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(machine.register(11), Some(SBI_SPEC_VERSION_3_0));

        machine.cpu.registers[10] = SBI_EXT_AOS_9P;
        machine.cpu.registers[16] = 3;
        machine.cpu.registers[17] = SBI_EXT_BASE;
        assert_eq!(machine.run_slice(1).instructions_retired, 0);
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(machine.register(11), Some(1));

        let message_address = DRAM_BASE + 0x1_0000;
        assert!(machine.devices.write_ram_slice(message_address, b"LINUX"));
        machine.cpu.registers[10] = 5;
        machine.cpu.registers[11] = message_address;
        machine.cpu.registers[12] = 0;
        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_DBCN;
        let console = machine.run_slice(1);
        assert_eq!(console.console, b"LINUX");
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(machine.register(11), Some(5));

        let deadline = machine.csr(Csr::Time) + 10;
        machine.cpu.registers[10] = deadline;
        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_TIME;
        assert_eq!(machine.run_slice(1).instructions_retired, 0);
        assert_eq!(machine.mtimecmp, deadline);
        assert_eq!(machine.csr(Csr::Mip) & MIP_STIP, 0);

        machine.cpu.registers[10] = 0;
        machine.cpu.registers[11] = 0;
        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_SRST;
        assert_eq!(
            machine.run_slice(1).outcome,
            SliceOutcome::Halted(HaltStatus {
                passed: true,
                code: 0,
            })
        );
    }

    #[test]
    fn sbi_hsm_ipi_rfence_and_time_drive_exact_secondary_hart_state() {
        fn call_sbi(
            machine: &mut Machine,
            extension: u64,
            function: u64,
            arguments: [u64; 6],
        ) -> Option<HaltStatus> {
            machine.cpu.registers[10..16].copy_from_slice(&arguments);
            machine.cpu.write(16, function);
            machine.cpu.write(17, extension);
            machine.handle_sbi_call().expect("bounded SBI call")
        }

        let mut machine = Machine::new_with_harts(
            MachineConfig {
                ram_bytes: 4096,
                max_console_bytes: 64,
            },
            2,
        )
        .expect("admit two harts");
        machine
            .load_program(&words(&[
                encode_csrr(5, Csr::Mhartid),
                encode_addi(5, 10, 0),
                0x0000_0073,
            ]))
            .expect("load shared HSM probe");
        machine.firmware.enabled = true;
        let secondary_start = DRAM_BASE + 4;
        let opaque = 0xfeed_beef;

        assert_eq!(
            call_sbi(
                &mut machine,
                SBI_EXT_HSM,
                0,
                [1, secondary_start, opaque, 0, 0, 0],
            ),
            None
        );
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        let secondary = &machine.harts[1];
        assert_eq!(secondary.lifecycle, HartLifecycle::Started);
        assert_eq!(secondary.cpu.pc, secondary_start);
        assert_eq!(secondary.cpu.privilege, Privilege::Supervisor);
        assert_eq!(secondary.cpu.registers[10], 1);
        assert_eq!(secondary.cpu.registers[11], opaque);
        assert_eq!(secondary.csrs.satp, 0);
        assert_eq!(secondary.csrs.mstatus & MSTATUS_SIE, 0);

        call_sbi(
            &mut machine,
            SBI_EXT_HSM,
            0,
            [1, secondary_start, opaque, 0, 0, 0],
        );
        assert_eq!(machine.register(10), Some(SBI_ERR_ALREADY_AVAILABLE));
        call_sbi(&mut machine, SBI_EXT_HSM, 2, [1, 0, 0, 0, 0, 0]);
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(machine.register(11), Some(SBI_HART_STATE_STARTED));

        call_sbi(&mut machine, SBI_EXT_IPI, 0, [1 << 1, 0, 0, 0, 0, 0]);
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_ne!(machine.harts[1].csrs.mip & MIP_SSIP, 0);
        call_sbi(&mut machine, SBI_EXT_IPI, 0, [1 << 2, 0, 0, 0, 0, 0]);
        assert_eq!(machine.register(10), Some(SBI_ERR_INVALID_PARAM));

        let context = TranslationContext {
            satp: 0,
            permission_context: 0,
            privilege: Privilege::Supervisor,
            access: AccessType::Instruction,
        };
        machine.harts[1]
            .translation_cache
            .insert(DRAM_BASE, DRAM_BASE, context);
        call_sbi(
            &mut machine,
            SBI_EXT_RFENCE,
            1,
            [1 << 1, 0, DRAM_BASE, 4096, 0, 0],
        );
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(
            machine.harts[1]
                .translation_cache
                .lookup(DRAM_BASE, context),
            None
        );
        call_sbi(&mut machine, SBI_EXT_RFENCE, 3, [1 << 1, 0, 0, 0, 0, 0]);
        assert_eq!(machine.register(10), Some(SBI_ERR_NOT_SUPPORTED));

        machine.scheduler_quantum_remaining = 0;
        assert_eq!(machine.run_slice(1).instructions_retired, 1);
        assert_eq!(machine.active_hart_id(), 1);
        assert_eq!(machine.register(5), Some(1));
        call_sbi(&mut machine, SBI_EXT_TIME, 0, [77, 0, 0, 0, 0, 0]);
        assert_eq!(machine.mtimecmp, 77);
        machine.cpu.write(16, 1);
        machine.cpu.write(17, SBI_EXT_HSM);
        assert_eq!(machine.run_slice(1).instructions_retired, 0);
        assert_eq!(machine.lifecycle, HartLifecycle::Stopped);

        assert_eq!(machine.run_slice(1).instructions_retired, 1);
        assert_eq!(machine.active_hart_id(), 0);
        call_sbi(&mut machine, SBI_EXT_HSM, 2, [1, 0, 0, 0, 0, 0]);
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(machine.register(11), Some(SBI_HART_STATE_STOPPED));
        assert_eq!(machine.harts[1].mtimecmp, 77);

        call_sbi(&mut machine, SBI_EXT_HSM, 0, [1, DRAM_BASE + 1, 0, 0, 0, 0]);
        assert_eq!(machine.register(10), Some(SBI_ERR_INVALID_ADDRESS));
        call_sbi(
            &mut machine,
            SBI_EXT_HSM,
            0,
            [2, secondary_start, 0, 0, 0, 0],
        );
        assert_eq!(machine.register(10), Some(SBI_ERR_INVALID_PARAM));
        call_sbi(
            &mut machine,
            SBI_EXT_HSM,
            0,
            [1, secondary_start, 0xcafe, 0, 0, 0],
        );
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        let restarted = &machine.harts[1];
        assert_eq!(restarted.lifecycle, HartLifecycle::Started);
        assert_eq!(restarted.cpu.registers[11], 0xcafe);
        assert_eq!(restarted.mtimecmp, u64::MAX);
        assert_eq!(restarted.csrs.mip & MIP_SSIP, 0);
    }

    #[test]
    fn sbi_9p_exchange_pauses_until_the_exact_bounded_response_arrives() {
        let mut machine = Machine::new(MachineConfig {
            ram_bytes: MIN_LINUX_RAM_BYTES,
            max_console_bytes: 64,
        })
        .expect("valid Linux machine");
        let mut kernel = halfwords(&[0x0001]); // c.nop
        kernel.extend_from_slice(&words(&[0x0000_0073; 2]));
        machine
            .boot_linux(&kernel, &[], "earlycon=sbi")
            .expect("admit Linux boot");

        let request_address = DRAM_BASE + 0x1_0000;
        let response_address = DRAM_BASE + 0x1_1000;
        let request = [7, 0, 0, 0, 100, 1, 0];
        let response = [7, 0, 0, 0, 101, 1, 0];
        assert!(machine.devices.write_ram_slice(request_address, &request));
        machine.cpu.registers[10] = request_address;
        machine.cpu.registers[11] = request.len() as u64;
        machine.cpu.registers[12] = response_address;
        machine.cpu.registers[13] = 32;
        machine.cpu.registers[14] = 7;
        machine.cpu.registers[15] = 0;
        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_AOS_9P;

        let first = machine.run_slice(10);
        let SliceOutcome::HostRequest(host_request) = first.outcome.clone() else {
            panic!("expected 9P host request, got {:?}", first.outcome);
        };
        assert_eq!(first.steps_executed, 2);
        assert_eq!(first.instructions_retired, 1);
        assert_eq!(host_request.message, request);
        assert_eq!(host_request.channel, 7);
        assert_eq!(host_request.max_response_bytes, 32);
        assert_eq!(machine.pc(), LINUX_KERNEL_BASE + 6);

        let repeated = machine.run_slice(10);
        assert_eq!(repeated.outcome, first.outcome);
        assert_eq!(repeated.steps_executed, 0);
        assert_eq!(repeated.total_steps_executed, first.total_steps_executed);
        assert_eq!(machine.pc(), LINUX_KERNEL_BASE + 6);
        machine.cpu.write_float32(3, (-3.5_f32).to_bits());
        machine.cpu.write_float64(4, 13.25_f64.to_bits());
        machine.csrs.mstatus = (machine.csrs.mstatus & !MSTATUS_FS) | MSTATUS_FS_DIRTY;
        machine.csrs.fcsr = (3 << FRM_SHIFT) | FFLAGS_DZ | FFLAGS_NX;

        let checkpoint = machine
            .checkpoint_host_suspension()
            .expect("drained host suspension is checkpointable");
        assert_eq!(checkpoint.ram_bytes(), MIN_LINUX_RAM_BYTES);
        assert_eq!(checkpoint.pending_host_request(), &host_request);
        let binding = CheckpointBinding::new(
            CheckpointDigest::new([0x11; 32]),
            CheckpointDigest::new([0x22; 32]),
        );
        let encoded = checkpoint.encode(binding);
        assert!(encoded.len() < 128 * 1024, "zero RAM pages remain sparse");
        let decoded = MachineCheckpoint::decode(&encoded, binding).expect("decode checkpoint");
        assert_eq!(
            decoded.encode(binding),
            encoded,
            "decoded checkpoints must retain one canonical byte encoding"
        );
        let mut restored = decoded.into_machine();
        assert_eq!(restored.run_slice(10).outcome, first.outcome);
        assert_eq!(
            restored.physical_ram(LINUX_KERNEL_BASE + 6, 4),
            Some(0x0000_0073_u32.to_le_bytes().as_slice())
        );
        assert_eq!(restored.pc(), LINUX_KERNEL_BASE + 6);
        assert_eq!(restored.privilege(), Privilege::Supervisor);
        assert_eq!(restored.cpu.read_float32(3), (-3.5_f32).to_bits());
        assert_eq!(restored.cpu.read_float64(4), 13.25_f64.to_bits());
        assert_eq!(restored.csrs.fcsr, (3 << FRM_SHIFT) | FFLAGS_DZ | FFLAGS_NX);
        assert_eq!(restored.csrs.mstatus & MSTATUS_FS, MSTATUS_FS_DIRTY);

        let wrong_binding = CheckpointBinding::new(
            CheckpointDigest::new([0x33; 32]),
            CheckpointDigest::new([0x22; 32]),
        );
        assert!(matches!(
            MachineCheckpoint::decode(&encoded, wrong_binding),
            Err(CheckpointDecodeError::BindingMismatch)
        ));
        let mut corrupted = encoded.clone();
        let last = corrupted.last_mut().expect("encoded checkpoint digest");
        *last ^= 1;
        assert!(matches!(
            MachineCheckpoint::decode(&corrupted, binding),
            Err(CheckpointDecodeError::IntegrityMismatch)
        ));

        machine.push_console_input(b"principal-input");
        assert!(matches!(
            machine.checkpoint_host_suspension(),
            Err(CheckpointError::PendingConsoleInput { bytes: 15 })
        ));
        machine.devices.console_input.clear();
        machine
            .devices
            .push_console_output(b'X')
            .expect("test console byte");
        assert!(matches!(
            machine.checkpoint_host_suspension(),
            Err(CheckpointError::UndrainedConsoleOutput { bytes: 1 })
        ));
        assert_eq!(machine.devices.take_new_console(), b"X");
        machine.hart_controls[0].raise_interrupt(MIP_SSIP);
        assert!(matches!(
            machine.checkpoint_host_suspension(),
            Err(CheckpointError::PendingHartControl { hart_id: 0 })
        ));
        assert!(machine.apply_hart_control(0));

        assert_eq!(
            machine.complete_9p_request(HostRequestId(host_request.id.get() + 1), &response),
            Err(HostCompletionError::RequestIdMismatch {
                expected: host_request.id,
                actual: HostRequestId(host_request.id.get() + 1),
            })
        );
        assert_eq!(
            machine.complete_9p_request(host_request.id, &[0; 6]),
            Err(HostCompletionError::InvalidResponseBytes(6))
        );
        assert_eq!(
            machine.complete_9p_request(host_request.id, &[0; 33]),
            Err(HostCompletionError::ResponseTooLarge {
                response: 33,
                capacity: 32,
            })
        );

        machine
            .complete_9p_request(host_request.id, &response)
            .expect("complete admitted 9P request");
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(machine.register(11), Some(response.len() as u64));
        assert_eq!(
            machine.physical_ram(response_address, response.len()),
            Some(response.as_slice())
        );
        assert_eq!(
            machine.complete_9p_request(host_request.id, &response),
            Err(HostCompletionError::NoPendingRequest)
        );

        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_BASE;
        let resumed = machine.run_slice(1);
        assert_eq!(resumed.outcome, SliceOutcome::Yielded);
        assert_eq!(resumed.steps_executed, 1);
        assert_eq!(machine.register(11), Some(SBI_SPEC_VERSION_3_0));

        restored
            .complete_9p_request(host_request.id, &response)
            .expect("restored machine independently completes its host request");
        restored.cpu.registers[16] = 0;
        restored.cpu.registers[17] = SBI_EXT_BASE;
        let restored_resumed = restored.run_slice(1);
        assert_eq!(restored_resumed, resumed);
        assert_eq!(restored.register(11), Some(SBI_SPEC_VERSION_3_0));
    }

    #[test]
    fn prewarm_checkpoint_rejects_unstable_or_principal_bearing_state() {
        let mut machine = machine(64);
        machine
            .load_program(&RV64_SMOKE_PROGRAM)
            .expect("load smoke program");
        assert!(matches!(
            machine.checkpoint_host_suspension(),
            Err(CheckpointError::NoPendingHostRequest)
        ));

        let halted = machine.run_slice(64);
        assert!(matches!(halted.outcome, SliceOutcome::Halted(_)));
        assert!(matches!(
            machine.checkpoint_host_suspension(),
            Err(CheckpointError::NotRunnable)
        ));
    }

    #[test]
    fn multi_hart_9p_completion_returns_only_to_the_requesting_hart() {
        let mut machine = Machine::new_with_harts(
            MachineConfig {
                ram_bytes: 4096,
                max_console_bytes: 64,
            },
            2,
        )
        .expect("admit two harts");
        assert!(machine.set_hart_lifecycle(1, HartLifecycle::Started));
        assert!(machine.switch_active_hart(1));
        let request_address = DRAM_BASE + 0x100;
        let response_address = DRAM_BASE + 0x200;
        let request = [7, 0, 0, 0, 100, 1, 0];
        let response = [7, 0, 0, 0, 101, 1, 0];
        assert!(machine.devices.write_ram_slice(request_address, &request));
        machine.cpu.registers[10..16].copy_from_slice(&[
            request_address,
            request.len() as u64,
            response_address,
            32,
            1,
            0,
        ]);
        machine.cpu.write(16, 0);
        machine.cpu.write(17, SBI_EXT_AOS_9P);
        assert_eq!(machine.handle_sbi_call().expect("9P SBI call"), None);
        let request_id = machine
            .pending_9p_request
            .as_ref()
            .expect("pending request")
            .request
            .id;
        assert!(machine.switch_active_hart(0));
        machine.cpu.write(10, 0xdead);
        machine.firmware.enabled = true;

        let checkpoint = machine
            .checkpoint_host_suspension()
            .expect("multi-hart host suspension is checkpointable");
        let binding = CheckpointBinding::new(
            CheckpointDigest::new([0x44; 32]),
            CheckpointDigest::new([0x55; 32]),
        );
        let encoded = checkpoint.encode(binding);
        let decoded =
            MachineCheckpoint::decode(&encoded, binding).expect("decode multi-hart checkpoint");
        assert_eq!(
            decoded.encode(binding),
            encoded,
            "per-hart state ownership must not alter the durable format"
        );
        let mut restored = decoded.into_machine();
        assert_eq!(restored.hart_count(), 2);
        assert_eq!(restored.active_hart_id(), 0);
        assert_eq!(
            restored
                .pending_9p_request
                .as_ref()
                .expect("restored request")
                .hart_id,
            1
        );

        machine
            .complete_9p_request(request_id, &response)
            .expect("complete requesting hart");
        restored
            .complete_9p_request(request_id, &response)
            .expect("complete restored requesting hart");

        assert_eq!(machine.active_hart_id(), 1);
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(machine.register(11), Some(response.len() as u64));
        assert_eq!(machine.harts[0].cpu.registers[10], 0xdead);
        assert_eq!(restored.active_hart_id(), machine.active_hart_id());
        assert_eq!(restored.register(10), machine.register(10));
        assert_eq!(restored.harts[0].cpu.registers[10], 0xdead);
    }

    #[test]
    fn sbi_9p_exchange_rejects_unadmitted_buffers_and_reload_invalidates_waiter() {
        let mut machine = Machine::new(MachineConfig {
            ram_bytes: MIN_LINUX_RAM_BYTES,
            max_console_bytes: 64,
        })
        .expect("valid Linux machine");
        let kernel = words(&[0x0000_0073; 3]);
        machine
            .boot_linux(&kernel, &[], "earlycon=sbi")
            .expect("admit Linux boot");
        let request_address = DRAM_BASE + 0x1_0000;
        let response_address = DRAM_BASE + 0x1_1000;
        let request = [7, 0, 0, 0, 100, 1, 0];
        assert!(machine.devices.write_ram_slice(request_address, &request));

        machine.cpu.registers[10] = request_address;
        machine.cpu.registers[11] = 6;
        machine.cpu.registers[12] = response_address;
        machine.cpu.registers[13] = 32;
        machine.cpu.registers[14] = 1;
        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_AOS_9P;
        assert_eq!(machine.run_slice(1).outcome, SliceOutcome::Yielded);
        assert_eq!(machine.register(10), Some(SBI_ERR_INVALID_PARAM));

        machine.cpu.registers[10] = request_address;
        machine.cpu.registers[11] = request.len() as u64;
        machine.cpu.registers[12] = DRAM_BASE + MIN_LINUX_RAM_BYTES as u64;
        machine.cpu.registers[13] = 32;
        assert_eq!(machine.run_slice(1).outcome, SliceOutcome::Yielded);
        assert_eq!(machine.register(10), Some(SBI_ERR_INVALID_ADDRESS));

        machine.cpu.registers[10] = request_address;
        machine.cpu.registers[11] = request.len() as u64;
        machine.cpu.registers[12] = response_address;
        let SliceOutcome::HostRequest(first) = machine.run_slice(1).outcome else {
            panic!("expected admitted host request");
        };
        machine
            .boot_linux(&kernel, &[], "earlycon=sbi")
            .expect("replace Linux guest");
        assert_eq!(
            machine.fail_9p_request(first.id, HostRequestFailure::Failed),
            Err(HostCompletionError::NoPendingRequest)
        );

        assert!(machine.devices.write_ram_slice(request_address, &request));
        machine.cpu.registers[10] = request_address;
        machine.cpu.registers[11] = request.len() as u64;
        machine.cpu.registers[12] = response_address;
        machine.cpu.registers[13] = 32;
        machine.cpu.registers[14] = 1;
        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_AOS_9P;
        let SliceOutcome::HostRequest(replacement) = machine.run_slice(1).outcome else {
            panic!("expected replacement host request");
        };
        assert_ne!(replacement.id, first.id);
        machine
            .fail_9p_request(replacement.id, HostRequestFailure::Denied)
            .expect("deny current request");
        assert_eq!(machine.register(10), Some(SBI_ERR_DENIED));
        assert_eq!(machine.register(11), Some(0));
    }

    #[test]
    fn linux_boot_admission_rejects_small_ram_oversize_images_and_bootargs() {
        let mut small = machine(8);
        assert!(matches!(
            small.boot_linux(&[1], &[], ""),
            Err(MachineError::InvalidLinuxImages { .. })
        ));
        let mut admitted = Machine::new(MachineConfig {
            ram_bytes: MIN_LINUX_RAM_BYTES,
            max_console_bytes: 8,
        })
        .expect("valid Linux machine");
        assert_eq!(
            admitted.boot_linux(&[1], &[], &"x".repeat(MAX_BOOTARGS_BYTES + 1)),
            Err(MachineError::InvalidBootArgsBytes(MAX_BOOTARGS_BYTES + 1))
        );
        assert!(matches!(
            admitted.boot_linux(&vec![0; MIN_LINUX_RAM_BYTES], &[], ""),
            Err(MachineError::InvalidLinuxImages { .. })
        ));
    }
}
