//! In-Store state for one principal-affine AOS Realm component instance.

use super::*;
use std::cell::RefCell;
const BOOT_SEQUENCE_KEY: &str = "realm/default/actor/boot-sequence";
const BOOT_SEQUENCE_CAS_ATTEMPTS: usize = 8;

thread_local! {
    /// A WebAssembly component instance is guest-single-threaded. Keeping the
    /// singleton in guest TLS avoids imposing host `Send`/`Sync` semantics on
    /// the semantic kernel's intentionally local `Rc<RefCell<_>>` graph.
    static RESIDENT_REALM: RefCell<ResidentRealm> = RefCell::new(ResidentRealm::default());
}

pub(crate) fn execute_resident(principal: &str, args: ExecArgs) -> Result<String, SysError> {
    RESIDENT_REALM.with(|state| {
        state
            .try_borrow_mut()
            .map_err(|_| SysError::ApiError("principal-affine Realm was re-entered".to_string()))?
            .execute(principal, args)
    })
}

pub(crate) fn status_resident(principal: &str, args: StatusArgs) -> Result<String, SysError> {
    RESIDENT_REALM.with(|state| {
        state
            .try_borrow_mut()
            .map_err(|_| SysError::ApiError("principal-affine Realm was re-entered".to_string()))?
            .status(principal, args)
    })
}

struct PrincipalRealm {
    boot_sequence: u64,
    commands_completed: u64,
    machine: RealmMachine,
    linux: LinuxActivity,
}

impl PrincipalRealm {
    fn new(boot_sequence: u64) -> Self {
        Self {
            boot_sequence,
            commands_completed: 0,
            machine: RealmMachine::with_generation(boot_sequence),
            linux: LinuxActivity::default(),
        }
    }

    fn snapshot(&self) -> ActorSnapshot {
        ActorSnapshot {
            state: "running",
            boot_sequence: self.boot_sequence,
            commands_completed: self.commands_completed,
            machine: self.machine.status(),
            linux: self.linux.snapshot(),
        }
    }
}

#[derive(Default)]
struct LinuxActivity {
    machine: Option<Rv64Machine>,
    boot_executions: u64,
    commands_completed: u64,
    clean_shutdowns: u64,
    guest_steps: u64,
    last_outcome: Option<&'static str>,
    last_exit_status: Option<i32>,
}

impl LinuxActivity {
    fn execute(
        &mut self,
        args: ExecArgs,
        principal: &str,
        boot_sequence: u64,
        next_process_id: Option<u64>,
        home_generation: u64,
    ) -> Result<ExecResponse, SysError> {
        let selected = select_program(&args)?;
        let SelectedExecution::Linux(action) = selected.execution else {
            return Err(SysError::ApiError(
                "non-Linux command reached the Linux supervisor".to_string(),
            ));
        };
        let requested_cwd = args.cwd.clone();
        let cwd = linux_effective_cwd(action, requested_cwd.as_deref())?.to_string();
        let hard_fuel = selected.execution.hard_fuel();
        let limits = RunLimits {
            fuel: args.fuel.unwrap_or(hard_fuel).min(hard_fuel),
            memory_bytes: selected.execution.memory_bytes(),
            output_bytes: args
                .max_output_bytes
                .unwrap_or(HARD_MAX_OUTPUT_BYTES)
                .min(HARD_MAX_OUTPUT_BYTES),
        };
        let command = selected.argv.get(1).map(String::as_str);
        let report = execute_linux_resident(&mut self.machine, action, command, limits)?;

        if report.booted {
            self.boot_executions = self.boot_executions.saturating_add(1);
        }
        if report.command_completed {
            self.commands_completed = self.commands_completed.saturating_add(1);
        }
        if report.clean_shutdown {
            self.clean_shutdowns = self.clean_shutdowns.saturating_add(1);
        }
        self.guest_steps = self.guest_steps.saturating_add(report.fuel_consumed);
        self.last_outcome = Some(report.outcome);
        self.last_exit_status = report.exit_status;
        let path_context = MountContext::for_execution(
            selected.execution.path_consumer(),
            &cwd,
            Some(home_generation),
            boot_sequence,
        )
        .map_err(|error| {
            SysError::ApiError(format!(
                "failed to describe Linux path context: {}",
                io_error_name(error)
            ))
        })?;

        Ok(ExecResponse {
            realm: REALM_NAME,
            owner_principal: principal.to_string(),
            program: selected.name.to_string(),
            execution_backend: selected.execution.backend(),
            argv: selected.argv,
            requested_cwd,
            cwd,
            path_context,
            outcome: report.outcome,
            exit_status: report.exit_status,
            fault: report.fault,
            stdout: String::from_utf8_lossy(&report.stdout).into_owned(),
            stderr: String::new(),
            fuel_consumed: report.fuel_consumed,
            memory_limit_bytes: limits.memory_bytes,
            suspensions: report.suspensions,
            processes: 0,
            realm_boot_sequence: boot_sequence,
            process_ids: Vec::new(),
            next_process_id,
        })
    }

