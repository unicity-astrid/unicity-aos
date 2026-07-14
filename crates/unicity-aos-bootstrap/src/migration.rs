//! Explicit, staged import of a standalone Astrid Runtime home.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{AosHome, runtime_binary_name};

const MIGRATION_VERSION: u32 = 1;
const RECEIPT_SCHEMA_VERSION: u32 = 2;
const STAGING_DIR: &str = ".runtime-import";
const LOCK_FILE: &str = "astrid-home-v1.lock";
const PERSISTENT_TOP_LEVEL: &[&str] = &[
    "config.toml",
    "keys",
    "secrets",
    "var",
    "wit",
    "home",
    "lib",
    "trust",
    "capsules",
    "history",
];
const EPHEMERAL_TOP_LEVEL: &[&str] = &["run", "log", "cow"];
const ETC_ALLOWLIST: &[&str] = &[
    "config.toml",
    "servers.toml",
    "gateway.toml",
    "gateway-http.toml",
    "layout-version",
    "groups.toml",
    "invites.toml",
    "pair-tokens.toml",
    "gateway-revocations.json",
    "profiles",
    "hooks",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    Migrated,
    AlreadyMigrated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyDistro {
    pub principal: String,
    pub id: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Receipt {
    #[serde(default)]
    migration_version: u32,
    #[serde(default)]
    schema_version: u32,
    source: PathBuf,
    entries: Vec<Entry>,
    #[serde(default)]
    legacy_distros: Vec<LegacyDistro>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    bytes: u64,
    #[serde(default)]
    sha256: String,
}

struct MigrationLock {
    _file: File,
}

impl MigrationLock {
    fn acquire(home: &AosHome) -> io::Result<Self> {
        let path = home.root().join("migrations").join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            return invalid("runtime migration lock must be a regular file");
        }
        set_private_permissions(&path, false)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another runtime migration is already in progress",
                )
            } else {
                error
            }
        })?;
        Ok(Self { _file: file })
    }
}

pub(crate) fn migrate_runtime(home: &AosHome, source: &Path) -> io::Result<MigrationOutcome> {
    let source = checked_root(source, "legacy runtime home")?;
    let target = checked_target_path(&home.runtime_home())?;
    if source == target || source.starts_with(&target) || target.starts_with(&source) {
        return invalid("legacy runtime home and product runtime home must not overlap");
    }
    if source.join("run/system.sock").exists() {
        return invalid("stop the standalone runtime before migration");
    }

    create_private_dir(&home.root().join("migrations"))?;
    let _migration_lock = MigrationLock::acquire(home)?;
    let receipt_path = home.migration_receipt();
    recover_interrupted_transaction(home, &target, &source, &receipt_path)?;
    if receipt_path.is_file() {
        let receipt: Receipt = read_receipt(&receipt_path)?;
        if receipt.source == source && receipt_matches(&target, &receipt)? {
            remove_backup(&target_backup(&target))?;
            return Ok(MigrationOutcome::AlreadyMigrated);
        }
        return invalid(
            "an existing migration receipt does not match the requested source or target",
        );
    }

    validate_target(&target)?;
    validate_source_layout(&source)?;
    let staging = home.root().join(STAGING_DIR);
    if staging.exists() {
        return invalid(
            "a previous migration staging directory could not be recovered automatically",
        );
    }

    let result = (|| {
        create_private_dir(&staging)?;
        let mut entries = vec![copy_product_binary(&target, &staging)?];
        copy_etc_state(&source, &staging, &mut entries)?;
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
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let receipt = Receipt {
            migration_version: MIGRATION_VERSION,
            schema_version: RECEIPT_SCHEMA_VERSION,
            source: source.clone(),
            entries,
            legacy_distros: legacy_distros(&staging)?,
        };
        if !receipt_matches(&staging, &receipt)? {
            return invalid("staged runtime did not validate against its import manifest");
        }
        let staged_receipt = write_staged_receipt(&receipt_path, &receipt)?;
        let backup = replace_target(&target, &staging)?;
        if let Err(error) = finalize_receipt(&staged_receipt, &receipt_path) {
            remove_path(&receipt_path)?;
            remove_path(&staged_receipt)?;
            rollback_target(&target, &backup)?;
            return Err(error);
        }
        remove_backup(&backup)?;
        Ok(())
    })();
    if result.is_err() && staging.exists() {
        let _ = remove_path(&staging);
    }
    result.map(|()| MigrationOutcome::Migrated)
}

