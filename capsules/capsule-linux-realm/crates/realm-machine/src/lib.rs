#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

//! A bounded, slice-driven RV64 machine for the AOS Realm Linux backend.
//!
//! This crate is intentionally below the Linux compatibility policy. It owns
//! guest CPU state, admitted RAM, and virtual hardware. The outer Realm owns
//! scheduling, authority, image admission, persistence, and all host effects.

mod fdt;

use fdt::{LinuxFdtConfig, build_linux_fdt};
use std::{collections::VecDeque, fmt};

/// Machine profile whose future device tree and Linux image are versioned together.
pub const MACHINE_MODEL: &str = "aos-rv64-virt-v0";

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
const MAX_RAM_BYTES: usize = 256 * 1024 * 1024;
const MAX_CONSOLE_BYTES: usize = 16 * 1024 * 1024;
const MIN_LINUX_RAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_BOOTARGS_BYTES: usize = 4096;
const LINUX_FDT_MAX_BYTES: usize = 64 * 1024;

const MSTATUS_SIE: u64 = 1 << 1;
const MSTATUS_MIE: u64 = 1 << 3;
const MSTATUS_SPIE: u64 = 1 << 5;
const MSTATUS_MPIE: u64 = 1 << 7;
const MSTATUS_SPP: u64 = 1 << 8;
const MSTATUS_MPP_SHIFT: u32 = 11;
const MSTATUS_MPP: u64 = 0b11 << MSTATUS_MPP_SHIFT;
const MSTATUS_MPRV: u64 = 1 << 17;
const MSTATUS_SUM: u64 = 1 << 18;
const MSTATUS_MXR: u64 = 1 << 19;
const MSTATUS_UXL_RV64: u64 = 0b10 << 32;
const MSTATUS_SXL_RV64: u64 = 0b10 << 34;
const MSTATUS_WRITABLE: u64 = MSTATUS_SIE
    | MSTATUS_MIE
    | MSTATUS_SPIE
    | MSTATUS_MPIE
    | MSTATUS_SPP
    | MSTATUS_MPP
    | MSTATUS_MPRV
    | MSTATUS_SUM
    | MSTATUS_MXR;
const SSTATUS_VISIBLE: u64 =
    MSTATUS_SIE | MSTATUS_SPIE | MSTATUS_SPP | MSTATUS_SUM | MSTATUS_MXR | MSTATUS_UXL_RV64;