    const fn snapshot(&self) -> LinuxSnapshot {
        LinuxSnapshot {
            state: if self.machine.is_some() {
                "running"
            } else {
                "cold"
            },
            boot_executions: self.boot_executions,
            clean_shutdowns: self.clean_shutdowns,
            guest_steps: self.guest_steps,
            last_outcome: self.last_outcome,
            last_exit_status: self.last_exit_status,
            commands_completed: self.commands_completed,
        }
    }
}

/// Stateful singleton held inside one principal-affine Wasmtime Store.
///
/// Astrid permanently binds that Store to a kernel-verified principal. The
/// owner check here independently fails closed if a runtime regression ever
/// attempts to retarget the component instance.
#[derive(Default)]
pub(crate) struct ResidentRealm {
    owner_principal: Option<String>,
    realm: Option<PrincipalRealm>,
}

impl ResidentRealm {
    fn bind_owner(&mut self, principal: &str) -> Result<(), SysError> {
        match self.owner_principal.as_deref() {
            Some(owner) if owner != principal => Err(SysError::ApiError(format!(
                "principal-affine Realm Store belongs to `{owner}`, not `{principal}`"
            ))),
            Some(_) => Ok(()),
            None => {
                self.owner_principal = Some(principal.to_string());
                Ok(())
            }
        }
    }

    fn realm_with_boot(
        &mut self,
        principal: &str,
        load_boot: impl FnOnce() -> Result<u64, SysError>,
    ) -> Result<&mut PrincipalRealm, SysError> {
        self.bind_owner(principal)?;
        if self.realm.is_none() {
            let boot_sequence = load_boot()?;
            self.realm = Some(PrincipalRealm::new(boot_sequence));
        }
        self.realm
            .as_mut()
            .ok_or_else(|| SysError::ApiError("resident Realm state disappeared".to_string()))
    }

    fn execute_with_boot(
        &mut self,
        principal: &str,
        args: ExecArgs,
        realm_host: Box<dyn RealmHost>,
        home_generation: u64,
        load_boot: impl FnOnce() -> Result<u64, SysError>,
    ) -> Result<ExecResponse, SysError> {
        let realm = self.realm_with_boot(principal, load_boot)?;
        let selected = select_program(&args)?;
        let response = if matches!(selected.execution, SelectedExecution::Linux(_)) {
            realm.linux.execute(
                args,
                principal,
                realm.boot_sequence,
                realm.machine.status().next_process_id.map(|id| id.get()),
                home_generation,
            )?
        } else {
            run_command_in_machine(
                args,
                principal.to_string(),
                realm_host,
                &mut realm.machine,
                realm.boot_sequence,
                home_generation,
            )?
        };
        realm.commands_completed = realm.commands_completed.saturating_add(1);
        Ok(response)
    }

    #[cfg(test)]
    fn snapshot_with_boot(
        &mut self,
        principal: &str,
        load_boot: impl FnOnce() -> Result<u64, SysError>,
    ) -> Result<ActorSnapshot, SysError> {
        self.realm_with_boot(principal, load_boot)
            .map(|realm| realm.snapshot())
    }

    fn snapshot(&self, principal: &str) -> ActorSnapshot {
        if self.owner_principal.as_deref() == Some(principal) {
            self.realm
                .as_ref()
                .map(PrincipalRealm::snapshot)
                .unwrap_or_else(ActorSnapshot::idle)
        } else {
            ActorSnapshot::idle()
        }
    }