fn checked_target_path(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return invalid("bundled product runtime must be a real directory");
            }
            path.canonicalize()
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "bundled product runtime path must have a parent",
                )
            })?;
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "bundled product runtime path must have a final component",
                )
            })?;
            Ok(parent.canonicalize()?.join(name))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn imported_legacy_distros(home: &AosHome) -> io::Result<Vec<LegacyDistro>> {
    let receipt = read_receipt(&home.migration_receipt())?;
    Ok(receipt.legacy_distros)
}

fn legacy_distros(runtime_home: &Path) -> io::Result<Vec<LegacyDistro>> {
    let homes = runtime_home.join("home");
    if !homes.is_dir() {
        return Ok(Vec::new());
    }
    let mut distros = Vec::new();
    for entry in fs::read_dir(homes)? {
        let entry = entry?;
        let principal = entry.file_name().to_string_lossy().into_owned();
        let lock = entry.path().join(".config/distro.lock");
        let Ok(contents) = fs::read_to_string(lock) else {
            continue;
        };
        let Ok(value) = contents.parse::<toml::Value>() else {
            continue;
        };
        let Some(distro) = value.get("distro") else {
            continue;
        };
        let (Some(id), Some(version)) = (
            distro.get("id").and_then(toml::Value::as_str),
            distro.get("version").and_then(toml::Value::as_str),
        ) else {
            continue;
        };
        if matches!(id, "astralis" | "aos-ce") {
            distros.push(LegacyDistro {
                principal,
                id: id.to_owned(),
                version: version.to_owned(),
            });
        }
    }
    distros.sort_by(|left, right| left.principal.cmp(&right.principal));
    Ok(distros)
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
    let target_metadata = fs::symlink_metadata(target).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "bundled product runtime is not installed",
        )
    })?;
    if target_metadata.file_type().is_symlink() || !target_metadata.is_dir() {
        return invalid("bundled product runtime must be a real directory");
    }
    let allowed = target.join("bin").join(runtime_binary_name());
    let metadata = fs::symlink_metadata(&allowed).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "bundled product runtime executable is not installed",
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
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

fn validate_source_layout(source: &Path) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "legacy runtime contains a non-UTF-8 top-level path",
            )
        })?;
        let known = name == "etc"
            || name == "bin"
            || PERSISTENT_TOP_LEVEL.contains(&name.as_str())
            || EPHEMERAL_TOP_LEVEL.contains(&name.as_str());
        if !known {
            return invalid(&format!(
                "legacy runtime contains unsupported top-level state `{name}`; migration refuses to omit it"
            ));
        }
    }
    Ok(())
}

fn copy_etc_state(source_root: &Path, staging: &Path, entries: &mut Vec<Entry>) -> io::Result<()> {
    let source = source_root.join("etc");
    let metadata = match fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return invalid("legacy runtime etc must be a real directory");
    }

    for entry in fs::read_dir(&source)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "legacy runtime etc contains a non-UTF-8 path",
            )
        })?;
        if !ETC_ALLOWLIST.contains(&name.as_str()) {
            return invalid(&format!(
                "legacy runtime contains unsupported configuration `etc/{name}`; migration refuses to omit it"
            ));
        }
        let relative = PathBuf::from("etc").join(&name);
        copy_tree(&entry.path(), &staging.join(&relative), &relative, entries)?;
    }
    Ok(())
}

fn copy_product_binary(target: &Path, staging: &Path) -> io::Result<Entry> {
    let source = target.join("bin").join(runtime_binary_name());
    let relative = PathBuf::from("bin").join(runtime_binary_name());
    let destination = staging.join(&relative);
    copy_executable(&source, &destination, &relative)
}

