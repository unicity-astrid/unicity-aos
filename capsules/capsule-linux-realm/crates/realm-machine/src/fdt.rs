//! Deterministic flattened device tree for the `aos-rv64-virt-v0` profile.

use std::collections::BTreeMap;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_VERSION: u32 = 17;
const FDT_LAST_COMPATIBLE_VERSION: u32 = 16;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_END: u32 = 9;
const HEADER_BYTES: usize = 40;
const RESERVATION_MAP_BYTES: usize = 16;

/// Deterministic counter frequency advertised to Linux. Guest time advances by
/// one tick per charged machine step; this value defines the guest-visible unit,
/// not an ambient host-clock coupling.
pub(crate) const TIMEBASE_FREQUENCY: u32 = 10_000_000;

pub(crate) struct LinuxFdtConfig<'a> {
    pub dram_base: u64,
    pub ram_bytes: u64,
    pub uart_base: u64,
    pub uart_bytes: u64,
    pub bootargs: &'a str,
    pub initrd_start: Option<u64>,
    pub initrd_end: Option<u64>,
}

pub(crate) fn build_linux_fdt(config: &LinuxFdtConfig<'_>) -> Vec<u8> {
    let mut tree = Tree::default();
    tree.begin_node("");
    tree.property_u32("#address-cells", 2);
    tree.property_u32("#size-cells", 2);
    tree.property_string("compatible", "aos,aos-rv64-virt-v0");
    tree.property_string("model", "AOS RV64 virtual machine v0");

    tree.begin_node("chosen");
    tree.property_string("bootargs", config.bootargs);
    tree.property_string("stdout-path", "serial0:115200n8");
    if let (Some(start), Some(end)) = (config.initrd_start, config.initrd_end) {
        tree.property_u64("linux,initrd-start", start);
        tree.property_u64("linux,initrd-end", end);
    }
    tree.end_node();

    tree.begin_node("aliases");
    tree.property_string("serial0", "/soc/uart@10000000");
    tree.end_node();

    tree.begin_node("cpus");
    tree.property_u32("#address-cells", 1);
    tree.property_u32("#size-cells", 0);
    tree.property_u32("timebase-frequency", TIMEBASE_FREQUENCY);
    tree.begin_node("cpu@0");
    tree.property_string("device_type", "cpu");
    tree.property_u32("reg", 0);
    tree.property_string("status", "okay");
    tree.property_string("compatible", "riscv");
    tree.property_string("riscv,isa", "rv64ima_zicsr_zifencei");
    tree.property_string("riscv,isa-base", "rv64i");
    tree.property_string_list(
        "riscv,isa-extensions",
        &["i", "m", "a", "zicsr", "zifencei"],
    );
    tree.property_string("mmu-type", "riscv,sv39");
    tree.begin_node("interrupt-controller");
    tree.property_u32("#interrupt-cells", 1);
    tree.property_empty("interrupt-controller");
    tree.property_string("compatible", "riscv,cpu-intc");
    tree.property_u32("phandle", 1);
    tree.end_node();
    tree.end_node();
    tree.end_node();

    tree.begin_node(&format!("memory@{:x}", config.dram_base));
    tree.property_string("device_type", "memory");
    tree.property_u64_pair("reg", config.dram_base, config.ram_bytes);
    tree.end_node();

    tree.begin_node("soc");
    tree.property_u32("#address-cells", 2);
    tree.property_u32("#size-cells", 2);
    tree.property_string("compatible", "simple-bus");
    tree.property_empty("ranges");
    tree.begin_node(&format!("uart@{:x}", config.uart_base));
    tree.property_string("compatible", "ns16550a");
    tree.property_u64_pair("reg", config.uart_base, config.uart_bytes);
    tree.property_u32("clock-frequency", 3_686_400);
    tree.property_u32("current-speed", 115_200);
    tree.property_u32("reg-shift", 0);
    tree.property_u32("reg-io-width", 1);
    tree.end_node();
    tree.end_node();
    tree.end_node();
    tree.finish()
}

#[derive(Default)]
struct Tree {
    structure: Vec<u8>,
    strings: Vec<u8>,
    string_offsets: BTreeMap<String, u32>,
}

