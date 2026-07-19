//! Typed path identities and audience-specific mount projections.

use aos_realm_runtime::RealmIoError;
use serde::Serialize;

use crate::host::{REALM_HOME, REALM_TMP, canonical_guest_path, resolve_guest_path};

pub(crate) const PATH_CONTRACT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PathConsumer {
    NestedCoreWasm,
    LinuxGuest,
    BareRv64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MountRole {
    Workspace,
    AgentHome,
    Temporary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProjectionState {
    Mounted,
    GuestRamOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReferenceStability {
    Invocation,
    PrincipalGeneration,
    RealmBoot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MountDurability {
    OuterCow,
    PrincipalPersistent,
    Ephemeral,
    RealmRam,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CommitPolicy {
    OuterPromotion,
    AtomicGeneration,
    DiscardOnRealmStop,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PathRef {
    mount: MountRole,
    mount_id: Option<String>,
    relative_path: String,
    guest_path: String,
    resource_uri: Option<String>,
    display_path: String,
    reference_stability: ReferenceStability,
    generation_at_admission: Option<u64>,
    realm_boot_sequence_at_admission: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct MountDescriptor {
    role: MountRole,
    mount_id: Option<String>,
    display_name: &'static str,
    guest_root: &'static str,
    declared_resource_root: &'static str,
    mode: &'static str,
    durability: MountDurability,
    commit_policy: CommitPolicy,
    projection: ProjectionState,
    reference_stability: ReferenceStability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct MountContext {
    version: u32,
    consumer: PathConsumer,
    active_cwd: Option<PathRef>,
    mounts: Vec<MountDescriptor>,
    physical_host_paths_visible: bool,
}

impl MountContext {
    pub(crate) fn for_execution(
        consumer: PathConsumer,
        cwd: &str,
        home_generation: Option<u64>,
        realm_boot_sequence: u64,
    ) -> Result<Self, RealmIoError> {
        if consumer == PathConsumer::BareRv64 {
            return Ok(Self {
                version: PATH_CONTRACT_VERSION,
                consumer,
                active_cwd: None,
                mounts: Vec::new(),
                physical_host_paths_visible: false,
            });
        }

        let active_cwd = path_ref(consumer, cwd, home_generation, realm_boot_sequence)?;
        Ok(Self {
            version: PATH_CONTRACT_VERSION,
            consumer,
            active_cwd: Some(active_cwd),
            mounts: mount_descriptors(consumer),
            physical_host_paths_visible: false,
        })
    }
}

fn path_ref(
    consumer: PathConsumer,
    requested: &str,
    home_generation: Option<u64>,
    realm_boot_sequence: u64,
) -> Result<PathRef, RealmIoError> {
    let guest_path = canonical_guest_path("/", requested)?;
    let (mount, guest_root, display_name, declared_stability) = classify_guest_path(&guest_path)?;
    let stability = if consumer == PathConsumer::LinuxGuest
        && matches!(mount, MountRole::AgentHome | MountRole::Temporary)
    {
        ReferenceStability::RealmBoot
    } else {
        declared_stability
    };
    let relative_path = guest_path
        .strip_prefix(guest_root)
        .and_then(|suffix| suffix.strip_prefix('/').or(Some(suffix)))
        .unwrap_or_default()
        .to_string();
    let display_path = if relative_path.is_empty() {
        display_name.to_string()
    } else {
        format!("{display_name}/{relative_path}")
    };
    let resource_uri = match (consumer, mount) {
        (PathConsumer::NestedCoreWasm, _) | (PathConsumer::LinuxGuest, MountRole::Workspace) => {
            Some(resolve_guest_path("/", &guest_path)?)
        }
        (PathConsumer::LinuxGuest | PathConsumer::BareRv64, _) => None,
    };
    let generation_at_admission =
        if mount == MountRole::AgentHome && consumer == PathConsumer::NestedCoreWasm {
            home_generation
        } else {
            None
        };

    Ok(PathRef {
        mount,
        mount_id: projection_mount_id(consumer, mount).map(str::to_string),
        relative_path,
        guest_path,
        resource_uri,
        display_path,
        reference_stability: stability,
        generation_at_admission,
        realm_boot_sequence_at_admission: (consumer == PathConsumer::LinuxGuest
            && stability == ReferenceStability::RealmBoot)
            .then_some(realm_boot_sequence),
    })
}

fn classify_guest_path(
    path: &str,
) -> Result<(MountRole, &'static str, &'static str, ReferenceStability), RealmIoError> {
    if path == "/workspace" || path.starts_with("/workspace/") {
        return Ok((
            MountRole::Workspace,
            "/workspace",
            "Workspace",
            ReferenceStability::Invocation,
        ));
    }
    if path == "/home/agent" || path.starts_with("/home/agent/") {
        return Ok((
            MountRole::AgentHome,
            "/home/agent",
            "Agent Home",
            ReferenceStability::PrincipalGeneration,
        ));
    }
    if path == "/tmp" || path.starts_with("/tmp/") {
        return Ok((
            MountRole::Temporary,
            "/tmp",
            "Temporary Files",
            ReferenceStability::RealmBoot,
        ));
    }
    Err(RealmIoError::InvalidPath)
}

const fn projection_mount_id(consumer: PathConsumer, mount: MountRole) -> Option<&'static str> {
    match (consumer, mount) {
        (PathConsumer::NestedCoreWasm, MountRole::AgentHome) => Some("realm-home:default"),
        (PathConsumer::NestedCoreWasm, MountRole::Temporary) => Some("realm-tmp:default"),
        (PathConsumer::LinuxGuest, MountRole::AgentHome | MountRole::Temporary) => {
            Some("linux-rootfs")
        }
        (PathConsumer::NestedCoreWasm | PathConsumer::LinuxGuest, MountRole::Workspace)
        | (PathConsumer::BareRv64, _) => None,
    }
}

fn mount_descriptors(consumer: PathConsumer) -> Vec<MountDescriptor> {
    let (workspace_projection, home_projection, tmp_projection) = match consumer {
        PathConsumer::NestedCoreWasm => (
            ProjectionState::Mounted,
            ProjectionState::Mounted,
            ProjectionState::Mounted,
        ),
        PathConsumer::LinuxGuest => (
            ProjectionState::Mounted,
            ProjectionState::GuestRamOnly,
            ProjectionState::GuestRamOnly,
        ),
        PathConsumer::BareRv64 => return Vec::new(),
    };
    let home_durability = if consumer == PathConsumer::LinuxGuest {
        MountDurability::RealmRam
    } else {
        MountDurability::PrincipalPersistent
    };
    let home_commit = if consumer == PathConsumer::LinuxGuest {
        CommitPolicy::DiscardOnRealmStop
    } else {
        CommitPolicy::AtomicGeneration
    };

    vec![
        MountDescriptor {
            role: MountRole::Workspace,
            mount_id: None,
            display_name: "Workspace",
            guest_root: "/workspace",
            declared_resource_root: "cwd://",
            mode: "read-write",
            durability: MountDurability::OuterCow,
            commit_policy: CommitPolicy::OuterPromotion,
            projection: workspace_projection,
            reference_stability: ReferenceStability::Invocation,
        },
        MountDescriptor {
            role: MountRole::AgentHome,
            mount_id: projection_mount_id(consumer, MountRole::AgentHome).map(str::to_string),
            display_name: "Agent Home",
            guest_root: "/home/agent",
            declared_resource_root: REALM_HOME,
            mode: "read-write",
            durability: home_durability,
            commit_policy: home_commit,
            projection: home_projection,
            reference_stability: if consumer == PathConsumer::LinuxGuest {
                ReferenceStability::RealmBoot
            } else {
                ReferenceStability::PrincipalGeneration
            },
        },
        MountDescriptor {
            role: MountRole::Temporary,
            mount_id: projection_mount_id(consumer, MountRole::Temporary).map(str::to_string),
            display_name: "Temporary Files",
            guest_root: "/tmp",
            declared_resource_root: REALM_TMP,
            mode: "read-write",
            durability: if consumer == PathConsumer::LinuxGuest {
                MountDurability::RealmRam
            } else {
                MountDurability::Ephemeral
            },
            commit_policy: CommitPolicy::DiscardOnRealmStop,
            projection: tmp_projection,
            reference_stability: ReferenceStability::RealmBoot,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_refs_are_invocation_scoped_and_have_no_fabricated_mount_id() {
        let context = MountContext::for_execution(
            PathConsumer::NestedCoreWasm,
            "/workspace/src/lib.rs",
            Some(7),
            9,
        )
        .expect("workspace path context");
        let cwd = context.active_cwd.expect("active cwd");

        assert_eq!(cwd.mount, MountRole::Workspace);
        assert_eq!(cwd.mount_id, None);
        assert_eq!(cwd.relative_path, "src/lib.rs");
        assert_eq!(cwd.guest_path, "/workspace/src/lib.rs");
        assert_eq!(cwd.resource_uri.as_deref(), Some("cwd://src/lib.rs"));
        assert_eq!(cwd.display_path, "Workspace/src/lib.rs");
        assert_eq!(cwd.reference_stability, ReferenceStability::Invocation);
        assert_eq!(cwd.generation_at_admission, None);
    }

    #[test]
    fn nested_home_refs_carry_the_selected_principal_generation() {
        let context = MountContext::for_execution(
            PathConsumer::NestedCoreWasm,
            "/home/agent/.config/aos",
            Some(42),
            9,
        )
        .expect("home path context");
        let cwd = context.active_cwd.expect("active cwd");

        assert_eq!(cwd.mount, MountRole::AgentHome);
        assert_eq!(cwd.mount_id.as_deref(), Some("realm-home:default"));
        assert_eq!(cwd.relative_path, ".config/aos");
        assert_eq!(cwd.display_path, "Agent Home/.config/aos");
        assert_eq!(
            cwd.reference_stability,
            ReferenceStability::PrincipalGeneration
        );
        assert_eq!(cwd.generation_at_admission, Some(42));
    }

    #[test]
    fn linux_context_distinguishes_invocation_workspace_from_ram_only_paths() {
        let context =
            MountContext::for_execution(PathConsumer::LinuxGuest, "/home/agent", Some(42), 9)
                .expect("Linux path context");
        let cwd = context.active_cwd.expect("active cwd");
        let workspace = context
            .mounts
            .iter()
            .find(|mount| mount.role == MountRole::Workspace)
            .expect("workspace descriptor");
        let home = context
            .mounts
            .iter()
            .find(|mount| mount.role == MountRole::AgentHome)
            .expect("home descriptor");

        assert_eq!(cwd.resource_uri, None);
        assert_eq!(cwd.mount_id.as_deref(), Some("linux-rootfs"));
        assert_eq!(cwd.generation_at_admission, None);
        assert_eq!(cwd.realm_boot_sequence_at_admission, Some(9));
        assert_eq!(cwd.reference_stability, ReferenceStability::RealmBoot);
        assert_eq!(workspace.projection, ProjectionState::Mounted);
        assert_eq!(home.projection, ProjectionState::GuestRamOnly);
        assert_eq!(home.mount_id.as_deref(), Some("linux-rootfs"));
        assert_eq!(home.durability, MountDurability::RealmRam);
        assert_eq!(home.commit_policy, CommitPolicy::DiscardOnRealmStop);
    }

    #[test]
    fn linux_workspace_ref_names_the_real_invocation_resource() {
        let context =
            MountContext::for_execution(PathConsumer::LinuxGuest, "/workspace/src", Some(42), 9)
                .expect("Linux workspace context");
        let cwd = context.active_cwd.expect("active cwd");

        assert_eq!(cwd.mount, MountRole::Workspace);
        assert_eq!(cwd.mount_id, None);
        assert_eq!(cwd.resource_uri.as_deref(), Some("cwd://src"));
        assert_eq!(cwd.reference_stability, ReferenceStability::Invocation);
        assert_eq!(cwd.realm_boot_sequence_at_admission, None);
    }

    #[test]
    fn bare_rv64_has_no_fabricated_filesystem_context() {
        let context =
            MountContext::for_execution(PathConsumer::BareRv64, "/workspace", Some(42), 9)
                .expect("bare RV64 path context");

        assert_eq!(context.consumer, PathConsumer::BareRv64);
        assert_eq!(context.active_cwd, None);
        assert!(context.mounts.is_empty());
        assert!(!context.physical_host_paths_visible);
    }

    #[test]
    fn host_and_unmounted_paths_never_become_path_refs() {
        for path in [
            "/Users/alice/project/file.rs",
            "C:\\Users\\alice\\file.rs",
            "/etc/passwd",
        ] {
            assert_eq!(
                MountContext::for_execution(PathConsumer::NestedCoreWasm, path, None, 9),
                Err(RealmIoError::InvalidPath)
            );
        }
    }

    #[test]
    fn serialized_context_contains_no_physical_path_or_false_home_resource_uri() {
        let context =
            MountContext::for_execution(PathConsumer::LinuxGuest, "/home/agent", Some(42), 9)
                .expect("Linux path context");
        let json = serde_json::to_string(&context).expect("serialize path context");

        assert!(json.contains("\"physical_host_paths_visible\":false"));
        assert!(json.contains("\"projection\":\"mounted\""));
        assert!(json.contains("\"projection\":\"guest-ram-only\""));
        assert!(json.contains("\"resource_uri\":null"));
        assert!(!json.contains("/Users/"));
    }
}
