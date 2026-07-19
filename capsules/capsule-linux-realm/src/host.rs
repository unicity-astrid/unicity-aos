//! Astrid-backed mounts for the private AOS Realm guest filesystem.

use aos_realm_9p::{
    DirectoryEntry as Plan9DirectoryEntry, Errno as Plan9Errno, FileSystem as Plan9FileSystem,
    FileSystemStats as Plan9FileSystemStats, Metadata as Plan9Metadata, NodeKind as Plan9NodeKind,
};
use aos_realm_runtime::{OpenMode, RealmFile, RealmHost, RealmIoError};
use aos_realm_vfs::{
    BlobDigest, FsError, FsStatus, MAX_MANIFEST_BYTES, RealmFs, RealmStore, StoreError,
};
use astrid_sdk::{SysError, fs, kv};
use std::time::UNIX_EPOCH;
#[cfg(test)]
use std::{
    fs as native_fs,
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) const REALM_NAME: &str = "default";
pub(crate) const DEFAULT_CWD: &str = "/workspace";
pub(crate) const REALM_HOME: &str = "home://.local/share/aos-realm/default/home/agent";
pub(crate) const REALM_STATE: &str = "home://.local/share/aos-realm/default/state";
// Astrid's principal tmp root lives at `home://.local/tmp`. Address it through
// the dynamic home scheme so the manifest gate can resolve the invoking
// principal before checking the path. The guest still sees only `/tmp`.
pub(crate) const REALM_TMP: &str = "home://.local/tmp/aos-realm/default";
const FORMAT_MARKER: &str = "home://.local/share/aos-realm/default/state/format";
const BLOB_ROOT: &str = "home://.local/share/aos-realm/default/store/blobs";
const HEAD_KEY: &str = "realm/default/fs/head";
const LEGACY_FORMAT_MARKER: &[u8] = b"aos-realm-format=0\n";
const CURRENT_FORMAT_MARKER: &[u8] = b"aos-realm-format=1\n";
const MAX_SEED_FILE_BYTES: usize = 64 * 1024;

pub(crate) const LINUX_WORKSPACE_9P_CHANNEL: u32 = 2;

/// Astrid VFS projection used by the Linux 9P workspace mount.
///
/// The root is deliberately fixed rather than guest-selected. The Astrid
/// kernel resolves `cwd://` against the current invocation's admitted COW
/// workspace on every operation.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct AstridWorkspace9p;

impl Plan9FileSystem for AstridWorkspace9p {
    fn metadata(&mut self, path: &str) -> Result<Plan9Metadata, Plan9Errno> {
        plan9_metadata(&workspace_path(path)?)
    }

