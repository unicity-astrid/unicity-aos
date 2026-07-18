use aos_realm_machine::{LINUX_FDT_BASE, Machine, MachineConfig};
use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let Some(output) = env::args_os().nth(1) else {
        eprintln!("usage: dump_linux_fdt OUTPUT.dtb");
        return ExitCode::FAILURE;
    };
    let mut machine = Machine::new(MachineConfig {
        ram_bytes: 64 * 1024 * 1024,
        max_console_bytes: 64 * 1024,
    })
    .expect("fixed verification machine profile");
    let layout = machine
        .boot_linux(
            &0x0000_0073_u32.to_le_bytes(),
            b"aos-initramfs-verification",
            "earlycon=uart8250,mmio8,0x10000000 console=ttyS0 init=/init",
        )
        .expect("fixed verification Linux boot");
    let fdt = machine
        .physical_ram(LINUX_FDT_BASE, layout.fdt_bytes)
        .expect("generated FDT remains in admitted RAM");
    fs::write(output, fdt).expect("write generated FDT");
    ExitCode::SUCCESS
}
