//! Principal-resolved resource admission for AOS Realm.

use astrid_sdk::prelude::*;
use serde::Serialize;

/// Zero delegates guest-RAM sizing to Astrid's admitted compute envelope.
pub(crate) const DEFAULT_LINUX_MEMORY_BYTES: usize = 0;
pub(crate) const MAX_LINUX_MEMORY_BYTES: usize = 3 * 1024 * 1024 * 1024;
pub(crate) const MIN_LINUX_MEMORY_BYTES: usize = 512 * 1024 * 1024;
/// No additional inner instruction ceiling; Astrid's principal CPU and timeout
/// policy remains the outer enforcement boundary.
pub(crate) const DEFAULT_LINUX_MAX_STEPS: u64 = 0;
pub(crate) const MAX_LINUX_MAX_STEPS: u64 = 1_000_000_000_000;
pub(crate) const DEFAULT_LINUX_MAX_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_LINUX_MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// No additional guest `RLIMIT_FSIZE`; Astrid's principal storage quota remains
/// the outer enforcement boundary.
pub(crate) const DEFAULT_LINUX_MAX_FILE_BYTES: u64 = 0;
pub(crate) const MAX_LINUX_MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
/// No additional guest `RLIMIT_NPROC`; the principal's outer compute and
/// memory policy remains authoritative for the virtual machine.
pub(crate) const DEFAULT_LINUX_MAX_PROCESSES: u32 = 0;
pub(crate) const MAX_LINUX_MAX_PROCESSES: u32 = 65_536;
/// Zero derives the guest's logical CPU topology from Astrid's admitted
/// compute parallelism. The current single-worker interpreter caps auto mode
/// at two harts; explicit 1–64-hart topologies remain available for testing.
pub(crate) const DEFAULT_LINUX_VCPUS: u32 = 0;
pub(crate) const MAX_LINUX_VCPUS: u32 = aos_realm_machine::MAX_HARTS as u32;
const GUEST_PAGE_BYTES: usize = 4096;

const LINUX_MEMORY_BYTES_KEY: &str = "linux_memory_bytes";
const LINUX_MAX_STEPS_KEY: &str = "linux_max_steps";
const LINUX_MAX_OUTPUT_BYTES_KEY: &str = "linux_max_output_bytes";
const LINUX_MAX_FILE_BYTES_KEY: &str = "linux_max_file_bytes";
const LINUX_MAX_PROCESSES_KEY: &str = "linux_max_processes";
const LINUX_VCPUS_KEY: &str = "linux_vcpus";

/// Inner resources admitted to one principal-affine Realm machine.
///
/// Astrid's principal profile remains the outer authority. These values are
/// resolved from the invoking principal's capsule-config overlay on every
/// operation and independently bounded by capsule hard limits. A request
/// cannot enlarge the enclosing Wasmtime Store or escape its kernel meter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RealmResources {
    pub(crate) linux_memory_bytes: usize,
    /// Per-invocation guest step limit. Zero delegates to Astrid's outer CPU
    /// and timeout policy.
    pub(crate) linux_max_steps: u64,
    pub(crate) linux_max_output_bytes: usize,
    /// Per-file guest limit. Zero means no inner limit; it never disables the
    /// outer principal storage quota enforced by Astrid.
    pub(crate) linux_max_file_bytes: u64,
    /// Guest processes/threads per agent UID. Zero leaves the inherited Linux
    /// limit unchanged and delegates admission to Astrid's outer envelope.
    pub(crate) linux_max_processes: u32,
    pub(crate) linux_vcpus: u32,
}

impl Default for RealmResources {
    fn default() -> Self {
        Self {
            linux_memory_bytes: DEFAULT_LINUX_MEMORY_BYTES,
            linux_max_steps: DEFAULT_LINUX_MAX_STEPS,
            linux_max_output_bytes: DEFAULT_LINUX_MAX_OUTPUT_BYTES,
            linux_max_file_bytes: DEFAULT_LINUX_MAX_FILE_BYTES,
            linux_max_processes: DEFAULT_LINUX_MAX_PROCESSES,
            linux_vcpus: DEFAULT_LINUX_VCPUS,
        }
    }
}