    fn read_dir(&mut self, path: &str) -> Result<Vec<Plan9DirectoryEntry>, Plan9Errno> {
        let path = workspace_path(path)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(&path).map_err(map_sdk_9p_error)? {
            entries.push(Plan9DirectoryEntry {
                name: entry.file_name().to_string(),
                metadata: plan9_metadata(entry.path())?,
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn read(&mut self, path: &str, offset: u64, count: u32) -> Result<Vec<u8>, Plan9Errno> {
        let file = fs::File::open(&workspace_path(path)?).map_err(map_sdk_9p_error)?;
        file.read_at(offset, count).map_err(map_sdk_9p_error)
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u32, Plan9Errno> {
        let file = fs::File::open_mode(&workspace_path(path)?, fs::OpenMode::ReadWrite)
            .map_err(map_sdk_9p_error)?;
        file.write_at(offset, data).map_err(map_sdk_9p_error)
    }

    fn create_file(
        &mut self,
        path: &str,
        _mode: u32,
        exclusive: bool,
        truncate: bool,
    ) -> Result<(), Plan9Errno> {
        let path = workspace_path(path)?;
        let exists = fs::exists(&path).map_err(map_sdk_9p_error)?;
        if exists && exclusive {
            return Err(Plan9Errno::AlreadyExists);
        }
        if exists {
            let metadata = plan9_metadata(&path)?;
            if metadata.kind != Plan9NodeKind::File {
                return Err(Plan9Errno::IsDirectory);
            }
            if truncate {
                drop(fs::File::create(&path).map_err(map_sdk_9p_error)?);
            }
        } else {
            drop(fs::File::create(&path).map_err(map_sdk_9p_error)?);
        }
        Ok(())
    }

    fn create_dir(&mut self, path: &str, _mode: u32) -> Result<(), Plan9Errno> {
        fs::create_dir(&workspace_path(path)?).map_err(map_sdk_9p_error)
    }

    fn set_len(&mut self, path: &str, len: u64) -> Result<(), Plan9Errno> {
        let file = fs::File::open_mode(&workspace_path(path)?, fs::OpenMode::ReadWrite)
            .map_err(map_sdk_9p_error)?;
        file.set_len(len).map_err(map_sdk_9p_error)
    }

    fn remove_file(&mut self, path: &str) -> Result<(), Plan9Errno> {
        fs::remove_file(&workspace_path(path)?).map_err(map_sdk_9p_error)
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), Plan9Errno> {
        let path = workspace_path(path)?;
        if fs::read_dir(&path)
            .map_err(map_sdk_9p_error)?
            .next()
            .is_some()
        {
            return Err(Plan9Errno::NotEmpty);
        }
        fs::remove_dir_all(&path)
            .map(|_| ())
            .map_err(map_sdk_9p_error)
    }

    fn rename(&mut self, source: &str, destination: &str) -> Result<(), Plan9Errno> {
        fs::rename(&workspace_path(source)?, &workspace_path(destination)?)
            .map_err(map_sdk_9p_error)
    }

    fn sync(&mut self, path: &str, data_only: bool) -> Result<(), Plan9Errno> {
        let path = workspace_path(path)?;
        if plan9_metadata(&path)?.kind == Plan9NodeKind::Directory {
            return Err(Plan9Errno::NotSupported);
        }
        let file = fs::File::open_mode(&path, fs::OpenMode::ReadWrite).map_err(map_sdk_9p_error)?;
        if data_only {
            file.sync_data()
        } else {
            file.sync_all()
        }
        .map_err(map_sdk_9p_error)
    }

    fn statfs(&mut self) -> Result<Plan9FileSystemStats, Plan9Errno> {
        // Astrid deliberately does not expose the physical backing filesystem's
        // capacity. Zero means unknown, not exhausted; operation quotas remain
        // enforced by the kernel on every call.
        Ok(Plan9FileSystemStats::default())
    }
}

fn workspace_path(relative: &str) -> Result<String, Plan9Errno> {
    if relative.starts_with('/')
        || relative.split('/').any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.chars().any(char::is_control)
        }) && !relative.is_empty()
    {
        return Err(Plan9Errno::InvalidArgument);
    }
    Ok(if relative.is_empty() {
        "cwd://".to_string()
    } else {
        format!("cwd://{relative}")
    })
}

#[cfg_attr(test, allow(dead_code))]
fn plan9_metadata(path: &str) -> Result<Plan9Metadata, Plan9Errno> {
    let metadata = fs::symlink_metadata(path).map_err(map_sdk_9p_error)?;
    let kind = if metadata.is_file() {
        Plan9NodeKind::File
    } else if metadata.is_dir() {
        Plan9NodeKind::Directory
    } else {
        // Symlinks and special nodes are deliberately not traversable through
        // the first workspace export.
        return Err(Plan9Errno::NotSupported);
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
    Ok(Plan9Metadata {
        kind,
        len: metadata.len(),
        mode: metadata.mode(),
        modified_seconds: modified.as_ref().map_or(0, std::time::Duration::as_secs),
        generation: modified
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .unwrap_or(0),
    })
}

#[cfg_attr(test, allow(dead_code))]
fn map_sdk_9p_error(error: SysError) -> Plan9Errno {
    if let SysError::HostError(code) = &error {
        return match code.as_str() {
            "NotFound" => Plan9Errno::NotFound,
            "Access" | "CapabilityDenied" | "BoundaryEscape" => Plan9Errno::Permission,
            "InvalidPath" | "CrossVfs" => Plan9Errno::InvalidArgument,
            "IsDirectory" => Plan9Errno::IsDirectory,
            "NotDirectory" => Plan9Errno::NotDirectory,
            "NotEmpty" => Plan9Errno::NotEmpty,
            "TooLarge" | "Quota" => Plan9Errno::NoSpace,
            "AlreadyExists" => Plan9Errno::AlreadyExists,
            "Closed" => Plan9Errno::BadFileDescriptor,
            // `WouldBlock` cannot occur on the synchronous workspace profile;
            // preserve an honest I/O failure if a backend violates that contract.
            _ => Plan9Errno::Io,
        };
    }
    match map_sdk_error(error) {
        RealmIoError::NotFound => Plan9Errno::NotFound,
        RealmIoError::InvalidPath => Plan9Errno::InvalidArgument,
        RealmIoError::Denied => Plan9Errno::Permission,
        RealmIoError::IsDirectory => Plan9Errno::IsDirectory,
        RealmIoError::NotDirectory => Plan9Errno::NotDirectory,
        RealmIoError::TooLarge => Plan9Errno::NoSpace,
        RealmIoError::Unsupported => Plan9Errno::NotSupported,
        RealmIoError::Io => Plan9Errno::Io,
    }
}

/// Native-only workspace used to exercise the real Linux 9P client in tests.
///
/// Production builds use `AstridWorkspace9p`; this backend exists solely so a
/// host test can prove Linux mount/read/write behavior without fabricating an
/// Astrid invocation context.
#[cfg(test)]
pub(crate) struct NativeTestWorkspace9p {
    root: PathBuf,
}

#[cfg(test)]
impl NativeTestWorkspace9p {
    pub(crate) fn new() -> Result<Self, SysError> {
        static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "aos-linux-realm-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        native_fs::create_dir(&root).map_err(|error| {
            SysError::ApiError(format!("failed to create native test workspace: {error}"))
        })?;
        Ok(Self { root })
    }

    fn path(&self, relative: &str) -> Result<PathBuf, Plan9Errno> {
        let mut path = self.root.clone();
        if relative.is_empty() {
            return Ok(path);
        }
        for component in Path::new(relative).components() {
            let Component::Normal(component) = component else {
                return Err(Plan9Errno::InvalidArgument);
            };
            path.push(component);
        }
        Ok(path)
    }

    fn metadata_at(&self, path: &Path) -> Result<Plan9Metadata, Plan9Errno> {
        let metadata = native_fs::symlink_metadata(path).map_err(map_native_9p_error)?;
        let kind = if metadata.is_file() {
            Plan9NodeKind::File
        } else if metadata.is_dir() {
            Plan9NodeKind::Directory
        } else {
            return Err(Plan9Errno::NotSupported);
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
        Ok(Plan9Metadata {
            kind,
            len: metadata.len(),
            mode: metadata.mode(),
            modified_seconds: modified.as_ref().map_or(0, std::time::Duration::as_secs),
            generation: modified
                .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
                .unwrap_or(0),
        })
    }
}

#[cfg(test)]
impl Drop for NativeTestWorkspace9p {
    fn drop(&mut self) {
        let _ = native_fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
impl Plan9FileSystem for NativeTestWorkspace9p {
    fn metadata(&mut self, path: &str) -> Result<Plan9Metadata, Plan9Errno> {
        self.metadata_at(&self.path(path)?)
    }

    fn read_dir(&mut self, path: &str) -> Result<Vec<Plan9DirectoryEntry>, Plan9Errno> {
        let mut entries = Vec::new();
        for entry in native_fs::read_dir(self.path(path)?).map_err(map_native_9p_error)? {
            let entry = entry.map_err(map_native_9p_error)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Plan9Errno::InvalidArgument)?;
            entries.push(Plan9DirectoryEntry {
                name,
                metadata: self.metadata_at(&entry.path())?,
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn read(&mut self, path: &str, offset: u64, count: u32) -> Result<Vec<u8>, Plan9Errno> {
        let file = native_fs::File::open(self.path(path)?).map_err(map_native_9p_error)?;
        let mut bytes = vec![0; usize::try_from(count).map_err(|_| Plan9Errno::MessageTooLarge)?];
        let read = file
            .read_at(&mut bytes, offset)
            .map_err(map_native_9p_error)?;
        bytes.truncate(read);
        Ok(bytes)
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u32, Plan9Errno> {
        let file = native_fs::OpenOptions::new()
            .write(true)
            .open(self.path(path)?)
            .map_err(map_native_9p_error)?;
        let written = file.write_at(data, offset).map_err(map_native_9p_error)?;
        u32::try_from(written).map_err(|_| Plan9Errno::MessageTooLarge)
    }

    fn create_file(
        &mut self,
        path: &str,
        mode: u32,
        exclusive: bool,
        truncate: bool,
    ) -> Result<(), Plan9Errno> {
        let mut options = native_fs::OpenOptions::new();
        options.read(true).write(true).mode(mode & 0o7777);
        if exclusive {
            options.create_new(true);
        } else {
            options.create(true).truncate(truncate);
        }
        drop(
            options
                .open(self.path(path)?)
                .map_err(map_native_9p_error)?,
        );
        Ok(())
    }

    fn create_dir(&mut self, path: &str, _mode: u32) -> Result<(), Plan9Errno> {
        native_fs::create_dir(self.path(path)?).map_err(map_native_9p_error)
    }

    fn set_len(&mut self, path: &str, len: u64) -> Result<(), Plan9Errno> {
        let file = native_fs::OpenOptions::new()
            .write(true)
            .open(self.path(path)?)
            .map_err(map_native_9p_error)?;
        file.set_len(len).map_err(map_native_9p_error)
    }

    fn remove_file(&mut self, path: &str) -> Result<(), Plan9Errno> {
        native_fs::remove_file(self.path(path)?).map_err(map_native_9p_error)
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), Plan9Errno> {
        native_fs::remove_dir(self.path(path)?).map_err(map_native_9p_error)
    }

    fn rename(&mut self, source: &str, destination: &str) -> Result<(), Plan9Errno> {
        native_fs::rename(self.path(source)?, self.path(destination)?).map_err(map_native_9p_error)
    }

    fn sync(&mut self, path: &str, data_only: bool) -> Result<(), Plan9Errno> {
        let file = native_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.path(path)?)
            .map_err(map_native_9p_error)?;
        if data_only {
            file.sync_data()
        } else {
            file.sync_all()
        }
        .map_err(map_native_9p_error)
    }
}

#[cfg(test)]
fn map_native_9p_error(error: std::io::Error) -> Plan9Errno {
    match error.kind() {
        std::io::ErrorKind::NotFound => Plan9Errno::NotFound,
        std::io::ErrorKind::PermissionDenied => Plan9Errno::Permission,
        std::io::ErrorKind::AlreadyExists => Plan9Errno::AlreadyExists,
        std::io::ErrorKind::InvalidInput => Plan9Errno::InvalidArgument,
        std::io::ErrorKind::IsADirectory => Plan9Errno::IsDirectory,
        std::io::ErrorKind::NotADirectory => Plan9Errno::NotDirectory,
        std::io::ErrorKind::DirectoryNotEmpty => Plan9Errno::NotEmpty,
        _ => Plan9Errno::Io,
    }
}

/// Create the durable and ephemeral mount roots required by the seed realm.
pub(crate) fn ensure_layout() -> Result<(), SysError> {
    fs::create_dir_all(REALM_HOME)?;
    fs::create_dir_all(REALM_STATE)?;
    fs::create_dir_all(REALM_TMP)?;
    fs::create_dir_all(BLOB_ROOT)?;
    if fs::exists(FORMAT_MARKER)? {
        match fs::read(FORMAT_MARKER)?.as_slice() {
            CURRENT_FORMAT_MARKER => {}
            LEGACY_FORMAT_MARKER => fs::write(FORMAT_MARKER, CURRENT_FORMAT_MARKER)?,
            _ => {
                return Err(SysError::ApiError(
                    "unsupported AOS Realm storage format marker".to_string(),
                ));
            }
        }
    } else {
        fs::write(FORMAT_MARKER, CURRENT_FORMAT_MARKER)?;
    }
    Ok(())
}

/// Inspect the realm layout state without creating or migrating it.
pub(crate) fn layout_state() -> Result<&'static str, SysError> {
    if !fs::exists(FORMAT_MARKER)? {
        return Ok("uninitialized");
    }
    match fs::read(FORMAT_MARKER)?.as_slice() {
        CURRENT_FORMAT_MARKER => Ok("ready"),
        LEGACY_FORMAT_MARKER => Ok("migration-required"),
        _ => Err(SysError::ApiError(
            "unsupported AOS Realm storage format marker".to_string(),
        )),
    }
}

/// Read the selected home generation without mutating it.
pub(crate) fn home_status() -> Result<FsStatus, SysError> {
    RealmFs::new(AstridRealmStore)
        .status()
        .map_err(fs_to_sys_error)
}

/// Confirm that a guest CWD names an existing directory in one admitted mount.
pub(crate) fn validate_cwd(cwd: &str) -> Result<(), SysError> {
    let outer = resolve_guest_path(cwd, ".")
        .map_err(|error| SysError::ApiError(format!("invalid realm cwd: {error}")))?;
    let metadata = fs::metadata(&outer)?;
    if !metadata.is_dir() {
        return Err(SysError::ApiError(format!(
            "realm cwd is not a directory: {cwd}"
        )));
    }
    Ok(())
}

/// Map a guest path to an Astrid VFS path without revealing a host path.
pub(crate) fn resolve_guest_path(cwd: &str, requested: &str) -> Result<String, RealmIoError> {
    let absolute = canonical_guest_path(cwd, requested)?;
    map_absolute_path(&absolute)
}

/// Normalize one guest-visible path without converting it into an Astrid URI.
///
/// Presentation code uses this to retain the guest spelling and mount identity;
/// authority is still decided separately by `resolve_guest_path`.
pub(crate) fn canonical_guest_path(cwd: &str, requested: &str) -> Result<String, RealmIoError> {
    if cwd.is_empty()
        || !cwd.starts_with('/')
        || requested.is_empty()
        || cwd.contains('\0')
        || requested.contains('\0')
        || cwd.contains('\\')
        || requested.contains('\\')
    {
        return Err(RealmIoError::InvalidPath);
    }

    let joined = if requested.starts_with('/') {
        requested.to_string()
    } else if cwd == "/" {
        format!("/{requested}")
    } else {
        format!("{}/{requested}", cwd.trim_end_matches('/'))
    };
    let mut components = Vec::new();
    for component in joined.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(RealmIoError::InvalidPath);
                }
            }
            value if value.chars().any(char::is_control) => {
                return Err(RealmIoError::InvalidPath);
            }
            value => components.push(value),
        }
    }
    Ok(format!("/{}", components.join("/")))
}

fn map_absolute_path(path: &str) -> Result<String, RealmIoError> {
    if path == "/workspace" {
        return Ok("cwd://".to_string());
    }
    if let Some(relative) = path.strip_prefix("/workspace/") {
        return Ok(format!("cwd://{relative}"));
    }
    if path == "/home/agent" {
        return Ok(REALM_HOME.to_string());
    }
    if let Some(relative) = path.strip_prefix("/home/agent/") {
        return Ok(format!("{REALM_HOME}/{relative}"));
    }
    if path == "/tmp" {
        return Ok(REALM_TMP.to_string());
    }
    if let Some(relative) = path.strip_prefix("/tmp/") {
        return Ok(format!("{REALM_TMP}/{relative}"));
    }
    Err(RealmIoError::InvalidPath)
}

/// Realm host adapter whose authority is exactly the outer Astrid FS imports.
#[derive(Default)]
pub(crate) struct AstridRealmHost;

impl RealmHost for AstridRealmHost {
    fn open(
        &mut self,
        cwd: &str,
        path: &str,
        mode: OpenMode,
    ) -> Result<Box<dyn RealmFile>, RealmIoError> {
        let resolved = resolve_guest_path(cwd, path)?;
        let home_relative = resolved
            .strip_prefix(REALM_HOME)
            .and_then(|suffix| suffix.strip_prefix('/'))
            .map(str::to_string);
        let backing = match mode {
            // Astrid's current component host exposes reliable whole-file I/O;
            // its positional FileHandle port is not live yet. Buffering here
            // keeps that host limitation out of the private nested-WASM ABI.
            OpenMode::Read => {
                let bytes = if let Some(relative) = home_relative.as_deref() {
                    read_versioned_home_file(relative, &resolved)?
                } else {
                    fs::read(&resolved).map_err(map_sdk_error)?
                };
                if bytes.len() > MAX_SEED_FILE_BYTES {
                    return Err(RealmIoError::TooLarge);
                }
                AstridFileBacking::Read { bytes }
            }
            // Defer replacement until `close`, so a trapped guest cannot leave
            // a partially written file behind.
            OpenMode::WriteTruncate => {
                if let Some(relative) = home_relative.as_deref() {
                    AstridFileBacking::VersionedWrite {
                        relative: relative.to_string(),
                        projection_path: resolved,
                        bytes: Vec::new(),
                    }
                } else {
                    AstridFileBacking::DirectWrite {
                        path: resolved,
                        bytes: Vec::new(),
                    }
                }
            }
        };
        Ok(Box::new(AstridRealmFile { backing, offset: 0 }))
    }
}

struct AstridRealmFile {
    backing: AstridFileBacking,
    offset: usize,
}

enum AstridFileBacking {
    Read {
        bytes: Vec<u8>,
    },
    DirectWrite {
        path: String,
        bytes: Vec<u8>,
    },
    VersionedWrite {
        relative: String,
        projection_path: String,
        bytes: Vec<u8>,
    },
}

impl RealmFile for AstridRealmFile {
    fn read(&mut self, max_bytes: usize) -> Result<Vec<u8>, RealmIoError> {
        let AstridFileBacking::Read { bytes } = &self.backing else {
            return Err(RealmIoError::Unsupported);
        };
        let end = self
            .offset
            .checked_add(max_bytes)
            .ok_or(RealmIoError::TooLarge)?
            .min(bytes.len());
        let chunk = bytes
            .get(self.offset..end)
            .ok_or(RealmIoError::InvalidPath)?
            .to_vec();
        self.offset = end;
        Ok(chunk)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<usize, RealmIoError> {
        let buffered = match &mut self.backing {
            AstridFileBacking::DirectWrite { bytes, .. }
            | AstridFileBacking::VersionedWrite { bytes, .. } => bytes,
            AstridFileBacking::Read { .. } => return Err(RealmIoError::Unsupported),
        };
        let end = self
            .offset
            .checked_add(bytes.len())
            .ok_or(RealmIoError::TooLarge)?;
        if end > MAX_SEED_FILE_BYTES {
            return Err(RealmIoError::TooLarge);
        }
        if end > buffered.len() {
            buffered.resize(end, 0);
        }
        buffered[self.offset..end].copy_from_slice(bytes);
        self.offset = end;
        Ok(bytes.len())
    }

    fn close(&mut self) -> Result<(), RealmIoError> {
        match &self.backing {
            AstridFileBacking::Read { .. } => Ok(()),
            AstridFileBacking::DirectWrite { path, bytes } => {
                fs::write(path, bytes).map_err(map_sdk_error)
            }
            AstridFileBacking::VersionedWrite {
                relative,
                projection_path,
                bytes,
            } => {
                let mut filesystem = RealmFs::new(AstridRealmStore);
                filesystem
                    .write_file(relative, bytes)
                    .map_err(map_vfs_error)?;
                // This plain-file projection is a rebuildable compatibility
                // view. The CAS-selected content-addressed generation above is
                // authoritative if projection refresh fails or is interrupted.
                if let Err(error) = fs::write(projection_path, bytes) {
                    astrid_sdk::log::warn(format!(
                        "AOS Realm committed {relative} but could not refresh its legacy projection: {error}"
                    ));
                }
                Ok(())
            }
        }
    }
}

#[derive(Default)]
struct AstridRealmStore;

impl RealmStore for AstridRealmStore {
    fn read_head(&self) -> Result<Option<Vec<u8>>, StoreError> {
        kv::get_bytes_opt(HEAD_KEY).map_err(map_sdk_store_error)
    }

    fn compare_and_swap_head(
        &mut self,
        expected: Option<&[u8]>,
        new: &[u8],
    ) -> Result<bool, StoreError> {
        kv::cas(HEAD_KEY, expected, new).map_err(map_sdk_store_error)
    }

    fn get_blob(&self, digest: &BlobDigest) -> Result<Option<Vec<u8>>, StoreError> {
        let path = blob_path(digest);
        if !fs::exists(&path).map_err(map_sdk_store_error)? {
            return Ok(None);
        }
        fs::read(&path).map(Some).map_err(map_sdk_store_error)
    }

    fn put_blob(&mut self, digest: &BlobDigest, bytes: &[u8]) -> Result<(), StoreError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(StoreError::TooLarge);
        }
        let path = blob_path(digest);
        if fs::exists(&path).map_err(map_sdk_store_error)?
            && fs::read(&path).map_err(map_sdk_store_error)? == bytes
        {
            return Ok(());
        }
        // A prior interrupted first materialization may have left a corrupt
        // unreachable object at this digest path. Rewriting the expected bytes
        // repairs it before any head can select it.
        fs::write(&path, bytes).map_err(map_sdk_store_error)?;
        let stored = fs::read(&path).map_err(map_sdk_store_error)?;
        if stored == bytes {
            Ok(())
        } else {
            Err(StoreError::Corrupt(format!(
                "blob {} failed read-after-write verification",
                digest.as_str()
            )))
        }
    }
}

fn blob_path(digest: &BlobDigest) -> String {
    format!("{BLOB_ROOT}/{}", digest.as_str())
}

fn read_versioned_home_file(relative: &str, legacy_path: &str) -> Result<Vec<u8>, RealmIoError> {
    let mut filesystem = RealmFs::new(AstridRealmStore);
    match filesystem.read_file(relative) {
        Ok(bytes) => Ok(bytes),
        Err(FsError::NotFound) => {
            // Format-0 stored files directly beneath REALM_HOME. Import each
            // one lazily into the immutable store when it is first observed.
            let bytes = fs::read(legacy_path).map_err(map_sdk_error)?;
            filesystem
                .write_file(relative, &bytes)
                .map_err(map_vfs_error)?;
            Ok(bytes)
        }
        Err(error) => Err(map_vfs_error(error)),
    }
}

fn map_vfs_error(error: FsError) -> RealmIoError {
    match error {
        FsError::NotFound => RealmIoError::NotFound,
        FsError::InvalidPath => RealmIoError::InvalidPath,
        FsError::TooLarge | FsError::Store(StoreError::TooLarge) => RealmIoError::TooLarge,
        FsError::Store(StoreError::Denied) => RealmIoError::Denied,
        FsError::Contended
        | FsError::Corrupt(_)
        | FsError::Serialization(_)
        | FsError::Store(StoreError::Corrupt(_) | StoreError::Io(_)) => RealmIoError::Io,
    }
}

fn fs_to_sys_error(error: FsError) -> SysError {
    SysError::ApiError(error.to_string())
}

fn map_sdk_store_error(error: SysError) -> StoreError {
    match map_sdk_error(error) {
        RealmIoError::Denied => StoreError::Denied,
        RealmIoError::TooLarge => StoreError::TooLarge,
        other => StoreError::Io(other.to_string()),
    }
}

fn map_sdk_error(error: SysError) -> RealmIoError {
    match error {
        // astrid-sdk intentionally preserves the typed WIT error's Debug name
        // in HostError. Match that stable boundary instead of parsing the
        // human-facing Display sentence.
        SysError::HostError(code) => map_host_error_code(&code),
        SysError::ApiError(message) if message.contains("not found") => RealmIoError::NotFound,
        SysError::ApiError(message) if message.contains("denied") => RealmIoError::Denied,
        _ => RealmIoError::Io,
    }
}

fn map_host_error_code(code: &str) -> RealmIoError {
    match code {
        "NotFound" => RealmIoError::NotFound,
        "CapabilityDenied" | "Access" => RealmIoError::Denied,
        "InvalidPath" | "BoundaryEscape" => RealmIoError::InvalidPath,
        "IsDirectory" => RealmIoError::IsDirectory,
        "NotDirectory" => RealmIoError::NotDirectory,
        "TooLarge" | "QuotaExceeded" => RealmIoError::TooLarge,
        // Some current VFS backends surface their original OS error inside
        // `Unknown(...)`. Normalize only this HostError payload so the private
        // realm ABI still presents a stable fault class.
        _ => {
            let normalized: String = code
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            if normalized.contains("notfound") || normalized.contains("nosuchfileordirectory") {
                RealmIoError::NotFound
            } else if normalized.contains("permissiondenied")
                || normalized.contains("capabilitydenied")
            {
                RealmIoError::Denied
            } else {
                RealmIoError::Io
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_is_an_explicit_cwd_mount() {
        assert_eq!(
            resolve_guest_path("/workspace/project", "src/lib.rs"),
            Ok("cwd://project/src/lib.rs".to_string())
        );
        assert_eq!(
            resolve_guest_path("/workspace/project", "../Cargo.toml"),
            Ok("cwd://Cargo.toml".to_string())
        );
    }

    #[test]
    fn linux_9p_paths_remain_beneath_the_invocation_workspace() {
        assert_eq!(workspace_path(""), Ok("cwd://".to_string()));
        assert_eq!(
            workspace_path("src/lib.rs"),
            Ok("cwd://src/lib.rs".to_string())
        );
        for rejected in ["/etc/passwd", "../escape", "a//b", "a/./b", "line\nbreak"] {
            assert_eq!(workspace_path(rejected), Err(Plan9Errno::InvalidArgument));
        }
    }

    #[test]
    fn linux_9p_preserves_typed_sdk_filesystem_errors() {
        for (code, expected) in [
            ("NotFound", Plan9Errno::NotFound),
            ("CapabilityDenied", Plan9Errno::Permission),
            ("InvalidPath", Plan9Errno::InvalidArgument),
            ("IsDirectory", Plan9Errno::IsDirectory),
            ("NotDirectory", Plan9Errno::NotDirectory),
            ("NotEmpty", Plan9Errno::NotEmpty),
            ("Quota", Plan9Errno::NoSpace),
            ("AlreadyExists", Plan9Errno::AlreadyExists),
            ("Closed", Plan9Errno::BadFileDescriptor),
        ] {
            assert_eq!(
                map_sdk_9p_error(SysError::HostError(code.to_string())),
                expected
            );
        }
    }

    #[test]
    fn principal_home_maps_only_beneath_the_realm_root() {
        assert_eq!(
            resolve_guest_path("/home/agent", ".config/tool.toml"),
            Ok(format!("{REALM_HOME}/.config/tool.toml"))
        );
        assert_eq!(
            resolve_guest_path("/home/agent", "../../outside"),
            Err(RealmIoError::InvalidPath)
        );
    }

    #[test]
    fn unmounted_guest_paths_and_scheme_injection_fail_closed() {
        assert_eq!(
            resolve_guest_path("/workspace", "/etc/passwd"),
            Err(RealmIoError::InvalidPath)
        );
        assert_eq!(
            resolve_guest_path("/workspace", "home://other"),
            Ok("cwd://home:/other".to_string())
        );
        assert_eq!(
            resolve_guest_path("/workspace", ".."),
            Err(RealmIoError::InvalidPath)
        );
    }

    #[test]
    fn typed_sdk_errors_map_to_stable_realm_faults() {
        assert_eq!(
            map_sdk_error(SysError::HostError("NotFound".to_string())),
            RealmIoError::NotFound
        );
        assert_eq!(
            map_sdk_error(SysError::HostError("CapabilityDenied".to_string())),
            RealmIoError::Denied
        );
        assert_eq!(
            map_sdk_error(SysError::HostError("BoundaryEscape".to_string())),
            RealmIoError::InvalidPath
        );
        assert_eq!(
            map_sdk_error(SysError::HostError(
                "Unknown(\"No such file or directory (os error 2)\")".to_string()
            )),
            RealmIoError::NotFound
        );
    }

    #[test]
    fn buffered_seed_files_are_bounded() {
        let mut file = AstridRealmFile {
            backing: AstridFileBacking::DirectWrite {
                path: "unused".to_string(),
                bytes: Vec::new(),
            },
            offset: 0,
        };

        assert_eq!(
            file.write(&vec![0; MAX_SEED_FILE_BYTES + 1]),
            Err(RealmIoError::TooLarge)
        );
    }
}