fn copy_if_present(
    source: &Path,
    destination: &Path,
    relative: &Path,
    entries: &mut Vec<Entry>,
) -> io::Result<()> {
    match fs::symlink_metadata(source) {
        Ok(_) => copy_tree(source, destination, relative, entries)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn copy_wasm_blobs(source: &Path, destination: &Path, entries: &mut Vec<Entry>) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return invalid("legacy runtime bin must be a real directory");
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
            entries.push(copy_file(&path, &destination.join(&name), &relative)?);
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
        entries.push(copy_file(source, destination, relative)?);
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

fn copy_file(source: &Path, destination: &Path, relative: &Path) -> io::Result<Entry> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return invalid("migration only copies regular files");
    }
    if let Some(parent) = destination.parent() {
        create_private_dir(parent)?;
    }
    let mut input = File::open(source)?;
    let mut output = File::create(destination)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("migration byte count overflow"))?;
    }
    set_private_permissions(destination, false)?;
    output.sync_all()?;
    sync_parent(destination)?;
    Ok(Entry {
        path: relative.to_path_buf(),
        bytes,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn copy_executable(source: &Path, destination: &Path, relative: &Path) -> io::Result<Entry> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return invalid("bundled runtime executable must be a regular file");
    }
    if let Some(parent) = destination.parent() {
        create_private_dir(parent)?;
    }
    let bytes = fs::copy(source, destination)?;
    fs::set_permissions(destination, metadata.permissions())?;
    File::open(destination)?.sync_all()?;
    sync_parent(destination)?;
    Ok(Entry {
        path: relative.to_path_buf(),
        bytes,
        sha256: sha256_file(destination)?,
    })
}

fn replace_target(target: &Path, staging: &Path) -> io::Result<PathBuf> {
    let backup = target_backup(target);
    if backup.exists() {
        return invalid("previous product runtime backup exists; inspect it before migration");
    }
    if target.exists() {
        fs::rename(target, &backup)?;
        sync_parent(target)?;
    }
    if let Err(error) = fs::rename(staging, target) {
        let _ = fs::rename(&backup, target);
        let _ = sync_parent(target);
        return Err(error);
    }
    sync_parent(target)?;
    Ok(backup)
}

fn target_backup(target: &Path) -> PathBuf {
    target.with_extension("pre-migration")
}

fn rollback_target(target: &Path, backup: &Path) -> io::Result<()> {
    let failed_target = target.with_extension("failed-migration");
    if failed_target.exists() {
        return invalid("failed migration target already exists; manual recovery is required");
    }
    fs::rename(target, &failed_target)?;
    sync_parent(target)?;
    if let Err(error) = fs::rename(backup, target) {
        let _ = fs::rename(&failed_target, target);
        let _ = sync_parent(target);
        return Err(error);
    }
    sync_parent(target)?;
    fs::remove_dir_all(failed_target)?;
    sync_parent(target)
}

fn remove_backup(backup: &Path) -> io::Result<()> {
    if backup.exists() {
        fs::remove_dir_all(backup)?;
        sync_parent(backup)?;
    }
    Ok(())
}

fn recover_interrupted_transaction(
    home: &AosHome,
    target: &Path,
    source: &Path,
    receipt_path: &Path,
) -> io::Result<()> {
    let backup = target_backup(target);
    let staging = home.root().join(STAGING_DIR);
    let staged_receipt = staged_receipt_path(receipt_path);

    if receipt_path.is_file() {
        return Ok(());
    }

    if !backup.exists() {
        if staging.exists() || staged_receipt.exists() {
            remove_path(&staging)?;
            remove_path(&staged_receipt)?;
        }
        return Ok(());
    }

    if !target.exists() {
        fs::rename(&backup, target)?;
        sync_parent(target)?;
        remove_path(&staging)?;
        remove_path(&staged_receipt)?;
        return Ok(());
    }

    let can_complete = read_receipt(&staged_receipt)
        .ok()
        .filter(|receipt| receipt.source == source)
        .is_some_and(|receipt| receipt_matches(target, &receipt).unwrap_or(false));
    if can_complete {
        finalize_receipt(&staged_receipt, receipt_path)?;
        remove_backup(&backup)?;
        return Ok(());
    }

    rollback_target(target, &backup)?;
    remove_path(&staging)?;
    remove_path(&staged_receipt)
}

