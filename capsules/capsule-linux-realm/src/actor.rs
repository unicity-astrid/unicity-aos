//! Long-lived Astrid run-loop that owns one isolated Realm machine per principal.

use super::*;
use std::collections::BTreeMap;

const EXEC_TOPIC: &str = "tool.v1.execute.linux_realm_exec";
const STATUS_TOPIC: &str = "tool.v1.execute.linux_realm_status";
const DESCRIBE_TOPIC: &str = "tool.v1.request.describe";
const BOOT_SEQUENCE_KEY: &str = "realm/default/actor/boot-sequence";
const BOOT_SEQUENCE_CAS_ATTEMPTS: usize = 8;
const MAX_PRINCIPAL_REALMS: usize = 32;
const ACTOR_POLL_MS: u64 = 250;

#[derive(Debug, Deserialize)]
struct ToolRequest {
    call_id: String,
    tool_name: String,
    arguments: serde_json::Value,
}

struct PrincipalRealm {
    boot_sequence: u64,
    commands_completed: u64,
    machine: RealmMachine,
}

impl PrincipalRealm {
    fn new(boot_sequence: u64) -> Self {
        Self {
            boot_sequence,
            commands_completed: 0,
            machine: RealmMachine::default(),
        }
    }

    fn snapshot(&self) -> ActorSnapshot {
        ActorSnapshot {
            state: "running",
            boot_sequence: self.boot_sequence,
            commands_completed: self.commands_completed,
            machine: self.machine.status(),
        }
    }
}

#[derive(Default)]
struct RealmActor {
    principals: BTreeMap<String, PrincipalRealm>,
}

impl RealmActor {
    fn ensure_capacity(&self, principal: &str) -> Result<(), SysError> {
        if !self.principals.contains_key(principal) && self.principals.len() >= MAX_PRINCIPAL_REALMS
        {
            return Err(SysError::ApiError(format!(
                "Realm actor principal quota exceeded ({MAX_PRINCIPAL_REALMS})"
            )));
        }
        Ok(())
    }

    fn realm_with_boot(
        &mut self,
        principal: &str,
        load_boot: impl FnOnce() -> Result<u64, SysError>,
    ) -> Result<&mut PrincipalRealm, SysError> {
        self.ensure_capacity(principal)?;
        if !self.principals.contains_key(principal) {
            let boot_sequence = load_boot()?;
            self.principals
                .insert(principal.to_string(), PrincipalRealm::new(boot_sequence));
        }
        self.principals
            .get_mut(principal)
            .ok_or_else(|| SysError::ApiError("Realm actor state disappeared".to_string()))
    }

    fn execute_with_boot(
        &mut self,
        principal: &str,
        args: ExecArgs,
        realm_host: Box<dyn RealmHost>,
        load_boot: impl FnOnce() -> Result<u64, SysError>,
    ) -> Result<ExecResponse, SysError> {
        let realm = self.realm_with_boot(principal, load_boot)?;
        let response = run_command_in_machine(
            args,
            principal.to_string(),
            realm_host,
            &mut realm.machine,
            realm.boot_sequence,
        )?;
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
        self.principals
            .get(principal)
            .map(PrincipalRealm::snapshot)
            .unwrap_or_else(ActorSnapshot::idle)
    }

    fn execute(&mut self, principal: &str, args: ExecArgs) -> Result<String, SysError> {
        // Reject aggregate admission before creating durable state for a realm
        // this actor cannot retain.
        self.ensure_capacity(principal)?;
        ensure_layout()?;
        validate_cwd(args.cwd.as_deref().unwrap_or(DEFAULT_CWD))?;
        let response = self.execute_with_boot(
            principal,
            args,
            Box::<AstridRealmHost>::default(),
            next_boot_sequence,
        )?;
        serde_json::to_string(&response).map_err(|error| SysError::ApiError(error.to_string()))
    }

