//! Product-owned runtime layout and launcher for Unicity AOS.
//!
//! Astrid Runtime keeps its standalone `ASTRID_HOME` and `.astrid` compatibility
//! contract. AOS instead owns `~/.aos` and passes a private runtime home
//! to the bundled runtime process only; it never changes the caller's process
//! environment or rewrites a standalone runtime installation.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};

pub mod health;
mod migration;
pub mod status;
pub use migration::{LegacyDistro, MigrationOutcome};

const UNICITY_CE_MANIFEST: &str = include_str!("../../../distros/community/unicity-ce/Distro.toml");
const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(windows)]
pub(crate) const RUNTIME_EXECUTABLE_NAMES: &[&str] = &[
    "astrid.exe",
    "astrid-daemon.exe",
    "astrid-build.exe",
    "astrid-emit.exe",
];

#[cfg(not(windows))]
pub(crate) const RUNTIME_EXECUTABLE_NAMES: &[&str] =
    &["astrid", "astrid-daemon", "astrid-build", "astrid-emit"];

/// Product-owned per-project state directory selected for all AOS runtime access.
pub const AOS_WORKSPACE_STATE_DIR: &str = ".aos";

/// Product state owned by one Unicity AOS installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AosHome {
    root: PathBuf,
}

impl AosHome {
    /// Resolve the AOS home directory.
    ///
    /// `AOS_HOME` is an explicit product override. Otherwise AOS uses
    /// `~/.aos`, independently of Astrid Runtime's standalone home.
    ///
    /// # Errors
    /// Returns an error when neither `AOS_HOME` nor `HOME` is present.
    pub fn resolve() -> io::Result<Self> {
        Self::resolve_with(|name| std::env::var_os(name))
    }

    fn resolve_with<F>(get: F) -> io::Result<Self>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        if let Some(root) = get("AOS_HOME") {
            return Self::from_environment_root(root, "AOS_HOME");
        }

