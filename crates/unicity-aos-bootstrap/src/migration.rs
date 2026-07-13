//! Explicit, staged import of a standalone Astrid Runtime home.

use std::fs::{self, File};
use std::io::{self};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{AosHome, runtime_binary_name};

const PERSISTENT_TOP_LEVEL: &[&str] = &["keys", "secrets", "var", "wit", "home"];
const EPHEMERAL_TOP_LEVEL: &[&str] = &["run", "log", "cow"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    Migrated,
    AlreadyMigrated,
}

#[derive(Debug, Serialize, Deserialize)]
struct Receipt {
    source: PathBuf,
    entries: Vec<Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    bytes: u64,
}

pub(crate) fn migrate_runtime(home: &AosHome, source: &Path) -> io::Result<MigrationOutcome> {
    let source = checked_root(source, "legacy runtime home")?;
    let target = home.runtime_home();
    if source == target {
        return invalid("legacy runtime home and product runtime home must differ");
    }
    if source.join("run/system.sock").exists() {
        return invalid("stop the standalone runtime before migration");
    }

    let receipt_path = home.migration_receipt();
    if receipt_path.is_file() {
        let receipt: Receipt = read_receipt(&receipt_path)?;
        if receipt.source == source && receipt_matches(&target, &receipt)? {
            return Ok(MigrationOutcome::AlreadyMigrated);
        }
        return invalid(
            "an existing migration receipt does not match the requested source or target",
        );
    }

    validate_target(&target)?;
    create_private_dir(&home.root().join("migrations"))?;
    let staging = home.root().join(format!(
        ".runtime-import-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    if staging.exists() {
        return invalid(
            "a previous migration staging directory exists; inspect and remove it before retrying",
        );
    }

    let result = (|| {
        create_private_dir(&staging)?;
        copy_product_binary(&target, &staging)?;
        let mut entries = Vec::new();
        for name in PERSISTENT_TOP_LEVEL {
            copy_if_present(
                &source.join(name),
                &staging.join(name),
                Path::new(name),
                &mut entries,
            )?;
        }
        copy_wasm_blobs(&source.join("bin"), &staging.join("bin"), &mut entries)?;
        ensure_no_ephemeral_data(&staging)?;
        let receipt = Receipt {
            source: source.clone(),
            entries,
        };
        if !receipt_matches(&staging, &receipt)? {
            return invalid("staged runtime did not validate against its import manifest");
        }
        replace_target(&target, &staging)?;
        write_receipt(&receipt_path, &receipt)?;
        Ok(())
    })();
    if result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result.map(|()| MigrationOutcome::Migrated)
}

fn checked_root(path: &Path, description: &str) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return invalid(&format!("{description} must be an absolute path"));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return invalid(&format!(
            "{description} must be a real directory, not a symlink"
        ));
    }
    path.canonicalize()
}

fn validate_target(target: &Path) -> io::Result<()> {
    if !target.is_dir() {
        return invalid("bundled product runtime is not installed");
    }
    let allowed = target.join("bin").join(runtime_binary_name());
    if !allowed.is_file() {
        return invalid("bundled product runtime executable is not installed");
    }
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        let path = entry.path();
        if path == target.join("bin") {
            for nested in fs::read_dir(&path)? {
                let nested = nested?;
                if nested.path() != allowed {
                    return invalid(
                        "product runtime home contains data; migration refuses to merge state",
                    );
                }
            }
        } else {
            return invalid("product runtime home contains data; migration refuses to merge state");
        }
    }
    Ok(())
}

fn copy_product_binary(target: &Path, staging: &Path) -> io::Result<()> {
    let source = target.join("bin").join(runtime_binary_name());
    let destination = staging.join("bin").join(runtime_binary_name());
    copy_executable(&source, &destination)
}

fn copy_if_present(
    source: &Path,
    destination: &Path,
    relative: &Path,
    entries: &mut Vec<Entry>,
) -> io::Result<()> {
    if source.exists() {
        copy_tree(source, destination, relative, entries)?;
    }
    Ok(())
}

fn copy_wasm_blobs(source: &Path, destination: &Path, entries: &mut Vec<Entry>) -> io::Result<()> {
    if !source.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let path = entry.path();
        if path.is_file()
            && Path::new(&name)
                .extension()
                .is_some_and(|ext| ext == "wasm")
        {
            let relative = PathBuf::from("bin").join(&name);
            copy_file(&path, &destination.join(&name))?;
            entries.push(Entry {
                path: relative,
                bytes: fs::metadata(path)?.len(),
            });
        }
    }
    Ok(())
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    relative: &Path,
    entries: &mut Vec<Entry>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return invalid("legacy runtime contains a symlink; migration refuses to follow links");
    }
    if metadata.is_file() {
        copy_file(source, destination)?;
        entries.push(Entry {
            path: relative.to_path_buf(),
            bytes: metadata.len(),
        });
        return Ok(());
    }
    if !metadata.is_dir() {
        return invalid("legacy runtime contains a non-regular file");
    }
    create_private_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if Path::new(&name)
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return invalid("unsafe legacy runtime path");
        }
        copy_tree(
            &entry.path(),
            &destination.join(&name),
            &relative.join(name),
            entries,
        )?;
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return invalid("migration only copies regular files");
    }
    if let Some(parent) = destination.parent() {
        create_private_dir(parent)?;
    }
    let mut input = File::open(source)?;
    let mut output = File::create(destination)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    set_private_permissions(destination, false)
}