impl RealmResources {
    pub(crate) const fn with_linux_memory_bytes(mut self, bytes: usize) -> Self {
        self.linux_memory_bytes = bytes;
        self
    }

    pub(crate) const fn with_linux_vcpus(mut self, count: u32) -> Self {
        self.linux_vcpus = count;
        self
    }

    pub(crate) const fn effective_max_steps(self) -> u64 {
        if self.linux_max_steps == 0 {
            u64::MAX
        } else {
            self.linux_max_steps
        }
    }

    pub(crate) fn load() -> Result<Self, SysError> {
        Self::from_values(env::var_opt)
    }

    fn from_values(
        mut read: impl FnMut(&str) -> Result<Option<String>, SysError>,
    ) -> Result<Self, SysError> {
        let linux_memory_bytes = parse_usize(
            LINUX_MEMORY_BYTES_KEY,
            read(LINUX_MEMORY_BYTES_KEY)?,
            DEFAULT_LINUX_MEMORY_BYTES,
            0,
            MAX_LINUX_MEMORY_BYTES,
        )?;
        if linux_memory_bytes != 0 && linux_memory_bytes < MIN_LINUX_MEMORY_BYTES {
            return Err(invalid_integer(
                LINUX_MEMORY_BYTES_KEY,
                &linux_memory_bytes.to_string(),
                u64::try_from(MIN_LINUX_MEMORY_BYTES).expect("Realm minimum fits u64"),
                u64::try_from(MAX_LINUX_MEMORY_BYTES).expect("Realm maximum fits u64"),
            ));
        }
        if linux_memory_bytes != 0 && !linux_memory_bytes.is_multiple_of(GUEST_PAGE_BYTES) {
            return Err(SysError::ApiError(format!(
                "Realm config `{LINUX_MEMORY_BYTES_KEY}` must be aligned to a {GUEST_PAGE_BYTES}-byte guest page"
            )));
        }

        Ok(Self {
            linux_memory_bytes,
            linux_max_steps: parse_u64(
                LINUX_MAX_STEPS_KEY,
                read(LINUX_MAX_STEPS_KEY)?,
                DEFAULT_LINUX_MAX_STEPS,
                0,
                MAX_LINUX_MAX_STEPS,
            )?,
            linux_max_output_bytes: parse_usize(
                LINUX_MAX_OUTPUT_BYTES_KEY,
                read(LINUX_MAX_OUTPUT_BYTES_KEY)?,
                DEFAULT_LINUX_MAX_OUTPUT_BYTES,
                1,
                MAX_LINUX_MAX_OUTPUT_BYTES,
            )?,
            linux_max_file_bytes: parse_u64(
                LINUX_MAX_FILE_BYTES_KEY,
                read(LINUX_MAX_FILE_BYTES_KEY)?,
                DEFAULT_LINUX_MAX_FILE_BYTES,
                0,
                MAX_LINUX_MAX_FILE_BYTES,
            )?,
            linux_max_processes: parse_u32(
                LINUX_MAX_PROCESSES_KEY,
                read(LINUX_MAX_PROCESSES_KEY)?,
                DEFAULT_LINUX_MAX_PROCESSES,
                0,
                MAX_LINUX_MAX_PROCESSES,
            )?,
            linux_vcpus: parse_u32(
                LINUX_VCPUS_KEY,
                read(LINUX_VCPUS_KEY)?,
                DEFAULT_LINUX_VCPUS,
                0,
                MAX_LINUX_VCPUS,
            )?,
        })
    }
}

fn parse_u32(
    key: &str,
    raw: Option<String>,
    default: u32,
    minimum: u32,
    maximum: u32,
) -> Result<u32, SysError> {
    let value = parse_u64(
        key,
        raw,
        u64::from(default),
        u64::from(minimum),
        u64::from(maximum),
    )?;
    u32::try_from(value).map_err(|_| {
        SysError::ApiError(format!(
            "Realm config `{key}` exceeds the supported CPU topology"
        ))
    })
}