        let home = default_home(&get)?;
        let home = Self::validated_environment_root(home, default_home_name())?;
        Ok(Self::from_root(home.join(".aos")))
    }

    fn from_environment_root(root: OsString, variable: &str) -> io::Result<Self> {
        Ok(Self::from_root(Self::validated_environment_root(
            root, variable,
        )?))
    }

    fn validated_environment_root(root: OsString, variable: &str) -> io::Result<PathBuf> {
        if root.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{variable} must not be empty"),
            ));
        }

        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{variable} must be an absolute path"),
            ));
        }
        validate_path_entry(&root, variable)?;
        Ok(root)
    }

    /// Build an AOS home from an explicit root, useful for embedding and tests.
    #[must_use]
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The product-owned AOS root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The private home passed to the bundled Astrid Runtime process.
    #[must_use]
    pub fn runtime_home(&self) -> PathBuf {
        self.root.join("runtime")
    }

    /// The receipt written only after a successful standalone-runtime import.
    #[must_use]
    pub fn migration_receipt(&self) -> PathBuf {
        self.root.join("migrations/astrid-home-v1.json")
    }

    /// Product-managed location for the Unicity CE manifest bundled with this AOS
    /// release.
    #[must_use]
    pub fn unicity_ce_manifest_path(&self) -> PathBuf {
        self.root
            .join("distributions")
            .join("unicity-ce")
            .join("Distro.toml")
    }

    /// Product-versioned capsule assets installed alongside this AOS binary.
    ///
    /// `UNICITY_AOS_CAPSULE_DIR` is reserved for package managers which keep
    /// immutable product assets outside the mutable AOS home. The override must
    /// identify an absolute, real directory containing exactly the capsule set
    /// selected by the embedded Community Edition manifest.
    pub fn capsule_dir(&self) -> io::Result<PathBuf> {
        self.capsule_dir_with(|name| std::env::var_os(name))
    }

    fn capsule_dir_with<F>(&self, get: F) -> io::Result<PathBuf>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let configured = get("UNICITY_AOS_CAPSULE_DIR");
        let path = match configured {
            Some(path) => Self::validated_environment_root(path, "UNICITY_AOS_CAPSULE_DIR")?,
            None => self
                .root
                .join("releases")
                .join(PRODUCT_VERSION)
                .join("capsules"),
        };
        validate_capsule_dir(&path, &capsule_assets_from_manifest()?)
    }

    /// Materialize the Unicity CE manifest embedded in this product binary.
    ///
    /// The product CLI hands this local path to the neutral runtime, so first-run
    /// provisioning uses the manifest shipped with the installed AOS release rather
    /// than following a mutable repository branch.
    ///
    /// # Errors
    /// Returns an error when the product manifest cannot be written atomically.
    pub fn ensure_unicity_ce_manifest(&self) -> io::Result<PathBuf> {
        let path = self.unicity_ce_manifest_path();
        let capsule_dir = self.capsule_dir()?;
        let manifest = materialize_manifest(&capsule_dir)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "product manifest path must be a regular file",
                ));
            }
            Ok(_) if fs::read(&path)?.as_slice() == manifest.as_bytes() => return Ok(path),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.ensure_layout()?;
        create_private_dir(&self.root.join("distributions"))?;
        let parent = path.parent().expect("manifest path has a parent");
        create_private_dir(parent)?;
        let temporary = path.with_extension("toml.tmp");
        if let Ok(metadata) = fs::symlink_metadata(&temporary) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "temporary product manifest path must be a regular file",
                ));
            }
            fs::remove_file(&temporary)?;
        }
        fs::write(&temporary, manifest)?;
        set_private_file_permissions(&temporary)?;
        fs::rename(&temporary, &path)?;
        Ok(path)
    }

    /// Initialize the trusted CE system fleet before the runtime performs its
    /// daemon-backed grant preflight.
    ///
    /// A completely fresh Astrid home has no capsule capable of accepting CLI
    /// connections. The first pass uses Astrid's normal distro initializer to
    /// install the release-pinned CE fleet under `default` without starting the
    /// daemon. The requested init can then boot Astrid, authorize its operator,
    /// and apply grants through the normal kernel path.
    ///
    /// # Errors
    /// Returns an error when the bundled runtime or exact CE capsule set is
    /// unavailable or the runtime initializer exits unsuccessfully.
    pub fn prepare_unicity_ce_init<I, S>(&self, args: I) -> io::Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.ensure_runtime_available()?;
        let status = self
            .runtime_command_with_args(args)?
            .status()
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("failed to initialize the bundled CE system fleet: {error}"),
                )
            })?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "bundled CE system-fleet initializer exited with {status}"
            )));
        }
        Ok(())
    }

    /// The conventional standalone Astrid Runtime home that first-run AOS can offer
    /// to import. This does not inspect `ASTRID_HOME`: an override may name another
    /// product or service installation and must be supplied explicitly by the user.
    pub fn default_legacy_runtime_home() -> io::Result<PathBuf> {
        let home = default_home(&|name| std::env::var_os(name))?;
        Ok(PathBuf::from(home).join(".astrid"))
    }

    /// The installed bundled-runtime executable.
    #[must_use]
    pub fn runtime_binary(&self) -> PathBuf {
        self.runtime_binary_with(|name| std::env::var_os(name))
    }

    /// The daemon executable installed beside the bundled runtime CLI.
    #[must_use]
    pub fn runtime_daemon_binary(&self) -> PathBuf {
        self.runtime_binary()
            .with_file_name(runtime_daemon_binary_name())
    }

    fn runtime_binary_with<F>(&self, get: F) -> PathBuf
    where
        F: Fn(&str) -> Option<OsString>,
    {
        if let Some(path) = get("UNICITY_AOS_RUNTIME_BIN").map(PathBuf::from)
            && path.is_absolute()
        {
            return path;
        }
        self.runtime_home().join("bin").join(runtime_binary_name())
    }

    /// Import a standalone Astrid Runtime home into this product installation.
    ///
    /// This is an explicit copy operation. It leaves the standalone source in
    /// place so the operator retains a rollback path and historical provenance.
    ///
    /// # Errors
    /// Returns an error for unsafe paths, a running source runtime, an
    /// incompatible target, or a failed staging/validation operation.
    pub fn migrate_runtime_from(&self, source: impl AsRef<Path>) -> io::Result<MigrationOutcome> {
        migration::migrate_runtime(self, source.as_ref())
    }

    /// Legacy product distro locks preserved by the last runtime import.
    ///
    /// # Errors
    /// Returns an error when no migration receipt exists or the receipt cannot be
    /// read or decoded.
    pub fn imported_legacy_distros(&self) -> io::Result<Vec<LegacyDistro>> {
        migration::imported_legacy_distros(self)
    }

    /// Create the product and bundled-runtime state directories.
    ///
    /// This intentionally creates neither a standalone Astrid home nor a
    /// project `.astrid` directory.
    ///
    /// # Errors
    /// Returns an error when the directories cannot be created.
    pub fn ensure_layout(&self) -> io::Result<()> {
        create_private_dir(&self.root)?;
        create_private_dir(&self.runtime_home())?;
        create_private_dir(&self.runtime_home().join("bin"))
    }

    /// Build a command for the bundled runtime with a process-local home.
    ///
    /// The `ASTRID_HOME` override is applied only to this child process. AOS
    /// therefore can bundle the neutral runtime without changing the host
    /// shell, another AOS install, or a standalone Astrid Runtime installation.
    /// # Errors
    /// Returns an error when the private runtime bin or inherited host PATH
    /// cannot be represented safely as a child PATH.
    pub fn runtime_command(&self) -> io::Result<Command> {
        self.runtime_command_with_args(std::iter::empty::<&OsStr>())
    }

    /// Build a command for the bundled runtime with product CLI arguments.
    ///
    /// The command is executed directly, not through a shell. This preserves
    /// argument boundaries and leaves the runtime in charge of its established
    /// local socket, credentials, and operator protocol.
    /// # Errors
    /// Returns an error when the private runtime bin or inherited host PATH
    /// cannot be represented safely as a child PATH.
    pub fn runtime_command_with_args<I, S>(&self, args: I) -> io::Result<Command>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let runtime_binary = self.runtime_binary();
        self.runtime_executable_command(&runtime_binary, args)
    }

    fn runtime_executable_command<I, S>(&self, executable: &Path, args: I) -> io::Result<Command>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let runtime_bin = executable.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "bundled executable must have a parent directory",
            )
        })?;
        let mut command = Command::new(executable);
        command
            .env("ASTRID_HOME", self.runtime_home())
            .env("ASTRID_WORKSPACE_STATE_DIR", AOS_WORKSPACE_STATE_DIR)
            .env("ASTRID_ENFORCED_DISTRO", self.unicity_ce_manifest_path())
            .env(
                "PATH",
                Self::runtime_child_path(runtime_bin, std::env::var_os("PATH"))?,
            );
        command.args(args);
        Ok(command)
    }

    /// Build a foreground command for the bundled daemon.
    ///
    /// The command receives the same product-owned runtime home, workspace
    /// state directory, enforced distro, and `PATH` as ordinary AOS runtime
    /// dispatch. Daemon diagnostics are routed to stderr for process
    /// supervisors; this does not alter daemon lifetime.
    ///
    /// # Errors
    /// Returns an error when the bundled daemon or product capsule set is
    /// unavailable, or the child `PATH` cannot be represented safely.
    pub fn foreground_daemon_command(
        &self,
        workspace: Option<&Path>,
        verbose: bool,
    ) -> io::Result<Command> {
        let daemon_binary = self.runtime_daemon_binary();
        self.ensure_runtime_executable(&daemon_binary, "daemon")?;
        self.ensure_unicity_ce_manifest()?;
        let mut args = Vec::new();
        if let Some(workspace) = workspace {
            args.push(OsString::from("--workspace"));
            args.push(workspace.as_os_str().to_owned());
        }
        if verbose {
            args.push(OsString::from("--verbose"));
        }
        let mut command = self.runtime_executable_command(&daemon_binary, args)?;
        command.env("ASTRID_DAEMON_LOG_TARGET", "stderr");
        Ok(command)
    }

    fn runtime_child_path(runtime_bin: &Path, host_path: Option<OsString>) -> io::Result<OsString> {
        let mut child_path = vec![runtime_bin.to_path_buf()];
        if let Some(host_path) = host_path {
            child_path.extend(std::env::split_paths(&host_path));
        }
        std::env::join_paths(child_path).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot construct the bundled runtime PATH: {error}"),
            )
        })
    }

    /// Spawn the bundled runtime with its AOS-owned runtime home.
    ///
    /// # Errors
    /// Returns an error when the bundled executable is absent or cannot start.
    pub fn spawn_runtime(&self) -> io::Result<Child> {
        self.spawn_runtime_with_args(std::iter::empty::<&OsStr>())
    }

    /// Spawn the bundled runtime with runtime CLI arguments.
    ///
    /// This path uses the runtime's normal local operator credentials. The
    /// runtime home remains scoped to this AOS installation.
    ///
    /// # Errors
    /// Returns an error when the bundled executable is absent or cannot start.
    pub fn spawn_runtime_with_args<I, S>(&self, args: I) -> io::Result<Child>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.ensure_runtime_available()?;
        self.runtime_command_with_args(args)?.spawn()
    }

    /// Replace the current Unix process with a bundled runtime command.
    ///
    /// `exec` preserves the runtime's signal and exit semantics for terminal
    /// users and service managers; it never returns on success.
    ///
    /// # Errors
    /// Returns an error when the bundled executable is absent or cannot start.
    #[cfg(unix)]
    pub fn exec_runtime_with_args<I, S>(&self, args: I) -> io::Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        use std::os::unix::process::CommandExt;

        self.ensure_runtime_available()?;
        Err(self.runtime_command_with_args(args)?.exec())
    }

    /// Replace the current Unix process with the persistent bundled daemon.
    ///
    /// This is the process-supervisor path: the daemon receives signals
    /// directly and owns the final exit status. Callers decide the workspace
    /// argument, while AOS fixes the product home, distro, and workspace-state
    /// layout.
    ///
    /// # Errors
    /// Returns an error when the bundled daemon or product assets are
    /// unavailable, or the process cannot be replaced.
    #[cfg(unix)]
    pub fn exec_foreground_daemon(
        &self,
        workspace: Option<&Path>,
        verbose: bool,
    ) -> io::Result<()> {
        use std::os::unix::process::CommandExt;

        Err(self.foreground_daemon_command(workspace, verbose)?.exec())
    }

    /// Run a bundled-runtime command.
    ///
    /// The runtime remains the authority for socket authentication and local
    /// credentials. AOS provides only product-owned installation state and
    /// preserves the runtime's exit status for scripts and service managers.
    ///
    /// # Errors
    /// Returns an error when the bundled executable is absent or cannot start.
    pub fn run_runtime_with_args<I, S>(&self, args: I) -> io::Result<ExitStatus>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.spawn_runtime_with_args(args)?.wait()
    }

    fn ensure_runtime_available(&self) -> io::Result<()> {
        let binary = self.runtime_binary();
        self.ensure_runtime_executable(&binary, "runtime")?;
        self.ensure_unicity_ce_manifest().map(drop)
    }

    fn ensure_runtime_executable(&self, binary: &Path, label: &str) -> io::Result<()> {
        let metadata = fs::metadata(binary).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "bundled {label} executable not found at {}",
                        binary.display()
                    ),
                )
            } else {
                error
            }
        })?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "bundled {label} executable not found at {}",
                    binary.display()
                ),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "bundled {label} executable is not executable at {}",
                        binary.display()
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "AOS managed path must be a real directory: {}",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn validate_path_entry(path: &Path, variable: &str) -> io::Result<()> {
    std::env::join_paths(std::iter::once(path))
        .map(drop)
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{variable} cannot contain a platform PATH separator"),
            )
        })
}

fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(windows)]
fn default_home<F>(get: &F) -> io::Result<OsString>
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(home) = get("USERPROFILE") {
        return Ok(home);
    }

    match (get("HOMEDRIVE"), get("HOMEPATH")) {
        (Some(drive), Some(path)) => Ok(PathBuf::from(drive).join(path).into_os_string()),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "AOS_HOME, USERPROFILE, and HOMEDRIVE/HOMEPATH are all unset",
        )),
    }
}

#[cfg(not(windows))]
fn default_home<F>(get: &F) -> io::Result<OsString>
where
    F: Fn(&str) -> Option<OsString>,
{
    get("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "AOS_HOME and HOME are both unset"))
}

#[cfg(windows)]
const fn default_home_name() -> &'static str {
    "USERPROFILE"
}

#[cfg(not(windows))]
const fn default_home_name() -> &'static str {
    "HOME"
}

const fn runtime_binary_name() -> &'static str {
    RUNTIME_EXECUTABLE_NAMES[0]
}

const fn runtime_daemon_binary_name() -> &'static str {
    RUNTIME_EXECUTABLE_NAMES[1]
}

fn capsule_assets_from_manifest() -> io::Result<Vec<String>> {
    let manifest = UNICITY_CE_MANIFEST
        .parse::<toml::Value>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let capsules = manifest
        .get("capsule")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "embedded distro has no capsules",
            )
        })?;
    let mut assets = Vec::with_capacity(capsules.len());
    for capsule in capsules {
        let package = capsule
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "embedded capsule has no name")
            })?;
        let source = capsule
            .get("source")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "embedded capsule has no source")
            })?;
        let relative = Path::new(source);
        let mut components = relative.components();
        if components.next() != Some(std::path::Component::Normal(OsStr::new("capsules")))
            || components
                .next()
                .and_then(|component| match component {
                    std::path::Component::Normal(name) => Some(name),
                    _ => None,
                })
                .is_none()
            || components.next().is_some()
            || relative.extension() != Some(OsStr::new("capsule"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("embedded capsule source is not canonical: {source}"),
            ));
        }
        let asset = relative
            .file_name()
            .expect("validated capsule source has a filename")
            .to_str()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "capsule asset is not UTF-8")
            })?
            .to_owned();
        if asset != format!("{package}.capsule") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("embedded capsule source does not match package {package}"),
            ));
        }
        if assets.contains(&asset) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("embedded distro selects duplicate capsule asset {asset}"),
            ));
        }
        assets.push(asset);
    }
    Ok(assets)
}

