//! Native AOS status over the runtime's typed local control operation.

use std::fs::{self, OpenOptions};
use std::io;
use std::time::Duration;

use astrid_core::PrincipalId;
use astrid_core::kernel_api::{DaemonStatus, KernelRequest, KernelResponse};
use astrid_uplink::KernelClient;
use fs2::FileExt;
use serde::Serialize;

use crate::AosHome;

const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_COMPATIBILITY: &str = include_str!("../../../release/runtime-compatibility.toml");

/// Product status derived from the typed runtime status response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AosStatus {
    pub state: &'static str,
    pub pid: u32,
    pub uptime_secs: u64,
    pub runtime_version: String,
    pub ephemeral: bool,
    pub connected_clients: u32,
    pub loaded_capsules: Vec<String>,
}

impl From<DaemonStatus> for AosStatus {
    fn from(status: DaemonStatus) -> Self {
        Self {
            state: "running",
            pid: status.pid,
            uptime_secs: status.uptime_secs,
            runtime_version: status.version,
            ephemeral: status.ephemeral,
            connected_clients: status.connected_clients,
            loaded_capsules: status.loaded_capsules,
        }
    }
}

impl AosStatus {
    fn stopped() -> Result<Self, String> {
        let compatibility = RUNTIME_COMPATIBILITY
            .parse::<toml::Value>()
            .map_err(|error| format!("embedded runtime compatibility is invalid: {error}"))?;
        let runtime_version = compatibility
            .get("runtime")
            .and_then(|runtime| runtime.get("version"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| "embedded runtime compatibility has no runtime version".to_owned())?
            .to_owned();
        Ok(Self {
            state: "stopped",
            pid: 0,
            uptime_secs: 0,
            runtime_version,
            ephemeral: false,
            connected_clients: 0,
            loaded_capsules: Vec::new(),
        })
    }
}

/// Read status through the typed authenticated local control client.
pub async fn read(home: &AosHome) -> Result<AosStatus, String> {
    let connection = tokio::time::timeout(
        STATUS_TIMEOUT,
        KernelClient::connect(PrincipalId::default()),
    )
    .await
    .map_err(|_| "connection timed out".to_owned())
    .and_then(|result| {
        result.map_err(|error| format!("could not connect to the local runtime: {error}"))
    });
    let mut client = match connection {
        Ok(client) => client,
        Err(connection_error) => {
            return stopped_status(home)
                .map_err(|state_error| format!("{connection_error}; {state_error}"));
        }
    };

    let response = tokio::time::timeout(STATUS_TIMEOUT, client.request(KernelRequest::GetStatus))
        .await
        .map_err(|_| "status request timed out".to_owned())?
        .map_err(|error| format!("status request failed: {error}"))?;

    match response {
        KernelResponse::Status(status) => Ok(status.into()),
        KernelResponse::Error(error) => Err(error),
        _ => Err("runtime returned an unexpected status response".to_owned()),
    }
}

fn stopped_status(home: &AosHome) -> Result<AosStatus, String> {
    let run_dir = home.runtime_home().join("run");
    for marker in ["system.sock", "system.pid", "system.ready", "system.token"] {
        match fs::symlink_metadata(run_dir.join(marker)) {
            Ok(_) => {
                return Err(format!(
                    "runtime coordination marker {marker} is still present"
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect runtime marker {marker}: {error}"
                ));
            }
        }
    }

    let lock_path = run_dir.join("system.lock");
    let lock_metadata = match fs::symlink_metadata(&lock_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return AosStatus::stopped(),
        Err(error) => return Err(format!("could not inspect runtime lock: {error}")),
    };
    if lock_metadata.file_type().is_symlink() || !lock_metadata.is_file() {
        return Err("runtime lock is not a real regular file".to_owned());
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("could not open runtime lock: {error}"))?;
    match lock.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&lock)
                .map_err(|error| format!("could not release runtime status lock: {error}"))?;
            AosStatus::stopped()
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            Err("runtime lock is still held".to_owned())
        }
        Err(error) => Err(format!("could not inspect runtime lock state: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use astrid_core::kernel_api::DaemonStatus;

    use super::{AosStatus, stopped_status};
    use crate::AosHome;

    fn temporary_status_home(case: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "unicity-aos-{case}-status-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn maps_typed_runtime_status_to_product_status() {
        let status = AosStatus::from(DaemonStatus {
            pid: 42,
            uptime_secs: 90,
            version: "0.9.4".to_owned(),
            ephemeral: false,
            connected_clients: 3,
            connections_by_principal: Vec::new(),
            loaded_capsules: vec!["agents".to_owned(), "session".to_owned()],
        });

        assert_eq!(status.state, "running");
        assert_eq!(status.pid, 42);
        assert_eq!(status.runtime_version, "0.9.4");
        assert_eq!(status.loaded_capsules, ["agents", "session"]);
    }

    #[test]
    fn json_has_aos_owned_field_names() {
        let status = AosStatus {
            state: "running",
            pid: 7,
            uptime_secs: 8,
            runtime_version: "0.9.4".to_owned(),
            ephemeral: false,
            connected_clients: 1,
            loaded_capsules: vec!["agents".to_owned()],
        };

        let value = serde_json::to_value(status).expect("serialize status");
        assert_eq!(value["state"], "running");
        assert_eq!(value["runtime_version"], "0.9.4");
        assert!(value.get("astrid").is_none());
    }

    #[test]
    fn reports_a_typed_stopped_state_when_the_runtime_lock_is_available() {
        let root = temporary_status_home("stopped");
        let home = AosHome::from_root(&root);
        fs::create_dir_all(home.runtime_home().join("run")).expect("create runtime run dir");
        fs::write(home.runtime_home().join("run/system.lock"), []).expect("create runtime lock");

        let status = stopped_status(&home).expect("read stopped status");
        assert_eq!(status.state, "stopped");
        assert_eq!(status.pid, 0);
        assert_eq!(status.runtime_version, "0.10.0");

        fs::remove_dir_all(root).expect("remove stopped status fixture");
    }

    #[test]
    fn refuses_to_report_stopped_while_the_runtime_lock_is_held() {
        use fs2::FileExt as _;

        let root = temporary_status_home("running");
        let home = AosHome::from_root(&root);
        fs::create_dir_all(home.runtime_home().join("run")).expect("create runtime run dir");
        let lock_path = home.runtime_home().join("run/system.lock");
        fs::write(&lock_path, []).expect("create runtime lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("open runtime lock");
        lock.try_lock_exclusive().expect("hold runtime lock");

        let error = stopped_status(&home).expect_err("held lock must not report stopped");
        assert!(error.contains("runtime lock is still held"));

        fs2::FileExt::unlock(&lock).expect("release runtime lock");
        fs::remove_dir_all(root).expect("remove running status fixture");
    }
}