fn parse_u64(
    key: &str,
    raw: Option<String>,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, SysError> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    let value = raw
        .trim()
        .trim_matches('"')
        .parse::<u64>()
        .map_err(|_| invalid_integer(key, &raw, minimum, maximum))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(invalid_integer(key, &raw, minimum, maximum));
    }
    Ok(value)
}

fn parse_usize(
    key: &str,
    raw: Option<String>,
    default: usize,
    minimum: usize,
    maximum: usize,
) -> Result<usize, SysError> {
    let value = parse_u64(
        key,
        raw,
        u64::try_from(default).expect("Realm default fits u64"),
        u64::try_from(minimum).expect("Realm minimum fits u64"),
        u64::try_from(maximum).expect("Realm maximum fits u64"),
    )?;
    usize::try_from(value).map_err(|_| {
        SysError::ApiError(format!(
            "Realm config `{key}` exceeds this platform's address space"
        ))
    })
}

fn invalid_integer(key: &str, raw: &str, minimum: u64, maximum: u64) -> SysError {
    SysError::ApiError(format!(
        "Realm config `{key}` must be an integer from {minimum} through {maximum}, got `{raw}`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn resources(values: &[(&str, &str)]) -> Result<RealmResources, SysError> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<BTreeMap<_, _>>();
        RealmResources::from_values(|key| Ok(values.get(key).cloned()))
    }

    #[test]
    fn defaults_delegate_optional_step_and_file_ceilings_to_outer_policy() {
        let defaults = resources(&[]).expect("defaults");
        assert_eq!(defaults, RealmResources::default());
        assert_eq!(defaults.linux_max_steps, 0);
        assert_eq!(defaults.linux_memory_bytes, 0);
        assert_eq!(defaults.effective_max_steps(), u64::MAX);
        assert_eq!(defaults.linux_max_file_bytes, 0);
        assert_eq!(defaults.linux_max_processes, 0);
        assert_eq!(defaults.linux_vcpus, 0);

        let installed_defaults = resources(&[
            (LINUX_MEMORY_BYTES_KEY, "0"),
            (LINUX_MAX_STEPS_KEY, "0"),
            (LINUX_MAX_FILE_BYTES_KEY, "0"),
            (LINUX_MAX_PROCESSES_KEY, "0"),
        ])
        .expect("serialized installation defaults");
        assert_eq!(installed_defaults, defaults);
    }

    #[test]
    fn principal_config_selects_a_bounded_page_aligned_envelope() {
        let selected = resources(&[
            (LINUX_MEMORY_BYTES_KEY, "536870912"),
            (LINUX_MAX_STEPS_KEY, "25000000"),
            (LINUX_MAX_OUTPUT_BYTES_KEY, "32768"),
            (LINUX_MAX_FILE_BYTES_KEY, "268435456"),
            (LINUX_MAX_PROCESSES_KEY, "2048"),
            (LINUX_VCPUS_KEY, "8"),
        ])
        .expect("bounded envelope");

        assert_eq!(selected.linux_memory_bytes, 512 * 1024 * 1024);
        assert_eq!(selected.linux_max_steps, 25_000_000);
        assert_eq!(selected.linux_max_output_bytes, 32 * 1024);
        assert_eq!(selected.linux_max_file_bytes, 256 * 1024 * 1024);
        assert_eq!(selected.linux_max_processes, 2048);
        assert_eq!(selected.linux_vcpus, 8);
    }

    #[test]
    fn malformed_or_excessive_config_fails_closed() {
        for values in [
            vec![(LINUX_MEMORY_BYTES_KEY, "unbounded")],
            vec![(LINUX_MEMORY_BYTES_KEY, "33554433")],
            vec![(LINUX_MEMORY_BYTES_KEY, "4294967296")],
            vec![(LINUX_MAX_STEPS_KEY, "1000000000001")],
            vec![(LINUX_MAX_OUTPUT_BYTES_KEY, "0")],
            vec![(LINUX_MAX_FILE_BYTES_KEY, "1099511627777")],
            vec![(LINUX_MAX_PROCESSES_KEY, "65537")],
            vec![(LINUX_VCPUS_KEY, "65")],
            vec![(LINUX_VCPUS_KEY, "many")],
        ] {
            assert!(resources(&values).is_err(), "accepted {values:?}");
        }
    }
}
