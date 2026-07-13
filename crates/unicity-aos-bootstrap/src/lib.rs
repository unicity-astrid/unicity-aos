//! Product-owned runtime layout and launcher for Unicity AOS.
//!
//! Astrid Runtime keeps its standalone `ASTRID_HOME` and `.astrid` compatibility
//! contract. AOS instead owns `~/.unicity-os` and passes a private runtime home
//! to the bundled runtime process only; it never changes the caller's process
//! environment or rewrites a standalone runtime installation.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};

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

    /// The installed bundled-runtime executable.
    #[must_use]
    pub fn runtime_binary(&self) -> PathBuf {
        self.runtime_home().join("bin").join(runtime_binary_name())
    }

    /// Create the product and bundled-runtime state directories.
    ///
    /// This intentionally creates neither a standalone Astrid home nor a
    /// project `.astrid` directory.
    ///
    /// # Errors
    /// Returns an error when the directories cannot be created.
    pub fn ensure_layout(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.runtime_home())
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
        command.env("ASTRID_HOME", self.runtime_home());
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

    /// Spawn the bundled runtime with product CLI arguments.
    ///
    /// Unicity AOS is a trusted distribution built on Astrid Runtime, so this
    /// path uses the runtime's normal local operator credentials. The runtime
    /// home remains scoped to this AOS installation.
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

    /// Run an Astrid Runtime command as an AOS product command.
    ///
    /// The runtime remains the authority for socket authentication and local
    /// credentials. Unicity provides only product-owned installation state and
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
    use super::{AosHome, runtime_binary_name};
    use std::ffi::OsString;
    use std::io::ErrorKind;
    use std::path::PathBuf;

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
    fn runtime_command_scopes_astrid_home_to_the_child() {
        let home = AosHome::from_root("/tmp/unicity-aos-test");
        let command = home.runtime_command();
        let runtime_home = command
            .get_envs()
            .find_map(|(name, value)| (name == "ASTRID_HOME").then_some(value))
            .flatten()
            .expect("runtime command sets ASTRID_HOME");

        assert_eq!(runtime_home, "/tmp/unicity-aos-test/runtime");
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
}