    pub(crate) fn execute(&mut self, principal: &str, args: ExecArgs) -> Result<String, SysError> {
        self.bind_owner(principal)?;
        ensure_layout()?;
        let selected = select_program(&args)?;
        match selected.execution {
            SelectedExecution::Linux(action) => {
                let _ = linux_effective_cwd(action, args.cwd.as_deref())?;
            }
            _ => validate_cwd(args.cwd.as_deref().unwrap_or(DEFAULT_CWD))?,
        }
        let home_generation = home_status()?.generation;
        let response = self.execute_with_boot(
            principal,
            args,
            Box::<AstridRealmHost>::default(),
            home_generation,
            next_boot_sequence,
        )?;
        serde_json::to_string(&response).map_err(|error| SysError::ApiError(error.to_string()))
    }

    pub(crate) fn status(
        &mut self,
        principal: &str,
        _args: StatusArgs,
    ) -> Result<String, SysError> {
        self.bind_owner(principal)?;
        let layout = layout_state()?;
        let filesystem = home_status()?;
        // Status is declared read-only. In particular, observing a principal
        // that has not executed must not allocate a machine or advance its
        // durable boot sequence.
        let actor = self.snapshot(principal);
        let response = status_response(principal.to_string(), layout, filesystem, actor);
        serde_json::to_string(&response).map_err(|error| SysError::ApiError(error.to_string()))
    }
}

fn next_boot_sequence() -> Result<u64, SysError> {
    for _ in 0..BOOT_SEQUENCE_CAS_ATTEMPTS {
        let current = kv::get_bytes_opt(BOOT_SEQUENCE_KEY)?;
        let (next, encoded) = increment_boot_sequence(current.as_deref())?;
        if kv::cas(BOOT_SEQUENCE_KEY, current.as_deref(), &encoded)? {
            return Ok(next);
        }
    }
    Err(SysError::ApiError(
        "Realm boot sequence remained contended".to_string(),
    ))
}