fn validate_capsule_dir(path: &Path, expected: &[String]) -> io::Result<PathBuf> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "AOS capsule directory is unavailable at {}: {error}",
                path.display()
            ),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "AOS capsule directory must be a real directory: {}",
                path.display()
            ),
        ));
    }
    let canonical = path.canonicalize()?;
    let mut actual = Vec::new();
    for entry in fs::read_dir(&canonical)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "AOS capsule directory contains a non-regular entry: {}",
                    entry.path().display()
                ),
            ));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "AOS capsule directory contains a non-UTF-8 entry",
            )
        })?;
        actual.push(name);
    }
    actual.sort();
    let mut expected = expected.to_vec();
    expected.sort();
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "AOS capsule set differs from Community Edition; expected {}, found {}",
                expected.len(),
                actual.len()
            ),
        ));
    }
    Ok(canonical)
}

fn materialize_manifest(capsule_dir: &Path) -> io::Result<String> {
    let mut manifest = UNICITY_CE_MANIFEST
        .parse::<toml::Value>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let capsules = manifest
        .get_mut("capsule")
        .and_then(toml::Value::as_array_mut)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "embedded distro has no capsules",
            )
        })?;
    for capsule in capsules {
        let source = capsule
            .get("source")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "embedded capsule has no source")
            })?;
        let asset = Path::new(source).file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "embedded capsule source has no asset",
            )
        })?;
        let absolute = capsule_dir.join(asset);
        let absolute = absolute.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "AOS capsule directory must be valid UTF-8 for the TOML manifest",
            )
        })?;
        capsule["source"] = toml::Value::String(absolute.to_owned());
    }
    toml::to_string_pretty(&manifest)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::{
        AosHome, UNICITY_CE_MANIFEST, capsule_assets_from_manifest, materialize_manifest,
        runtime_binary_name, runtime_daemon_binary_name,
    };
    use std::ffi::OsString;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_home() -> PathBuf {
        std::env::temp_dir().join(format!(
            "unicity-aos-bundled-manifest-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ))
    }

    fn install_capsule_fixtures(root: &std::path::Path) -> PathBuf {
        let directory = root
            .join("releases")
            .join(env!("CARGO_PKG_VERSION"))
            .join("capsules");
        fs::create_dir_all(&directory).expect("create capsule fixture directory");
        for asset in capsule_assets_from_manifest().expect("read embedded capsule set") {
            fs::write(directory.join(asset), b"capsule fixture").expect("write capsule fixture");
        }
        directory
    }

    #[test]
    fn runtime_is_scoped_beneath_the_product_home() {
        let home = AosHome::from_root("/tmp/unicity-aos-test");
        assert_eq!(home.root(), PathBuf::from("/tmp/unicity-aos-test"));
        assert_eq!(
            home.runtime_home(),
            PathBuf::from("/tmp/unicity-aos-test/runtime")
        );
        assert_eq!(
            home.runtime_binary(),
            home.runtime_home().join("bin").join(runtime_binary_name())
        );
        assert_eq!(
            home.runtime_daemon_binary(),
            home.runtime_home()
                .join("bin")
                .join(runtime_daemon_binary_name())
        );
    }

    #[test]
    fn packaged_runtime_override_must_be_absolute() {
        let home = AosHome::from_root("/tmp/unicity-aos-test");
        assert_eq!(
            home.runtime_binary_with(|_| Some(OsString::from("runtime/astrid"))),
            home.runtime_home().join("bin").join(runtime_binary_name())
        );
        assert_eq!(
            home.runtime_binary_with(|_| Some(OsString::from("/opt/aos/runtime/bin/astrid"))),
            PathBuf::from("/opt/aos/runtime/bin/astrid")
        );
    }

    #[test]
    fn runtime_command_scopes_global_and_project_state_to_aos() {
        let home = AosHome::from_root("/tmp/unicity-aos-test");
        let caller_path = std::env::var_os("PATH");
        let command = home.runtime_command().expect("build runtime command");
        let env_value = |target: &str| {
            command
                .get_envs()
                .find_map(|(name, value)| (name == target).then_some(value))
                .flatten()
                .expect("runtime command sets product-scoped environment")
        };

        assert_eq!(env_value("ASTRID_HOME"), "/tmp/unicity-aos-test/runtime");
        assert_eq!(env_value("ASTRID_WORKSPACE_STATE_DIR"), ".aos");
        let path_entries: Vec<_> = std::env::split_paths(env_value("PATH")).collect();
        assert_eq!(
            path_entries.first(),
            Some(&PathBuf::from("/tmp/unicity-aos-test/runtime/bin"))
        );
        assert_eq!(std::env::var_os("PATH"), caller_path);
    }

    #[test]
    fn runtime_command_emplaces_the_bundled_unicity_ce_distro() {
        let home = AosHome::from_root("/tmp/unicity-aos-test");
        let command = home.runtime_command().expect("build runtime command");
        let distro = command
            .get_envs()
            .find_map(|(name, value)| (name == "ASTRID_ENFORCED_DISTRO").then_some(value))
            .flatten()
            .expect("runtime command sets ASTRID_ENFORCED_DISTRO");

        assert_eq!(
            distro,
            "/tmp/unicity-aos-test/distributions/unicity-ce/Distro.toml"
        );
    }

    #[test]
    fn runtime_command_forwards_product_cli_arguments_without_a_shell() {
        let home = AosHome::from_root("/tmp/unicity-aos-test");
        let command = home
            .runtime_command_with_args(["status", "--json"])
            .expect("build runtime command");
        let args: Vec<_> = command.get_args().collect();

        assert_eq!(args, ["status", "--json"]);
        assert_eq!(command.get_program(), home.runtime_binary());
    }

    #[test]
    fn foreground_daemon_uses_the_product_environment_without_ephemeral_mode() {
        let fixture = temporary_home();
        let home = AosHome::from_root(&fixture);
        install_capsule_fixtures(home.root());
        let runtime_bin = home.runtime_home().join("bin");
        fs::create_dir_all(&runtime_bin).expect("create runtime bin");
        fs::write(home.runtime_daemon_binary(), b"daemon").expect("write daemon fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let daemon = home.runtime_daemon_binary();
            let mut permissions = fs::metadata(&daemon)
                .expect("read daemon fixture metadata")
                .permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&daemon, permissions).expect("make daemon fixture executable");
        }

        let command = home
            .foreground_daemon_command(Some(std::path::Path::new("/workspace")), true)
            .expect("build foreground daemon command");

        assert_eq!(command.get_program(), home.runtime_daemon_binary());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--workspace", "/workspace", "--verbose"]
        );
        assert!(
            command.get_args().all(|argument| argument != "--ephemeral"),
            "foreground daemon must retain persistent lifetime"
        );
        let env_value = |target: &str| {
            command
                .get_envs()
                .find_map(|(name, value)| (name == target).then_some(value))
                .flatten()
                .expect("foreground daemon sets product environment")
        };
        assert_eq!(env_value("ASTRID_HOME"), home.runtime_home());
        assert_eq!(env_value("ASTRID_WORKSPACE_STATE_DIR"), ".aos");
        assert_eq!(env_value("ASTRID_DAEMON_LOG_TARGET"), "stderr");
        assert_eq!(
            env_value("ASTRID_ENFORCED_DISTRO"),
            home.unicity_ce_manifest_path()
        );
        fs::remove_dir_all(fixture).expect("remove foreground daemon fixture");
    }

    #[cfg(unix)]
    #[test]
    fn foreground_daemon_rejects_a_non_executable_fixture() {
        let fixture = temporary_home();
        let home = AosHome::from_root(&fixture);
        install_capsule_fixtures(home.root());
        let runtime_bin = home.runtime_home().join("bin");
        fs::create_dir_all(&runtime_bin).expect("create runtime bin");
        fs::write(home.runtime_daemon_binary(), b"daemon").expect("write daemon fixture");

        let error = home
            .foreground_daemon_command(None, false)
            .expect_err("non-executable daemon must fail before spawn");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(
            error
                .to_string()
                .contains("daemon executable is not executable")
        );
        fs::remove_dir_all(fixture).expect("remove foreground daemon fixture");
    }

    #[test]
    fn explicit_home_override_wins_over_the_host_home() {
        let home = AosHome::resolve_with(|name| match name {
            "AOS_HOME" => Some(OsString::from("/var/lib/aos")),
            "HOME" => Some(OsString::from("/home/operator")),
            _ => None,
        })
        .expect("absolute override resolves");

        assert_eq!(home.root(), PathBuf::from("/var/lib/aos"));
    }

    #[test]
    fn empty_or_relative_override_is_rejected() {
        for root in ["", "runtime"] {
            let error = AosHome::resolve_with(|name| match name {
                "AOS_HOME" => Some(OsString::from(root)),
                _ => None,
            })
            .expect_err("unsafe override must fail");
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
    }

    #[cfg(unix)]
    #[test]
    fn product_home_with_a_path_separator_is_rejected() {
        let error = AosHome::resolve_with(|name| match name {
            "AOS_HOME" => Some(OsString::from("/tmp/aos:test")),
            _ => None,
        })
        .expect_err("an unrepresentable runtime bin must fail closed");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);

        let home = AosHome::from_root("/tmp/aos:test");
        let error = home
            .runtime_command()
            .expect_err("explicit roots must fail at command construction too");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn child_path_preserves_host_entries_and_handles_an_absent_host_path() {
        let home = AosHome::from_root("/tmp/unicity-aos-test");
        let host_entries = [PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")];
        let host_path = std::env::join_paths(&host_entries).expect("build host PATH");
        let runtime_bin = home.runtime_home().join("bin");
        let child_path =
            AosHome::runtime_child_path(&runtime_bin, Some(host_path)).expect("build child PATH");
        assert_eq!(
            std::env::split_paths(&child_path).collect::<Vec<_>>(),
            [
                PathBuf::from("/tmp/unicity-aos-test/runtime/bin"),
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/usr/bin"),
            ]
        );

        let child_path =
            AosHome::runtime_child_path(&runtime_bin, None).expect("build private-only child PATH");
        assert_eq!(
            std::env::split_paths(&child_path).collect::<Vec<_>>(),
            [PathBuf::from("/tmp/unicity-aos-test/runtime/bin")]
        );
    }

    #[test]
    fn empty_default_home_is_rejected() {
        let error = AosHome::resolve_with(|name| match name {
            "HOME" => Some(OsString::new()),
            _ => None,
        })
        .expect_err("empty host home must fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn bundled_unicity_ce_manifest_is_restored_at_its_product_path() {
        let root = temporary_home();
        let home = AosHome::from_root(&root);
        let capsule_dir = install_capsule_fixtures(&root);
        let path = home
            .ensure_unicity_ce_manifest()
            .expect("write bundled manifest");
        assert_eq!(path, root.join("distributions/unicity-ce/Distro.toml"));
        assert!(
            fs::read_to_string(&path).expect("read manifest").contains(
                capsule_dir
                    .canonicalize()
                    .expect("canonical capsule fixture")
                    .to_str()
                    .expect("utf8 fixture path")
            )
        );

        fs::write(&path, "tampered").expect("tamper product manifest");
        home.ensure_unicity_ce_manifest()
            .expect("restore bundled manifest");
        assert!(
            fs::read_to_string(path)
                .expect("read restored manifest")
                .contains("id = \"unicity-ce\"")
        );
        fs::remove_dir_all(root).expect("remove temporary product home");
    }

    #[cfg(unix)]
    #[test]
    fn product_layout_and_manifest_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_home();
        let home = AosHome::from_root(&root);
        install_capsule_fixtures(&root);
        let manifest = home
            .ensure_unicity_ce_manifest()
            .expect("write private product manifest");

        for directory in [
            &root,
            &home.runtime_home(),
            &home.runtime_home().join("bin"),
            &root.join("distributions"),
            &root.join("distributions/unicity-ce"),
        ] {
            assert_eq!(
                fs::metadata(directory)
                    .expect("read directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(manifest)
                .expect("read manifest metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(root).expect("remove temporary product home");
    }

    #[cfg(unix)]
    #[test]
    fn product_manifest_refuses_a_symlink_destination() {
        use std::os::unix::fs::symlink;

        let root = temporary_home();
        let home = AosHome::from_root(&root);
        install_capsule_fixtures(&root);
        let manifest = home.unicity_ce_manifest_path();
        fs::create_dir_all(manifest.parent().expect("manifest parent"))
            .expect("create manifest parent");
        let external = root.join("outside.toml");
        fs::write(&external, UNICITY_CE_MANIFEST).expect("write external manifest");
        symlink(&external, &manifest).expect("symlink product manifest");

        assert_eq!(
            home.ensure_unicity_ce_manifest()
                .expect_err("manifest symlink must fail closed")
                .kind(),
            ErrorKind::InvalidInput
        );
        assert!(manifest.is_symlink());
        assert_eq!(fs::read(&external).unwrap(), UNICITY_CE_MANIFEST.as_bytes());
        fs::remove_dir_all(root).expect("remove temporary product home");
    }

    #[test]
    fn bundled_distro_version_matches_the_product_release() {
        let manifest: toml::Value = UNICITY_CE_MANIFEST.parse().expect("parse bundled manifest");
        assert_eq!(
            manifest["distro"]["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "the product binary and bundled Unicity CE manifest must release together"
        );
    }

    #[test]
    fn bundled_distro_uses_product_project_state() {
        let manifest: toml::Value = UNICITY_CE_MANIFEST.parse().expect("parse bundled manifest");
        let capsules = manifest["capsule"]
            .as_array()
            .expect("manifest capsule array");
        let cwd_dirs: Vec<_> = capsules
            .iter()
            .filter_map(|capsule| capsule.get("env"))
            .filter_map(|env| env.get("cwd_dir"))
            .filter_map(toml::Value::as_str)
            .collect();
        assert!(!cwd_dirs.is_empty(), "fixture must exercise project state");
        assert!(
            cwd_dirs.iter().all(|path| *path == ".aos"),
            "product capsules must not create Astrid-branded project state"
        );
    }

    #[test]
    fn capsule_override_must_be_absolute_real_and_exact() {
        let root = temporary_home();
        let home = AosHome::from_root(&root);
        let valid = install_capsule_fixtures(&root);
        assert_eq!(
            home.capsule_dir_with(|name| {
                (name == "UNICITY_AOS_CAPSULE_DIR").then(|| valid.clone().into_os_string())
            })
            .expect("valid package-manager capsule directory"),
            valid.canonicalize().expect("canonical capsule directory")
        );
        for invalid in [OsString::new(), OsString::from("relative/capsules")] {
            assert_eq!(
                home.capsule_dir_with(|_| Some(invalid.clone()))
                    .expect_err("invalid capsule override must fail")
                    .kind(),
                ErrorKind::InvalidInput
            );
        }
        fs::write(valid.join("unexpected.capsule"), b"unexpected")
            .expect("write unexpected capsule");
        assert_eq!(
            home.capsule_dir_with(|_| Some(valid.clone().into_os_string()))
                .expect_err("non-exact capsule set must fail")
                .kind(),
            ErrorKind::InvalidInput
        );
        fs::remove_dir_all(root).expect("remove temporary product home");
    }

    #[test]
    fn materialized_capsule_paths_are_toml_serialized_without_text_substitution() {
        let root = temporary_home().join("quoted-\"-capsules");
        let capsule_dir = install_capsule_fixtures(&root)
            .canonicalize()
            .expect("canonical unusual capsule path");
        let encoded = materialize_manifest(&capsule_dir).expect("materialize manifest");
        let decoded: toml::Value = encoded.parse().expect("parse materialized TOML");
        let capsules = decoded["capsule"].as_array().expect("capsule array");
        assert_eq!(capsules.len(), 21);
        assert!(capsules.iter().all(|capsule| {
            PathBuf::from(capsule["source"].as_str().expect("absolute source")).parent()
                == Some(capsule_dir.as_path())
        }));
        fs::remove_dir_all(
            root.parent()
                .expect("unusual fixture root has temporary parent"),
        )
        .expect("remove temporary product home");
    }

    #[test]
    fn missing_migration_receipt_is_reported_to_callers() {
        let root = temporary_home();
        let home = AosHome::from_root(&root);

        let error = home
            .imported_legacy_distros()
            .expect_err("missing receipt must not look like an empty import");
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }
}
