//! Principal-resolved resource admission for AOS Realm.

use astrid_sdk::prelude::*;
use serde::Serialize;

pub(crate) const DEFAULT_LINUX_MEMORY_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_LINUX_MEMORY_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MIN_LINUX_MEMORY_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const DEFAULT_LINUX_MAX_STEPS: u64 = 50_000_000;
pub(crate) const MAX_LINUX_MAX_STEPS: u64 = 50_000_000;
pub(crate) const DEFAULT_LINUX_MAX_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_LINUX_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const GUEST_PAGE_BYTES: usize = 4096;

const LINUX_MEMORY_BYTES_KEY: &str = "linux_memory_bytes";
const LINUX_MAX_STEPS_KEY: &str = "linux_max_steps";
const LINUX_MAX_OUTPUT_BYTES_KEY: &str = "linux_max_output_bytes";

/// Inner resources admitted to one principal-affine Realm machine.
///
/// Astrid's principal profile remains the outer authority. These values are
/// resolved from the invoking principal's capsule-config overlay on every
/// operation and independently bounded by capsule hard limits. A request
/// cannot enlarge the enclosing Wasmtime Store or escape its kernel meter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RealmResources {
    pub(crate) linux_memory_bytes: usize,
    pub(crate) linux_max_steps: u64,
    pub(crate) linux_max_output_bytes: usize,
    pub(crate) linux_vcpus: u32,
}

impl Default for RealmResources {
    fn default() -> Self {
        Self {
            linux_memory_bytes: DEFAULT_LINUX_MEMORY_BYTES,
            linux_max_steps: DEFAULT_LINUX_MAX_STEPS,
            linux_max_output_bytes: DEFAULT_LINUX_MAX_OUTPUT_BYTES,
            linux_vcpus: 1,
        }
    }
}

impl RealmResources {
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
            MIN_LINUX_MEMORY_BYTES,
            MAX_LINUX_MEMORY_BYTES,
        )?;
        if !linux_memory_bytes.is_multiple_of(GUEST_PAGE_BYTES) {
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
                1,
                MAX_LINUX_MAX_STEPS,
            )?,
            linux_max_output_bytes: parse_usize(
                LINUX_MAX_OUTPUT_BYTES_KEY,
                read(LINUX_MAX_OUTPUT_BYTES_KEY)?,
                DEFAULT_LINUX_MAX_OUTPUT_BYTES,
                1,
                MAX_LINUX_MAX_OUTPUT_BYTES,
            )?,
            linux_vcpus: 1,
        })
    }
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
    fn defaults_are_the_current_proven_envelope() {
        assert_eq!(resources(&[]).expect("defaults"), RealmResources::default());
    }

    #[test]
    fn principal_config_selects_a_bounded_page_aligned_envelope() {
        let selected = resources(&[
            (LINUX_MEMORY_BYTES_KEY, "67108864"),
            (LINUX_MAX_STEPS_KEY, "25000000"),
            (LINUX_MAX_OUTPUT_BYTES_KEY, "32768"),
        ])
        .expect("bounded envelope");

        assert_eq!(selected.linux_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(selected.linux_max_steps, 25_000_000);
        assert_eq!(selected.linux_max_output_bytes, 32 * 1024);
        assert_eq!(selected.linux_vcpus, 1);
    }

    #[test]
    fn malformed_or_excessive_config_fails_closed() {
        for values in [
            vec![(LINUX_MEMORY_BYTES_KEY, "unbounded")],
            vec![(LINUX_MEMORY_BYTES_KEY, "33554433")],
            vec![(LINUX_MEMORY_BYTES_KEY, "536870912")],
            vec![(LINUX_MAX_STEPS_KEY, "50000001")],
            vec![(LINUX_MAX_OUTPUT_BYTES_KEY, "0")],
        ] {
            assert!(resources(&values).is_err(), "accepted {values:?}");
        }
    }
}