    fn status(&mut self, principal: &str, _args: StatusArgs) -> Result<String, SysError> {
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

pub(crate) fn run_actor_loop() -> Result<(), SysError> {
    let exec = ipc::subscribe(EXEC_TOPIC)?;
    let status = ipc::subscribe(STATUS_TOPIC)?;
    let describe = ipc::subscribe(DESCRIBE_TOPIC)?;
    let _ = runtime::signal_ready();
    log::info("AOS Realm actor ready");

    let mut actor = RealmActor::default();
    loop {
        let result = exec.recv(ACTOR_POLL_MS)?;
        dispatch_tool_messages(&mut actor, &result, "linux_realm_exec")?;
        dispatch_tool_messages(&mut actor, &status.poll()?, "linux_realm_status")?;
        dispatch_describe_messages(&describe.poll()?)?;
    }
}

fn dispatch_tool_messages(
    actor: &mut RealmActor,
    result: &ipc::PollResult,
    expected_tool: &str,
) -> Result<(), SysError> {
    log_lag(result, expected_tool);
    for message in &result.messages {
        let request: ToolRequest = match serde_json::from_str(&message.payload) {
            Ok(request) => request,
            Err(error) => {
                log::warn(format!(
                    "AOS Realm rejected malformed {expected_tool} request: {error}"
                ));
                continue;
            }
        };
        let call_id = request.call_id.clone();
        let execution = handle_tool_message(actor, message, expected_tool, request);
        publish_tool_result(expected_tool, &call_id, execution)?;
    }
    Ok(())
}

fn handle_tool_message(
    actor: &mut RealmActor,
    message: &ipc::Message,
    expected_tool: &str,
    request: ToolRequest,
) -> Result<String, String> {
    if request.tool_name != expected_tool {
        return Err(format!(
            "tool payload named `{}` arrived on `{expected_tool}`",
            request.tool_name
        ));
    }
    let principal = message
        .principal
        .verified()
        .ok_or_else(|| "AOS Realm requires a kernel-verified principal".to_string())?;
    match expected_tool {
        "linux_realm_exec" => {
            let args = serde_json::from_value::<ExecArgs>(request.arguments)
                .map_err(|error| format!("failed to parse tool arguments: {error}"))?;
            actor
                .execute(principal, args)
                .map_err(|error| error.to_string())
        }
        "linux_realm_status" => {
            let args = serde_json::from_value::<StatusArgs>(request.arguments)
                .map_err(|error| format!("failed to parse tool arguments: {error}"))?;
            actor
                .status(principal, args)
                .map_err(|error| error.to_string())
        }
        _ => Err(format!("unsupported Realm actor tool `{expected_tool}`")),
    }
}

fn publish_tool_result(
    tool_name: &str,
    call_id: &str,
    result: Result<String, String>,
) -> Result<(), SysError> {
    let (content, is_error) = match result {
        // Preserve the SDK macro's existing tool contract: a Rust String result
        // is JSON-serialized before it is placed in ToolCallResult.content.
        Ok(value) => (
            serde_json::to_string(&value).map_err(|error| SysError::ApiError(error.to_string()))?,
            false,
        ),
        Err(error) => (error, true),
    };
    ipc::publish_json(
        &format!("tool.v1.execute.{tool_name}.result"),
        &serde_json::json!({
            "type": "tool_execute_result",
            "call_id": call_id,
            "result": {
                "call_id": call_id,
                "content": content,
                "is_error": is_error,
            }
        }),
    )
}

fn dispatch_describe_messages(result: &ipc::PollResult) -> Result<(), SysError> {
    log_lag(result, "tool_describe");
    for _message in &result.messages {
        ipc::publish_json("tool.v1.response.describe.self", &tool_description())?;
    }
    Ok(())
}

fn tool_description() -> serde_json::Value {
    let mut exec_schema = schemars::schema_for!(ExecArgs);
    exec_schema
        .schema
        .extensions
        .insert("mutable".to_string(), serde_json::Value::Bool(true));
    let mut status_schema = schemars::schema_for!(StatusArgs);
    status_schema
        .schema
        .extensions
        .insert("mutable".to_string(), serde_json::Value::Bool(false));
    let mut exec_schema = serde_json::to_value(exec_schema)
        .unwrap_or_else(|_| serde_json::json!({"type": "object", "properties": {}}));
    let mut status_schema = serde_json::to_value(status_schema)
        .unwrap_or_else(|_| serde_json::json!({"type": "object", "properties": {}}));
    ensure_properties(&mut exec_schema);
    ensure_properties(&mut status_schema);
    serde_json::json!({
        "tools": [
            {
                "name": "linux_realm_exec",
                "description": "Run one signed command in the caller's principal-scoped AOS Realm.",
                "input_schema": exec_schema,
            },
            {
                "name": "linux_realm_status",
                "description": "Inspect the initialized Realm and its live actor accounting.",
                "input_schema": status_schema,
            }
        ],
        "description": "Principal-scoped AOS Realm workbench",
    })
}

fn ensure_properties(schema: &mut serde_json::Value) {
    if let Some(object) = schema.as_object_mut() {
        object
            .entry("properties")
            .or_insert_with(|| serde_json::json!({}));
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

fn log_lag(result: &ipc::PollResult, operation: &str) {
    if result.dropped > 0 || result.lagged > 0 {
        log::warn(format!(
            "AOS Realm actor {operation} lagged: dropped={}, lagged={}",
            result.dropped, result.lagged
        ));
    }
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
    fn one_actor_keeps_monotonic_pids_but_isolates_principal_machines() {
        let mut actor = RealmActor::default();
        let alice_first = actor
            .execute_with_boot("alice", echo("one"), Box::<TestHost>::default(), || Ok(7))
            .expect("alice first command");
        let alice_second = actor
            .execute_with_boot("alice", echo("two"), Box::<TestHost>::default(), || {
                panic!("existing principal must not allocate another boot")
            })
            .expect("alice second command");
        let bob_first = actor
            .execute_with_boot("bob", echo("other"), Box::<TestHost>::default(), || Ok(11))
            .expect("bob first command");

        assert_eq!(alice_first.realm_boot_sequence, 7);
        assert_eq!(alice_first.process_ids, vec![1]);
        assert_eq!(alice_second.process_ids, vec![2]);
        assert_eq!(alice_second.next_process_id, Some(3));
        assert_eq!(bob_first.realm_boot_sequence, 11);
        assert_eq!(bob_first.process_ids, vec![1]);
        assert_eq!(bob_first.next_process_id, Some(2));
    }

    #[test]
    fn pipeline_ids_share_the_principals_monotonic_boot_namespace() {
        let mut actor = RealmActor::default();
        let first = actor
            .execute_with_boot("alice", echo("seed"), Box::<TestHost>::default(), || Ok(3))
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
        let actor = RealmActor::default();
        let snapshot = actor.snapshot("unseen");

        assert_eq!(snapshot.state, "idle");
        assert_eq!(snapshot.boot_sequence, 0);
        assert_eq!(snapshot.commands_completed, 0);
        assert_eq!(snapshot.machine.next_process_id.map(|id| id.get()), Some(1));
        assert!(actor.principals.is_empty());
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
    fn manual_describe_contract_matches_the_run_loop_tools() {
        let description = tool_description();
        let tools = description["tools"].as_array().expect("tools array");

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "linux_realm_exec");
        assert_eq!(tools[0]["input_schema"]["mutable"], true);
        assert_eq!(tools[1]["name"], "linux_realm_status");
        assert_eq!(tools[1]["input_schema"]["mutable"], false);
    }

    #[test]
    fn actor_has_an_explicit_aggregate_principal_quota() {
        let mut actor = RealmActor::default();
        for index in 0..MAX_PRINCIPAL_REALMS {
            actor
                .snapshot_with_boot(&format!("principal-{index}"), || Ok(1))
                .expect("principal admitted within quota");
        }

        let error = actor
            .snapshot_with_boot("one-too-many", || Ok(1))
            .expect_err("aggregate principal quota is enforced");
        assert!(error.to_string().contains("principal quota exceeded"));
    }
}