fn remove_path(path: &Path) -> io::Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    sync_parent(path)
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
    validate_receipt(receipt)?;
    for entry in &receipt.entries {
        let path = root.join(&entry.path);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != entry.bytes
            || sha256_file(&path)? != entry.sha256
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn write_staged_receipt(path: &Path, receipt: &Receipt) -> io::Result<PathBuf> {
    validate_receipt(receipt)?;
    let temporary = staged_receipt_path(path);
    let bytes = serde_json::to_vec_pretty(receipt).map_err(io::Error::other)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    set_private_permissions(&temporary, false)?;
    file.sync_all()?;
    sync_parent(&temporary)?;
    Ok(temporary)
}

fn staged_receipt_path(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}

fn finalize_receipt(temporary: &Path, path: &Path) -> io::Result<()> {
    fs::rename(temporary, path)?;
    sync_parent(path)
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    set_private_permissions(path, true)?;
    sync_directory(path)?;
    sync_parent(path)
}

fn read_receipt(path: &Path) -> io::Result<Receipt> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return invalid("migration receipt must be a regular file");
    }
    let receipt = serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?;
    validate_receipt(&receipt)?;
    Ok(receipt)
}

fn validate_receipt(receipt: &Receipt) -> io::Result<()> {
    if receipt.migration_version != MIGRATION_VERSION {
        return invalid("unsupported runtime migration version in receipt");
    }
    if receipt.schema_version != RECEIPT_SCHEMA_VERSION {
        return invalid("unsupported runtime migration receipt schema");
    }
    let mut paths = HashSet::new();
    for entry in &receipt.entries {
        if !is_safe_relative(&entry.path) {
            return invalid("migration receipt contains an unsafe path");
        }
        if !paths.insert(entry.path.clone()) {
            return invalid("migration receipt contains a duplicate path");
        }
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return invalid("migration receipt contains an invalid SHA-256 digest");
        }
    }
    Ok(())
}