const SSTATUS_WRITABLE: u64 = MSTATUS_SIE | MSTATUS_SPIE | MSTATUS_SPP | MSTATUS_SUM | MSTATUS_MXR;
const MISA_RV64_IMASU: u64 = (0b10 << 62) | (1 << 0) | (1 << 8) | (1 << 12) | (1 << 18) | (1 << 20);
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
const SBI_EXT_DBCN: u64 = 0x4442_434e;
const SBI_EXT_SRST: u64 = 0x5352_5354;
const SBI_SPEC_VERSION_3_0: u64 = 3 << 24;
const SBI_AOS_PRIVATE_IMPL_ID: u64 = 0x414f_5300;
const SBI_SUCCESS: u64 = 0;
const SBI_ERR_NOT_SUPPORTED: u64 = (-2_i64) as u64;
const SBI_ERR_INVALID_PARAM: u64 = (-3_i64) as u64;
const SBI_ERR_INVALID_ADDRESS: u64 = (-5_i64) as u64;

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
    /// The retained serial-output limit exceeds the hard machine cap.
    InvalidConsoleBytes(usize),
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
            Self::InvalidConsoleBytes(bytes) => write!(
                f,
                "console limit must not exceed {MAX_CONSOLE_BYTES} bytes, got {bytes}"
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

/// Result of one bounded scheduling slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SliceOutcome {
    /// The instruction budget ended while the guest remained runnable.
    Yielded,
    /// The guest wrote a terminal value to the standard finisher.
    Halted(HaltStatus),
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

#[derive(Clone, Debug)]
struct Cpu {
    registers: [u64; 32],
    pc: u64,
    privilege: Privilege,
}

impl Cpu {
    fn new() -> Self {
        Self {
            registers: [0; 32],
            pc: DRAM_BASE,
            privilege: Privilege::Machine,
        }
    }

    fn reset(&mut self) {
        self.registers.fill(0);
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
}

#[derive(Clone, Debug, Default)]
struct CsrFile {
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
            Csr::Sstatus => self.mstatus & SSTATUS_VISIBLE,
            Csr::Sie => self.mie & self.mideleg,
            Csr::Sip => self.mip & self.mideleg,
            Csr::Mideleg => self.mideleg,
            Csr::Mie => self.mie,
            Csr::Mip => self.mip,
            Csr::Stvec => self.stvec,
            Csr::Scounteren => self.scounteren,
            Csr::Sscratch => self.sscratch,
            Csr::Sepc => self.sepc & !0b11,
            Csr::Scause => self.scause,
            Csr::Stval => self.stval,
            Csr::Satp => self.satp,
            Csr::Mstatus => self.mstatus | MSTATUS_UXL_RV64 | MSTATUS_SXL_RV64,
            Csr::Misa => MISA_RV64_IMASU,
            Csr::Medeleg => self.medeleg,
            Csr::Mtvec => self.mtvec,
            Csr::Mcounteren => self.mcounteren,
            Csr::Mscratch => self.mscratch,
            Csr::Mepc => self.mepc & !0b11,
            Csr::Mcause => self.mcause,
            Csr::Mtval => self.mtval,
            Csr::Mhartid => 0,
            Csr::Cycle | Csr::Time | Csr::Instret | Csr::Mcycle | Csr::Minstret => 0,
        }
    }

    fn write(&mut self, csr: Csr, value: u64) {
        match csr {
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
            Csr::Sepc => self.sepc = value & !0b11,
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
            Csr::Mepc => self.mepc = value & !0b11,
            Csr::Mcause => self.mcause = value,
            Csr::Mtval => self.mtval = value,
            Csr::Mhartid => {}
            Csr::Cycle | Csr::Time | Csr::Instret => {}
            Csr::Mcycle | Csr::Minstret => {}
        }
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

#[derive(Clone, Copy, Debug, Default)]
struct SbiFirmware {
    enabled: bool,
}

#[derive(Debug)]
struct Devices {
    ram: Vec<u8>,
    mtime: u64,
    mtimecmp: u64,
    msip: bool,
    console_input: VecDeque<u8>,
    console_output: Vec<u8>,
    console_reported: usize,
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
    fn new(config: MachineConfig) -> Self {
        Self {
            ram: vec![0; config.ram_bytes],
            mtime: 0,
            mtimecmp: u64::MAX,
            msip: false,
            console_input: VecDeque::new(),
            console_output: Vec::new(),
            console_reported: 0,
            max_console_bytes: config.max_console_bytes,
        }
    }

    fn reset(&mut self) {
        self.ram.fill(0);
        self.mtime = 0;
        self.mtimecmp = u64::MAX;
        self.msip = false;
        self.console_input.clear();
        self.console_output.clear();
        self.console_reported = 0;
    }

    fn load_program(&mut self, program: &[u8]) {
        self.ram[..program.len()].copy_from_slice(program);
    }

    fn take_new_console(&mut self) -> Vec<u8> {
        let bytes = self.console_output[self.console_reported..].to_vec();
        self.console_reported = self.console_output.len();
        bytes
    }

    fn read(&mut self, address: u64, bytes: u8) -> Result<u64, MachineTrap> {
        if let Some(offset) = address
            .checked_sub(CLINT_BASE)
            .filter(|offset| *offset < CLINT_SIZE)
        {
            return match (offset, bytes) {
                (CLINT_MSIP, 4) => Ok(u64::from(self.msip)),
                (CLINT_MTIMECMP, 8) => Ok(self.mtimecmp),
                (CLINT_MTIME, 8) => Ok(self.mtime),
                _ => Err(MachineTrap::LoadAccessFault { address, bytes }),
            };
        }
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
        let mut value = 0_u64;
        for (shift, byte) in self.ram[range].iter().enumerate() {
            value |= u64::from(*byte) << (shift * 8);
        }
        Ok(value)
    }

    fn write(
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
                    self.mtime = value;
                    Ok(None)
                }
                _ => Err(MachineTrap::StoreAccessFault { address, bytes }),
            };
        }
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
        for (shift, byte) in self.ram[range].iter_mut().enumerate() {
            *byte = (value >> (shift * 8)) as u8;
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
        let mut value = 0_u64;
        for (shift, byte) in self.ram[self.ram_range(address, bytes)?].iter().enumerate() {
            value |= u64::from(*byte) << (shift * 8);
        }
        Some(value)
    }

    fn write_ram(&mut self, address: u64, value: u64, bytes: u8) -> bool {
        let Some(range) = self.ram_range(address, bytes) else {
            return false;
        };
        for (shift, byte) in self.ram[range].iter_mut().enumerate() {
            *byte = (value >> (shift * 8)) as u8;
        }
        true
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
        self.ram[range].copy_from_slice(bytes);
        true
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

    fn hardware_interrupts(&self) -> u64 {
        let mut pending = 0;
        if self.msip {
            pending |= MIP_MSIP;
        }
        if self.mtime >= self.mtimecmp {
            pending |= MIP_MTIP;
        }
        pending
    }
}

/// An admitted RV64 machine whose execution can only advance in explicit slices.
#[derive(Debug)]
pub struct Machine {
    config: MachineConfig,
    cpu: Cpu,
    csrs: CsrFile,
    devices: Devices,
    state: RunState,
    steps_executed: u64,
    instructions_retired: u64,
    cycle: u64,
    instret: u64,
    reservation: Option<(u64, u8)>,
    firmware: SbiFirmware,
}

impl Machine {
    /// Admit resources and construct a reset RV64 machine.
    pub fn new(config: MachineConfig) -> Result<Self, MachineError> {
        if !(MIN_RAM_BYTES..=MAX_RAM_BYTES).contains(&config.ram_bytes)
            || !config.ram_bytes.is_multiple_of(MIN_RAM_BYTES)
        {
            return Err(MachineError::InvalidRamBytes(config.ram_bytes));
        }
        if config.max_console_bytes > MAX_CONSOLE_BYTES {
            return Err(MachineError::InvalidConsoleBytes(config.max_console_bytes));
        }
        Ok(Self {
            config,
            cpu: Cpu::new(),
            csrs: CsrFile::default(),
            devices: Devices::new(config),
            state: RunState::Runnable,
            steps_executed: 0,
            instructions_retired: 0,
            cycle: 0,
            instret: 0,
            reservation: None,
            firmware: SbiFirmware::default(),
        })
    }

    /// Reset the machine and copy a raw RV64 image to [`DRAM_BASE`].
    pub fn load_program(&mut self, program: &[u8]) -> Result<(), MachineError> {
        if program.is_empty() || program.len() > self.config.ram_bytes {
            return Err(MachineError::InvalidProgramBytes {
                image: program.len(),
                ram: self.config.ram_bytes,
            });
        }
        self.cpu.reset();
        self.csrs.reset();
        self.devices.reset();
        self.devices.load_program(program);
        self.state = RunState::Runnable;
        self.steps_executed = 0;
        self.instructions_retired = 0;
        self.cycle = 0;
        self.instret = 0;
        self.reservation = None;
        self.firmware = SbiFirmware::default();
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
        let invalid_images = || MachineError::InvalidLinuxImages {
            kernel: kernel.len(),
            initramfs: initramfs.len(),
            ram: self.config.ram_bytes,
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

        self.cpu.reset();
        self.csrs.reset();
        self.devices.reset();
        if !self.devices.write_ram_slice(LINUX_KERNEL_BASE, kernel)
            || !self.devices.write_ram_slice(LINUX_FDT_BASE, &fdt)
            || initrd_start.is_some_and(|start| !self.devices.write_ram_slice(start, initramfs))
        {
            return Err(invalid_images());
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
        self.cycle = 0;
        self.instret = 0;
        self.reservation = None;
        self.firmware.enabled = true;

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
            .map(|range| &self.devices.ram[range])
    }

    /// Read one architectural integer register. Register zero is always zero.
    #[must_use]
    pub fn register(&self, register: usize) -> Option<u64> {
        self.cpu.registers.get(register).copied()
    }

    /// Current guest program counter.
    #[must_use]
    pub const fn pc(&self) -> u64 {
        self.cpu.pc
    }

    /// Current guest privilege level.
    #[must_use]
    pub const fn privilege(&self) -> Privilege {
        self.cpu.privilege
    }

    /// Read an implemented architectural CSR without bypassing the typed set.
    #[must_use]
    pub fn csr(&self, csr: Csr) -> u64 {
        self.read_csr(csr)
    }

    /// Run at most `instruction_budget` instructions and return control to the Realm.
    pub fn run_slice(&mut self, instruction_budget: u64) -> SliceReport {
        let mut steps = 0_u64;
        let mut retired = 0_u64;
        while steps < instruction_budget && matches!(self.state, RunState::Runnable) {
            match self.step() {
                Ok(effect) => {
                    steps = steps.saturating_add(1);
                    self.steps_executed = self.steps_executed.saturating_add(1);
                    self.cycle = self.cycle.wrapping_add(1);
                    self.devices.tick();
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
                        self.take_exception(cause, value, self.cpu.pc);
                        self.cpu.registers[0] = 0;
                    } else {
                        self.state = RunState::Trapped(trap);
                    }
                }
            }
        }

        let outcome = match &self.state {
            RunState::Runnable => SliceOutcome::Yielded,
            RunState::Halted(status) => SliceOutcome::Halted(*status),
            RunState::Trapped(trap) => SliceOutcome::Trapped(trap.clone()),
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
        if pc & 3 != 0 {
            return Err(MachineTrap::InstructionAddressMisaligned { address: pc });
        }
        let physical_pc = self.translate(pc, AccessType::Instruction)?;
        let instruction = self
            .devices
            .read(physical_pc, 4)
            .map_err(|_| MachineTrap::InstructionAccessFault { address: pc })?
            as u32;
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
                    .devices
                    .read(physical, bytes)
                    .map_err(|_| MachineTrap::LoadAccessFault { address, bytes })?;
                let value = if signed {
                    sign_extend(value, u32::from(bytes) * 8)
                } else {
                    value
                };
                self.cpu.write(rd, value);
            }
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
                halt = self
                    .devices
                    .write(physical, self.cpu.read(rs2), bytes)
                    .map_err(|trap| match trap {
                        MachineTrap::ConsoleLimit { .. } => trap,
                        _ => MachineTrap::StoreAccessFault { address, bytes },
                    })?;
            }
            0x2f => halt = self.execute_atomic(instruction, rd, rs1, rs2, funct3, pc)?,
            0x33 => self.execute_op(instruction, rd, rs1, rs2, funct3, funct7, pc)?,
            0x37 => self.cpu.write(rd, immediate_u(instruction)),
            0x3b => self.execute_op_32(instruction, rd, rs1, rs2, funct3, funct7, pc)?,
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

    fn translate(&mut self, address: u64, access: AccessType) -> Result<u64, MachineTrap> {
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
            let mut pte = self
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
                pte |= required_ad;
                if !self.devices.write_ram(pte_address, pte, 8) {
                    return Err(access.access_fault(address, 8));
                }
            }

            let mut physical_ppn = pte_ppn;
            if level >= 1 {
                physical_ppn = (physical_ppn & !0x1ff) | vpn[0];
            }
            if level == 2 {
                physical_ppn = (physical_ppn & !0x3_ffff) | (vpn[1] << 9) | vpn[0];
            }
            return Ok((physical_ppn << 12) | (address & 0xfff));
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
            let value =
                self.devices
                    .read(physical, bytes)
                    .map_err(|_| MachineTrap::LoadAccessFault {
                        address: virtual_address,
                        bytes,
                    })?;
            self.reservation = Some((physical, bytes));
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
            let succeeds = self.reservation == Some((physical, bytes));
            self.reservation = None;
            if !succeeds {
                self.cpu.write(rd, 1);
                return Ok(None);
            }
            let halt = self
                .devices
                .write(physical, self.cpu.read(rs2), bytes)
                .map_err(|trap| map_store_fault(trap, virtual_address, bytes))?;
            self.cpu.write(rd, 0);
            return Ok(halt);
        }

        let old =
            self.devices
                .read(physical, bytes)
                .map_err(|_| MachineTrap::StoreAccessFault {
                    address: virtual_address,
                    bytes,
                })?;
        let rhs = self.cpu.read(rs2);
        let value =
            atomic_result(operation, old, rhs, bytes).ok_or_else(|| illegal(pc, instruction))?;
        self.invalidate_reservation(physical, bytes);
        let halt = self
            .devices
            .write(physical, value, bytes)
            .map_err(|trap| map_store_fault(trap, virtual_address, bytes))?;
        self.cpu.write(
            rd,
            if bytes == 4 {
                sign_extend(old, 32)
            } else {
                old
            },
        );
        Ok(halt)
    }

    fn invalidate_reservation(&mut self, address: u64, bytes: u8) {
        if let Some((reserved, reserved_bytes)) = self.reservation {
            let end = address.saturating_add(u64::from(bytes));
            let reserved_end = reserved.saturating_add(u64::from(reserved_bytes));
            if address < reserved_end && reserved < end {
                self.reservation = None;
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
            _ => self.csrs.read(csr),
        }
    }

    fn write_csr(&mut self, csr: Csr, value: u64) {
        match csr {
            Csr::Mcycle => self.cycle = value,
            Csr::Minstret => self.instret = value,
            _ => self.csrs.write(csr, value),
        }
    }

    fn refresh_hardware_interrupts(&mut self) {
        let mut hardware = self.devices.hardware_interrupts();
        if self.firmware.enabled && hardware & MIP_MTIP != 0 {
            hardware = (hardware & !MIP_MTIP) | MIP_STIP;
        }
        let hardware_mask = MIP_MSIP | MIP_MTIP | if self.firmware.enabled { MIP_STIP } else { 0 };
        self.csrs.mip = (self.csrs.mip & !hardware_mask) | hardware;
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
        let (error, value) = match (extension, function) {
            (SBI_EXT_BASE, 0) => (SBI_SUCCESS, SBI_SPEC_VERSION_3_0),
            (SBI_EXT_BASE, 1) => (SBI_SUCCESS, SBI_AOS_PRIVATE_IMPL_ID),
            (SBI_EXT_BASE, 2) => (SBI_SUCCESS, 1),
            (SBI_EXT_BASE, 3) => (
                SBI_SUCCESS,
                u64::from(matches!(
                    arguments[0],
                    SBI_EXT_BASE | SBI_EXT_TIME | SBI_EXT_DBCN | SBI_EXT_SRST
                )),
            ),
            (SBI_EXT_BASE, 4..=6) => (SBI_SUCCESS, 0),
            (SBI_EXT_TIME, 0) => {
                self.devices.mtimecmp = arguments[0];
                self.csrs.mip &= !MIP_STIP;
                (SBI_SUCCESS, 0)
            }
            (SBI_EXT_DBCN, 0) => self.sbi_debug_console_write(arguments)?,
            (SBI_EXT_DBCN, 1) => self.sbi_debug_console_read(arguments),
            (SBI_EXT_DBCN, 2) => {
                self.devices.push_console_output(arguments[0] as u8)?;
                (SBI_SUCCESS, 0)
            }
            (SBI_EXT_SRST, 0) if arguments[0] <= 2 && arguments[1] <= 1 => {
                halt = Some(HaltStatus {
                    passed: arguments[0] == 0 && arguments[1] == 0,
                    code: ((arguments[0] as u32) << 16) | arguments[1] as u32,
                });
                (SBI_SUCCESS, 0)
            }
            (SBI_EXT_SRST, 0) => (SBI_ERR_INVALID_PARAM, 0),
            _ => (SBI_ERR_NOT_SUPPORTED, 0),
        };
        self.cpu.write(10, error);
        self.cpu.write(11, value);
        Ok(halt)
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
        let output = self.devices.ram[range.start..range.start + written].to_vec();
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
            self.devices.ram[range.start + offset] = self
                .devices
                .console_input
                .pop_front()
                .expect("length checked console input");
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
        if delegated {
            self.csrs.sepc = pc & !0b11;
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
            self.csrs.mepc = pc & !0b11;
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
        if origin != Privilege::Machine && self.csrs.medeleg & (1 << cause) != 0 {
            self.csrs.sepc = pc & !0b11;
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
            self.csrs.mepc = pc & !0b11;
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
    if address.is_multiple_of(4) {
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
        supervisor.devices.ram[offset..offset + 4].copy_from_slice(&forbidden.to_le_bytes());
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
        supervisor.devices.ram[offset..offset + 4].copy_from_slice(&mret.to_le_bytes());
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
    fn misaligned_jump_traps_without_writing_the_link_register() {
        let mut machine = machine(8);
        // jal x1, +2. The RV64I profile has IALIGN=32, so the jump itself
        // traps and must not commit x1 before control returns to the Realm.
        machine
            .load_program(&0x0020_00ef_u32.to_le_bytes())
            .expect("load program");

        let report = machine.run_slice(1);
        assert_eq!(report.outcome, SliceOutcome::Yielded);
        assert_eq!(report.instructions_retired, 0);
        assert_eq!(machine.register(1), Some(0));
        assert_eq!(machine.pc(), 0);
        assert_eq!(machine.csr(Csr::Mcause), 0);
        assert_eq!(machine.csr(Csr::Mtval), DRAM_BASE + 2);
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
    fn sfence_vma_is_privileged_and_retires_without_a_software_tlb() {
        let mut machine = paged_machine();
        let virtual_address = 0x4000_0000;
        let physical = DRAM_BASE + 0x4000;
        let sfence_vma = 0x1200_0073_u32;
        assert!(
            machine
                .devices
                .write_ram(physical, u64::from(sfence_vma), 4)
        );
        let leaf = install_4k_mapping(
            &mut machine,
            virtual_address,
            physical,
            PTE_VALID | PTE_READ | PTE_EXECUTE,
        );
        machine.cpu.pc = virtual_address;
        machine.cpu.privilege = Privilege::Supervisor;
        let report = machine.run_slice(1);
        assert_eq!(report.instructions_retired, 1);
        assert_eq!(machine.pc(), virtual_address + 4);

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
        machine.devices.mtimecmp = 2;
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
        assert_eq!(machine.devices.read(CLINT_BASE + CLINT_MTIME, 8), Ok(0));
        assert_eq!(
            machine.devices.write(CLINT_BASE + CLINT_MTIMECMP, 3, 8),
            Ok(None)
        );
        assert_eq!(
            machine.devices.write(CLINT_BASE + CLINT_MSIP, 1, 4),
            Ok(None)
        );
        assert_eq!(machine.devices.hardware_interrupts(), MIP_MSIP);
        machine.devices.tick();
        machine.devices.tick();
        machine.devices.tick();
        assert_eq!(machine.devices.hardware_interrupts(), MIP_MSIP | MIP_MTIP);
        assert_eq!(
            machine.devices.read(CLINT_BASE + CLINT_MTIME, 4),
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
                .map(|range| machine.devices.ram[range].to_vec()),
            Some(vec![0xd0, 0x0d, 0xfe, 0xed])
        );
        assert_eq!(
            machine
                .devices
                .ram_range_len(layout.initrd_start.expect("initrd start"), initramfs.len())
                .map(|range| machine.devices.ram[range].to_vec()),
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
            .boot_linux(&words(&[0x0000_0073; 4]), &[], "earlycon=sbi")
            .expect("admit Linux boot");

        machine.cpu.registers[16] = 0;
        machine.cpu.registers[17] = SBI_EXT_BASE;
        let base = machine.run_slice(1);
        assert_eq!(base.instructions_retired, 0);
        assert_eq!(machine.register(10), Some(SBI_SUCCESS));
        assert_eq!(machine.register(11), Some(SBI_SPEC_VERSION_3_0));

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
        assert_eq!(machine.devices.mtimecmp, deadline);
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