fn copy_executable(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return invalid("bundled runtime executable must be a regular file");
    }
    if let Some(parent) = destination.parent() {
        create_private_dir(parent)?;
    }
    fs::copy(source, destination)?;
    fs::set_permissions(destination, metadata.permissions())
}

fn replace_target(target: &Path, staging: &Path) -> io::Result<()> {
    let backup = target.with_extension("pre-migration");
    if backup.exists() {
        return invalid("previous product runtime backup exists; inspect it before migration");
    }
    if target.exists() {
        fs::rename(target, &backup)?;
    }
    if let Err(error) = fs::rename(staging, target) {
        if backup.exists() {
            let _ = fs::rename(&backup, target);
        }
        return Err(error);
    }
    if backup.exists() {
        fs::remove_dir_all(backup)?;
    }
    Ok(())
}

fn ensure_no_ephemeral_data(staging: &Path) -> io::Result<()> {
    if EPHEMERAL_TOP_LEVEL
        .iter()
        .any(|name| staging.join(name).exists())
    {
        return invalid("ephemeral runtime data entered migration staging");
    }
    Ok(())
}

fn receipt_matches(root: &Path, receipt: &Receipt) -> io::Result<bool> {
    receipt.entries.iter().try_fold(true, |valid, entry| {
        let path = root.join(&entry.path);
        Ok(valid && path.is_file() && fs::metadata(path)?.len() == entry.bytes)
    })
}

fn write_receipt(path: &Path, receipt: &Receipt) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(receipt).map_err(io::Error::other)?;
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    set_private_permissions(path, true)
}

fn read_receipt(path: &Path) -> io::Result<Receipt> {
    serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> io::Result<()> {
    Ok(())
}

fn invalid<T>(message: &str) -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        message.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{MigrationOutcome, migrate_runtime};
    use crate::AosHome;

    fn fixture_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "unicity-aos-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create fixture root");
        root
    }

    fn write(root: &Path, relative: &str, content: &[u8]) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
        fs::write(path, content).expect("write fixture file");
    }

    #[test]
    fn imports_persistent_state_without_live_or_legacy_binaries() {
        let root = fixture_root("runtime-migration");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&source, "secrets/provider", b"provider-secret");
        write(&source, "var/state.db", b"state");
        write(&source, "wit/store/contracts.wit", b"wit");
        write(&source, "home/alice/.local/audit/chain", b"audit");
        write(&source, "bin/capsule.wasm", b"wasm");
        write(&source, "bin/astrid", b"legacy-binary");
        write(&source, "run/ready", b"live-state");
        write(&source, "log/daemon.log", b"log");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");

        assert_eq!(
            migrate_runtime(&product, &source).expect("migration succeeds"),
            MigrationOutcome::Migrated
        );
        let runtime = product.runtime_home();
        assert_eq!(
            fs::read(runtime.join("keys/runtime.key")).unwrap(),
            b"runtime-key"
        );
        assert_eq!(
            fs::read(runtime.join("secrets/provider")).unwrap(),
            b"provider-secret"
        );
        assert_eq!(fs::read(runtime.join("bin/capsule.wasm")).unwrap(), b"wasm");
        assert_eq!(
            fs::read(runtime.join("bin/astrid")).unwrap(),
            b"bundled-binary"
        );
        assert!(!runtime.join("run").exists());
        assert!(!runtime.join("log").exists());
        assert_eq!(
            fs::read(source.join("keys/runtime.key")).unwrap(),
            b"runtime-key"
        );
        assert!(
            product
                .root()
                .join("migrations/astrid-home-v1.json")
                .is_file()
        );
        assert_eq!(
            migrate_runtime(&product, &source).expect("matching migration is idempotent"),
            MigrationOutcome::AlreadyMigrated
        );
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn refuses_to_merge_into_existing_runtime_state() {
        let root = fixture_root("runtime-existing-target");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");
        write(&product.runtime_home(), "var/state.db", b"existing-state");

        let error =
            migrate_runtime(&product, &source).expect_err("existing runtime state must not merge");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            fs::read(product.runtime_home().join("var/state.db")).unwrap(),
            b"existing-state"
        );
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_legacy_content() {
        use std::os::unix::fs::symlink;

        let root = fixture_root("runtime-symlink");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "outside-key", b"runtime-key");
        fs::create_dir_all(source.join("keys")).expect("create keys directory");
        symlink(source.join("outside-key"), source.join("keys/runtime.key"))
            .expect("create legacy symlink");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");

        let error = migrate_runtime(&product, &source).expect_err("symlink must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!product.runtime_home().join("keys/runtime.key").exists());
        assert_eq!(
            fs::read(product.runtime_home().join("bin/astrid")).unwrap(),
            b"bundled-binary"
        );
        fs::remove_dir_all(root).expect("remove fixture root");
    }
}