fn increment_boot_sequence(current: Option<&[u8]>) -> Result<(u64, Vec<u8>), SysError> {
    let current = current
        .map(serde_json::from_slice::<u64>)
        .transpose()
        .map_err(|error| SysError::ApiError(format!("Realm boot sequence is malformed: {error}")))?
        .unwrap_or(0);
    let next = current
        .checked_add(1)
        .ok_or_else(|| SysError::ApiError("Realm boot sequence exhausted".to_string()))?;
    let encoded = serde_json::to_vec(&next).map_err(|error| {
        SysError::ApiError(format!("Realm boot sequence encode failed: {error}"))
    })?;
    Ok((next, encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestHost;

    impl RealmHost for TestHost {
        fn open(
            &mut self,
            _cwd: &str,
            _path: &str,
            _mode: aos_realm_runtime::OpenMode,
        ) -> Result<Box<dyn aos_realm_runtime::RealmFile>, RealmIoError> {
            Err(RealmIoError::Denied)
        }
    }

    fn echo(value: &str) -> ExecArgs {
        ExecArgs {
            command: Some("echo".to_string()),
            args: vec![value.to_string()],
            ..ExecArgs::default()
        }
    }

    #[test]
    fn one_resident_store_keeps_monotonic_pids_and_rejects_retargeting() {
        let mut alice = ResidentRealm::default();
        let alice_first = alice
            .execute_with_boot("alice", echo("one"), Box::<TestHost>::default(), 0, || {
                Ok(7)
            })
            .expect("alice first command");
        let alice_second = alice
            .execute_with_boot("alice", echo("two"), Box::<TestHost>::default(), 0, || {
                panic!("existing principal must not allocate another boot")
            })
            .expect("alice second command");
        let error = alice
            .execute_with_boot("bob", echo("other"), Box::<TestHost>::default(), 0, || {
                Ok(11)
            })
            .expect_err("a resident Store cannot cross principals");

        assert_eq!(alice_first.realm_boot_sequence, 7);
        assert_eq!(alice_first.process_ids, vec![1]);
        assert_eq!(alice_second.process_ids, vec![2]);
        assert_eq!(alice_second.next_process_id, Some(3));
        assert!(error.to_string().contains("belongs to `alice`, not `bob`"));
    }

    #[test]
    fn separate_resident_stores_isolate_principal_machines() {
        let mut alice = ResidentRealm::default();
        let mut bob = ResidentRealm::default();
        let alice_first = alice
            .execute_with_boot("alice", echo("one"), Box::<TestHost>::default(), 0, || {
                Ok(7)
            })
            .expect("alice command");
        let bob_first = bob
            .execute_with_boot("bob", echo("other"), Box::<TestHost>::default(), 0, || {
                Ok(11)
            })
            .expect("bob command");

        assert_eq!(alice_first.realm_boot_sequence, 7);
        assert_eq!(bob_first.realm_boot_sequence, 11);
        assert_eq!(alice_first.process_ids, vec![1]);
        assert_eq!(bob_first.process_ids, vec![1]);
    }

    #[test]
    fn pipeline_ids_share_the_principals_monotonic_boot_namespace() {
        let mut actor = ResidentRealm::default();
        let first = actor
            .execute_with_boot("alice", echo("seed"), Box::<TestHost>::default(), 0, || {
                Ok(3)
            })
            .expect("first command");
        let pipeline = actor
            .execute_with_boot(
                "alice",
                ExecArgs {
                    command: Some("pipe-echo".to_string()),
                    args: vec!["through actor".to_string()],
                    ..ExecArgs::default()
                },
                Box::<TestHost>::default(),
                0,
                || panic!("boot is already allocated"),
            )
            .expect("pipeline command");
        let snapshot = actor
            .snapshot_with_boot("alice", || panic!("boot is already allocated"))
            .expect("actor snapshot");

        assert_eq!(first.process_ids, vec![1]);
        // PID 2 is the reaped pipeline supervisor; guest processes are 3 and 4.
        assert_eq!(pipeline.process_ids, vec![3, 4]);
        assert_eq!(pipeline.next_process_id, Some(5));
        assert_eq!(snapshot.commands_completed, 2);
        assert_eq!(snapshot.machine.process_records, 0);
        assert_eq!(snapshot.machine.pipe_objects, 0);
    }

    #[test]
    fn read_only_snapshot_does_not_allocate_a_machine_or_boot_identity() {
        let actor = ResidentRealm::default();
        let snapshot = actor.snapshot("unseen");

        assert_eq!(snapshot.state, "idle");
        assert_eq!(snapshot.boot_sequence, 0);
        assert_eq!(snapshot.commands_completed, 0);
        assert_eq!(snapshot.machine.next_process_id.map(|id| id.get()), Some(1));
        assert!(actor.realm.is_none());
    }

    #[test]
    fn boot_sequence_encoding_is_monotonic_and_fail_closed() {
        let (first, first_bytes) = increment_boot_sequence(None).expect("first boot");
        let (second, second_bytes) =
            increment_boot_sequence(Some(&first_bytes)).expect("second boot");

        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(
            serde_json::from_slice::<u64>(&second_bytes).expect("encoded boot sequence"),
            2
        );
        assert!(increment_boot_sequence(Some(b"not-json")).is_err());
        assert!(increment_boot_sequence(Some(b"18446744073709551615")).is_err());
    }

    #[test]
    fn linux_activity_is_principal_local() {
        let mut alice = PrincipalRealm::new(7);
        let bob = PrincipalRealm::new(11);
        alice.linux.boot_executions = 1;
        alice.linux.commands_completed = 2;
        alice.linux.clean_shutdowns = 1;
        alice.linux.guest_steps = 14_823_384;
        alice.linux.last_outcome = Some("stopped");
        alice.linux.last_exit_status = Some(0);

        let snapshot = alice.linux.snapshot();
        assert_eq!(snapshot.state, "cold");
        assert_eq!(snapshot.boot_executions, 1);
        assert_eq!(snapshot.commands_completed, 2);
        assert_eq!(snapshot.clean_shutdowns, 1);
        assert_eq!(snapshot.guest_steps, 14_823_384);
        assert_eq!(snapshot.last_outcome, Some("stopped"));
        assert_eq!(snapshot.last_exit_status, Some(0));
        assert_eq!(bob.linux.snapshot().boot_executions, 0);
        assert_eq!(bob.linux.snapshot().guest_steps, 0);
    }
}
