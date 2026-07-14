//! Product-owned runtime layout and launcher for Unicity AOS.
//!
//! Astrid Runtime keeps its standalone `ASTRID_HOME` and `.astrid` compatibility
//! contract. AOS instead owns `~/.unicity-os` and passes a private runtime home
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
const AOS_WORKSPACE_STATE_DIR: &str = ".unicity-os";

/// Product state owned by one Unicity AOS installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AosHome {
    root: PathBuf,
}

impl AosHome {
    /// Resolve the AOS home directory.
    ///
    /// `UNICITY_AOS_HOME` is an explicit product override. Otherwise AOS uses
    /// `~/.unicity-os`, independently of Astrid Runtime's standalone home.
    ///
    /// # Errors
    /// Returns an error when neither `UNICITY_AOS_HOME` nor `HOME` is present.
    pub fn resolve() -> io::Result<Self> {
        Self::resolve_with(|name| std::env::var_os(name))
    }

    fn resolve_with<F>(get: F) -> io::Result<Self>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        if let Some(root) = get("UNICITY_AOS_HOME") {
            return Self::from_environment_root(root, "UNICITY_AOS_HOME");
        }

        let home = default_home(&get)?;
        let home = Self::validated_environment_root(home, default_home_name())?;
        Ok(Self::from_root(home.join(".unicity-os")))
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
        if fs::read(&path).ok().as_deref() == Some(UNICITY_CE_MANIFEST.as_bytes()) {
            return Ok(path);
        }
        self.ensure_layout()?;
        create_private_dir(&self.root.join("distributions"))?;
        let parent = path.parent().expect("manifest path has a parent");
        create_private_dir(parent)?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, UNICITY_CE_MANIFEST)?;
        set_private_file_permissions(&temporary)?;
        fs::rename(&temporary, &path)?;
        Ok(path)
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
    #[must_use]
    pub fn runtime_command(&self) -> Command {
        self.runtime_command_with_args(std::iter::empty::<&OsStr>())
    }

    /// Build a command for the bundled runtime with product CLI arguments.
    ///
    /// The command is executed directly, not through a shell. This preserves
    /// argument boundaries and leaves the runtime in charge of its established
    /// local socket, credentials, and operator protocol.
    #[must_use]
    pub fn runtime_command_with_args<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(self.runtime_binary());
        command
            .env("ASTRID_HOME", self.runtime_home())
            .env("ASTRID_WORKSPACE_STATE_DIR", AOS_WORKSPACE_STATE_DIR);
        command.args(args);
        command
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
    /// The runtime home remains scoped to this AOS installation.
    ///
    /// # Errors
    /// Returns an error when the bundled executable is absent or cannot start.
    pub fn spawn_runtime_with_args<I, S>(&self, args: I) -> io::Result<Child>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.ensure_runtime_available()?;
        self.runtime_command_with_args(args).spawn()
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
        Err(self.runtime_command_with_args(args).exec())
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
        if !binary.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "bundled runtime executable not found at {}",
                    binary.display()
                ),
            ));
        }
        self.ensure_layout()
    }
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
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
            "UNICITY_AOS_HOME, USERPROFILE, and HOMEDRIVE/HOMEPATH are all unset",
        )),
    }
}

#[cfg(not(windows))]
fn default_home<F>(get: &F) -> io::Result<OsString>
where
    F: Fn(&str) -> Option<OsString>,
{
    get("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "UNICITY_AOS_HOME and HOME are both unset",
        )
    })
}

#[cfg(windows)]
const fn default_home_name() -> &'static str {
    "USERPROFILE"
}

#[cfg(not(windows))]
const fn default_home_name() -> &'static str {
    "HOME"
}

#[cfg(windows)]
const fn runtime_binary_name() -> &'static str {
    "astrid.exe"
}

#[cfg(not(windows))]
const fn runtime_binary_name() -> &'static str {
    "astrid"
}

#[cfg(test)]
mod tests {
    use super::{AosHome, UNICITY_CE_MANIFEST, runtime_binary_name};
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
        let command = home.runtime_command();
        let env_value = |target: &str| {
            command
                .get_envs()
                .find_map(|(name, value)| (name == target).then_some(value))
                .flatten()
                .expect("runtime command sets product-scoped environment")
        };

        assert_eq!(env_value("ASTRID_HOME"), "/tmp/unicity-aos-test/runtime");
        assert_eq!(env_value("ASTRID_WORKSPACE_STATE_DIR"), ".unicity-os");
    }

    #[test]
    fn runtime_command_forwards_product_cli_arguments_without_a_shell() {
        let home = AosHome::from_root("/tmp/unicity-aos-test");
        let command = home.runtime_command_with_args(["status", "--json"]);
        let args: Vec<_> = command.get_args().collect();

        assert_eq!(args, ["status", "--json"]);
        assert_eq!(command.get_program(), home.runtime_binary());
    }

    #[test]
    fn explicit_home_override_wins_over_the_host_home() {
        let home = AosHome::resolve_with(|name| match name {
            "UNICITY_AOS_HOME" => Some(OsString::from("/var/lib/unicity-aos")),
            "HOME" => Some(OsString::from("/home/operator")),
            _ => None,
        })
        .expect("absolute override resolves");

        assert_eq!(home.root(), PathBuf::from("/var/lib/unicity-aos"));
    }

    #[test]
    fn empty_or_relative_override_is_rejected() {
        for root in ["", "runtime"] {
            let error = AosHome::resolve_with(|name| match name {
                "UNICITY_AOS_HOME" => Some(OsString::from(root)),
                _ => None,
            })
            .expect_err("unsafe override must fail");
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
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
        let path = home
            .ensure_unicity_ce_manifest()
            .expect("write bundled manifest");
        assert_eq!(path, root.join("distributions/unicity-ce/Distro.toml"));
        assert!(
            fs::read_to_string(&path)
                .expect("read manifest")
                .contains("id = \"unicity-ce\"")
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
            cwd_dirs.iter().all(|path| *path == ".unicity-os"),
            "product capsules must not create Astrid-branded project state"
        );
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