impl Tree {
    fn begin_node(&mut self, name: &str) {
        self.token(FDT_BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        pad_to_four(&mut self.structure);
    }

    fn end_node(&mut self) {
        self.token(FDT_END_NODE);
    }

    fn property_empty(&mut self, name: &str) {
        self.property(name, &[]);
    }

    fn property_string(&mut self, name: &str, value: &str) {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        self.property(name, &bytes);
    }

    fn property_string_list(&mut self, name: &str, values: &[&str]) {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        self.property(name, &bytes);
    }

    fn property_u32(&mut self, name: &str, value: u32) {
        self.property(name, &value.to_be_bytes());
    }

    fn property_u64(&mut self, name: &str, value: u64) {
        self.property(name, &value.to_be_bytes());
    }

    fn property_u64_pair(&mut self, name: &str, first: u64, second: u64) {
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&first.to_be_bytes());
        bytes[8..].copy_from_slice(&second.to_be_bytes());
        self.property(name, &bytes);
    }

    fn property(&mut self, name: &str, value: &[u8]) {
        let offset = self.string_offset(name);
        self.token(FDT_PROP);
        self.structure.extend_from_slice(
            &u32::try_from(value.len())
                .expect("bounded FDT property length")
                .to_be_bytes(),
        );
        self.structure.extend_from_slice(&offset.to_be_bytes());
        self.structure.extend_from_slice(value);
        pad_to_four(&mut self.structure);
    }

    fn string_offset(&mut self, name: &str) -> u32 {
        if let Some(offset) = self.string_offsets.get(name) {
            return *offset;
        }
        let offset = u32::try_from(self.strings.len()).expect("bounded FDT strings block");
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        self.string_offsets.insert(name.to_string(), offset);
        offset
    }

    fn token(&mut self, token: u32) {
        self.structure.extend_from_slice(&token.to_be_bytes());
    }

    fn finish(mut self) -> Vec<u8> {
        self.token(FDT_END);
        let structure_offset = HEADER_BYTES + RESERVATION_MAP_BYTES;
        let strings_offset = structure_offset + self.structure.len();
        let total_bytes = strings_offset + self.strings.len();
        let mut bytes = Vec::with_capacity(total_bytes);
        for value in [
            FDT_MAGIC,
            u32::try_from(total_bytes).expect("bounded FDT total size"),
            u32::try_from(structure_offset).expect("fixed structure offset"),
            u32::try_from(strings_offset).expect("bounded strings offset"),
            u32::try_from(HEADER_BYTES).expect("fixed reservation-map offset"),
            FDT_VERSION,
            FDT_LAST_COMPATIBLE_VERSION,
            0,
            u32::try_from(self.strings.len()).expect("bounded FDT strings size"),
            u32::try_from(self.structure.len()).expect("bounded FDT structure size"),
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(&[0; RESERVATION_MAP_BYTES]);
        bytes.extend_from_slice(&self.structure);
        bytes.extend_from_slice(&self.strings);
        bytes
    }
}

fn pad_to_four(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tree_has_exact_header_offsets_and_linux_contract_strings() {
        let tree = build_linux_fdt(&LinuxFdtConfig {
            dram_base: 0x8000_0000,
            ram_bytes: 64 * 1024 * 1024,
            uart_base: 0x1000_0000,
            uart_bytes: 0x100,
            bootargs: "console=ttyS0 init=/init",
            initrd_start: Some(0x8080_0000),
            initrd_end: Some(0x8090_0000),
        });
        let word = |offset: usize| {
            u32::from_be_bytes(tree[offset..offset + 4].try_into().expect("header word"))
        };
        assert_eq!(word(0), FDT_MAGIC);
        assert_eq!(word(4) as usize, tree.len());
        assert_eq!(word(8), (HEADER_BYTES + RESERVATION_MAP_BYTES) as u32);
        assert_eq!(word(16), HEADER_BYTES as u32);
        assert_eq!(word(20), FDT_VERSION);
        assert_eq!(word(24), FDT_LAST_COMPATIBLE_VERSION);
        assert_eq!(
            &tree[HEADER_BYTES..HEADER_BYTES + RESERVATION_MAP_BYTES],
            &[0; 16]
        );
        for expected in [
            b"aos,aos-rv64-virt-v0\0".as_slice(),
            b"rv64ima_zicsr_zifencei\0".as_slice(),
            b"riscv,sv39\0".as_slice(),
            b"console=ttyS0 init=/init\0".as_slice(),
            b"ns16550a\0".as_slice(),
        ] {
            assert!(
                tree.windows(expected.len())
                    .any(|window| window == expected)
            );
        }
    }
}