fn is_safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
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

    use super::{
        MIGRATION_VERSION, MigrationLock, MigrationOutcome, RECEIPT_SCHEMA_VERSION,
        create_private_dir, migrate_runtime, recover_interrupted_transaction,
    };
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
        write(&source, "lib/shared.wasm", b"shared-component");
        write(&source, "trust/unicity-ce.pub", b"distro-key");
        write(&source, "capsules/system/meta.toml", b"system-capsule");
        write(&source, "history", b"aos status\n");
        write(&source, "config.toml", b"[security]\nstrict = true\n");
        write(&source, "etc/config.toml", b"[runtime]\n");
        write(&source, "etc/servers.toml", b"[servers]\n");
        write(&source, "etc/gateway.toml", b"[gateway]\n");
        write(&source, "etc/gateway-http.toml", b"enabled = false\n");
        write(&source, "etc/layout-version", b"1");
        write(&source, "etc/groups.toml", b"[groups]\n");
        write(&source, "etc/invites.toml", b"[invites]\n");
        write(&source, "etc/pair-tokens.toml", b"[tokens]\n");
        write(&source, "etc/gateway-revocations.json", b"[]");
        write(&source, "etc/profiles/alice.toml", b"enabled = true\n");
        write(&source, "etc/hooks/audit.toml", b"enabled = true\n");
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
            fs::read(runtime.join("etc/profiles/alice.toml")).unwrap(),
            b"enabled = true\n"
        );
        assert_eq!(
            fs::read(runtime.join("etc/groups.toml")).unwrap(),
            b"[groups]\n"
        );
        assert_eq!(
            fs::read(runtime.join("trust/unicity-ce.pub")).unwrap(),
            b"distro-key"
        );
        assert_eq!(fs::read(runtime.join("history")).unwrap(), b"aos status\n");
        assert_eq!(
            fs::read(runtime.join("config.toml")).unwrap(),
            b"[security]\nstrict = true\n"
        );
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
        let receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(product.migration_receipt()).expect("read migration receipt"),
        )
        .expect("decode migration receipt");
        assert_eq!(receipt["migration_version"], MIGRATION_VERSION);
        assert_eq!(receipt["schema_version"], RECEIPT_SCHEMA_VERSION);
        assert!(receipt["entries"].as_array().is_some_and(|entries| {
            entries.iter().all(|entry| {
                entry["sha256"]
                    .as_str()
                    .is_some_and(|digest| digest.len() == 64)
            })
        }));
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

    #[test]
    fn records_legacy_distro_locks_without_rewriting_them() {
        let root = fixture_root("legacy-distros");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");
        let astralis = b"[distro]\nid = \"astralis\"\nversion = \"0.2.2\"\n";
        let aos_ce = b"[distro]\nid = \"aos-ce\"\nversion = \"2026.1.0\"\n";
        write(&source, "home/alice/.config/distro.lock", astralis);
        write(&source, "home/bob/.config/distro.lock", aos_ce);
        write(
            &source,
            "home/carol/.config/distro.lock",
            b"[distro]\nid = \"unicity-ce\"\nversion = \"2026.1.0\"\n",
        );
        write(&source, "home/dan/.config/distro.lock", b"not toml");

        migrate_runtime(&product, &source).expect("migration succeeds");
        assert_eq!(
            product.imported_legacy_distros().expect("read receipt"),
            vec![
                super::LegacyDistro {
                    principal: "alice".into(),
                    id: "astralis".into(),
                    version: "0.2.2".into()
                },
                super::LegacyDistro {
                    principal: "bob".into(),
                    id: "aos-ce".into(),
                    version: "2026.1.0".into()
                },
            ]
        );
        assert_eq!(
            fs::read(
                product
                    .runtime_home()
                    .join("home/alice/.config/distro.lock")
            )
            .unwrap(),
            astralis
        );
        assert_eq!(
            fs::read(product.runtime_home().join("home/bob/.config/distro.lock")).unwrap(),
            aos_ce
        );
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn rejects_an_unversioned_unhashed_receipt() {
        let root = fixture_root("legacy-receipt");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "keys/runtime.key", b"runtime-key");

        let source = source.canonicalize().expect("canonical source");
        let receipt = serde_json::json!({
            "source": source,
            "entries": [{ "path": "keys/runtime.key", "bytes": 11 }],
        });
        let receipt_path = product.migration_receipt();
        fs::create_dir_all(receipt_path.parent().expect("receipt parent"))
            .expect("create receipt parent");
        fs::write(
            &receipt_path,
            serde_json::to_vec(&receipt).expect("serialize legacy receipt"),
        )
        .expect("write legacy receipt");

        let error = migrate_runtime(&product, &source)
            .expect_err("unversioned receipt must not bypass integrity validation");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn rejects_unknown_configuration_instead_of_silently_dropping_it() {
        let root = fixture_root("unknown-runtime-config");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&source, "etc/future-policy.toml", b"deny = true\n");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");

        let error = migrate_runtime(&product, &source)
            .expect_err("unknown authorization state must stop migration");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("etc/future-policy.toml"));
        assert!(!product.runtime_home().join("keys/runtime.key").exists());
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn rejects_unknown_top_level_state_instead_of_silently_dropping_it() {
        let root = fixture_root("unknown-runtime-state");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&source, "future-store/state", b"important");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");

        let error = migrate_runtime(&product, &source)
            .expect_err("unknown persistent state must stop migration");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("future-store"));
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn detects_same_length_content_tampering() {
        let root = fixture_root("runtime-content-tamper");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");
        migrate_runtime(&product, &source).expect("migration succeeds");

        write(&product.runtime_home(), "keys/runtime.key", b"tampered-ke");
        let error = migrate_runtime(&product, &source)
            .expect_err("same-length tampering must invalidate the receipt");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn detects_bundled_runtime_executable_tampering() {
        let root = fixture_root("runtime-binary-tamper");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");
        migrate_runtime(&product, &source).expect("migration succeeds");

        write(&product.runtime_home(), "bin/astrid", b"tamperd-binary");
        let error = migrate_runtime(&product, &source)
            .expect_err("runtime executable tampering must invalidate the receipt");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn rejects_a_lexical_alias_of_the_product_runtime_as_source() {
        let root = fixture_root("runtime-source-alias");
        fs::create_dir_all(root.join("alias")).expect("create alias path component");
        let product = AosHome::from_root(root.join("alias/../product"));
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");
        let source = root.join("product/runtime");

        let error = migrate_runtime(&product, &source)
            .expect_err("the product runtime cannot be imported through a path alias");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            fs::read(source.join("bin/astrid")).unwrap(),
            b"bundled-binary"
        );
        assert!(!product.migration_receipt().exists());
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn rejects_overlapping_roots_without_modifying_the_source() {
        let root = fixture_root("runtime-overlapping-roots");
        let source = root.join("legacy");
        let product = AosHome::from_root(source.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");

        let error = migrate_runtime(&product, &source)
            .expect_err("product state must not be created beneath the source");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!product.root().join("migrations").exists());
        assert!(!product.root().join(".runtime-import").exists());
        assert_eq!(
            fs::read(source.join("keys/runtime.key")).unwrap(),
            b"runtime-key"
        );
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn serializes_concurrent_runtime_migrations() {
        let root = fixture_root("runtime-concurrent");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");
        create_private_dir(&product.root().join("migrations")).expect("create migrations dir");
        let held = MigrationLock::acquire(&product).expect("hold migration lock");

        let error = migrate_runtime(&product, &source)
            .expect_err("a concurrent migration must fail before staging");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(!product.root().join(".runtime-import").exists());
        assert!(!product.migration_receipt().exists());
        assert_eq!(
            fs::read(product.runtime_home().join("bin/astrid")).unwrap(),
            b"bundled-binary"
        );
        drop(held);
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn completes_an_interrupted_validated_cutover() {
        let root = fixture_root("runtime-cutover-complete");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");
        migrate_runtime(&product, &source).expect("migration succeeds");

        let receipt = product.migration_receipt();
        let staged_receipt = receipt.with_extension("tmp");
        fs::rename(&receipt, &staged_receipt).expect("stage committed receipt");
        write(
            product.root(),
            "runtime.pre-migration/bin/astrid",
            b"bundled-binary",
        );
        let canonical_source = source.canonicalize().expect("canonical source");

        assert_eq!(
            migrate_runtime(&product, &canonical_source).expect("recover valid cutover"),
            MigrationOutcome::AlreadyMigrated
        );
        assert!(receipt.is_file());
        assert!(!staged_receipt.exists());
        assert!(!product.root().join("runtime.pre-migration").exists());
        assert_eq!(
            fs::read(product.runtime_home().join("keys/runtime.key")).unwrap(),
            b"runtime-key"
        );
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[test]
    fn rolls_back_an_interrupted_unvalidated_cutover() {
        let root = fixture_root("runtime-cutover-rollback");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&product.runtime_home(), "bin/astrid", b"bundled-binary");
        let target = product.runtime_home();
        let backup = product.root().join("runtime.pre-migration");
        fs::rename(&target, &backup).expect("move original to transaction backup");
        write(&target, "bin/astrid", b"partial-binary");
        write(
            product.root(),
            "migrations/astrid-home-v1.tmp",
            b"invalid receipt",
        );
        let canonical_source = source.canonicalize().expect("canonical source");

        recover_interrupted_transaction(
            &product,
            &target,
            &canonical_source,
            &product.migration_receipt(),
        )
        .expect("roll back invalid cutover");

        assert_eq!(
            fs::read(target.join("bin/astrid")).unwrap(),
            b"bundled-binary"
        );
        assert!(!backup.exists());
        assert!(!product.migration_receipt().with_extension("tmp").exists());
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

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_bundled_runtime_executable() {
        use std::os::unix::fs::symlink;

        let root = fixture_root("runtime-binary-symlink");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        write(&source, "keys/runtime.key", b"runtime-key");
        write(
            &product.runtime_home(),
            "bin/runtime-target",
            b"bundled-binary",
        );
        symlink(
            product.runtime_home().join("bin/runtime-target"),
            product.runtime_home().join("bin/astrid"),
        )
        .expect("create bundled binary symlink");

        let error =
            migrate_runtime(&product, &source).expect_err("binary symlink must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!product.runtime_home().join("keys/runtime.key").exists());
        fs::remove_dir_all(root).expect("remove fixture root");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_product_runtime_home() {
        use std::os::unix::fs::symlink;

        let root = fixture_root("runtime-home-symlink");
        let source = root.join("legacy");
        let product = AosHome::from_root(root.join("product"));
        let external_runtime = root.join("external-runtime");
        write(&source, "keys/runtime.key", b"runtime-key");
        write(&external_runtime, "bin/astrid", b"bundled-binary");
        fs::create_dir_all(product.root()).expect("create product root");
        symlink(&external_runtime, product.runtime_home()).expect("symlink product runtime home");

        let error = migrate_runtime(&product, &source).expect_err("runtime home symlink must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!external_runtime.join("keys/runtime.key").exists());
        fs::remove_dir_all(root).expect("remove fixture root");
    }
}
