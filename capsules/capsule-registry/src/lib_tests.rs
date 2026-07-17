use super::*;

/// The clear path must broadcast a distinguishable "no active model"
/// payload on `registry.v1.active_model_changed` so warm-cache
/// subscribers (react) drop the stale binding instead of routing to it.
/// The publish itself is IO-buried in `ipc::publish_json`, so we assert
/// the extracted payload shape: JSON `null`, never a `ProviderEntry`
/// object. A subscriber can therefore tell a clear from a change.
#[test]
fn cleared_payload_is_null() {
    let payload = cleared_payload();
    assert!(payload.is_null(), "clear must broadcast JSON null");

    // And it is genuinely distinguishable from a real model-changed
    // payload, which serializes to a JSON object.
    let entry = ProviderEntry {
        id: "openai-compat:gpt-5.4".to_string(),
        description: "test".to_string(),
        request_topic: "llm.v1.request.generate.openai-compat".to_string(),
        stream_topic: "llm.v1.stream.openai-compat".to_string(),
        capabilities: vec!["text".to_string()],
        context_window: Some(128_000),
        max_output_tokens: Some(8_192),
    };
    let changed = serde_json::to_value(&entry).expect("serialize entry");
    assert!(changed.is_object(), "a real change serializes to an object");
    assert_ne!(payload, changed);
}

/// `req_id` is interpolated into the result topic; only the exact UUID shape
/// the CLI actually sends (lowercase hex, simple or hyphenated) may pass.
/// Anything carrying a topic separator (`.`), a wildcard (`*`), whitespace,
/// uppercase, non-hex letters, or that is empty/oversized must be rejected so
/// we never publish to a derived, unexpected topic.
#[test]
fn req_id_validation_accepts_cli_shape_rejects_topic_injection() {
    // The exact shape the CLI emits: `Uuid::new_v4().simple()` (32 hex).
    assert!(is_valid_req_id("0123456789abcdef0123456789abcdef"));
    // Hyphenated UUID form is tolerated.
    assert!(is_valid_req_id("01234567-89ab-cdef-0123-456789abcdef"));

    // Topic separator would split into extra segments and miss the CLI's
    // `cli.v1.command.result.*` single-wildcard subscription.
    assert!(!is_valid_req_id("abc.def"));
    // Wildcard chars must never reach a published topic.
    assert!(!is_valid_req_id("abc*"));
    assert!(!is_valid_req_id("*"));
    // Whitespace and empty are not the CLI shape.
    assert!(!is_valid_req_id("abc def"));
    assert!(!is_valid_req_id(""));
    // Oversized values are bounded out.
    assert!(!is_valid_req_id(&"a".repeat(MAX_REQ_ID_LEN + 1)));
    // Uppercase hex is NOT the CLI shape (`Uuid::simple()` is lowercase).
    assert!(!is_valid_req_id("0123456789ABCDEF0123456789ABCDEF"));
    assert!(!is_valid_req_id("01234567-89AB-CDEF-0123-456789ABCDEF"));
    // Non-hex ASCII alphanumerics must be rejected (formerly accepted).
    assert!(!is_valid_req_id("zzzzzzzz"));
    assert!(!is_valid_req_id("0123456789abcdefg"));
}

#[test]
fn cli_run_topic_uses_provider_capsule_package_id() {
    // The CLI publishes to cli.v1.command.run.<provider_capsule>, where
    // provider_capsule comes from GetCommands and is the package id.
    assert_eq!(CLI_RUN_TOPIC, "cli.v1.command.run.aos-registry");

    // Ensure Capsule.toml subscription matches CLI_RUN_TOPIC to prevent drift.
    let capsule_toml = include_str!("../Capsule.toml");
    assert!(
        capsule_toml.contains(&format!("\"{}\"", CLI_RUN_TOPIC)),
        "Capsule.toml is missing subscription for {}",
        CLI_RUN_TOPIC
    );
}

/// The CLI run topic is per-capsule, not
/// per-verb. Only a payload whose `command` is `"models"` may be dispatched
/// as a models subcommand; any other command (or a missing/non-string
/// field) must be rejected so unrelated verbs aren't misinterpreted as
/// `models` args.
#[test]
fn cli_run_accepts_only_models_command() {
    assert!(is_models_command(
        &serde_json::json!({"command": "models", "args": ["list"]})
    ));
    // A different verb routed to this per-capsule topic is rejected.
    assert!(!is_models_command(
        &serde_json::json!({"command": "providers", "args": ["list"]})
    ));
    // Missing `command` is rejected (don't treat args as a models subcommand).
    assert!(!is_models_command(&serde_json::json!({"args": ["list"]})));
    // A non-string `command` is rejected.
    assert!(!is_models_command(&serde_json::json!({"command": 42})));
}

fn test_entry() -> ProviderEntry {
    ProviderEntry {
        id: "openai-compat:gpt-5.4".to_string(),
        description: "test".to_string(),
        request_topic: "llm.v1.request.generate.openai-compat".to_string(),
        stream_topic: "llm.v1.stream.openai-compat".to_string(),
        capabilities: vec!["text".to_string()],
        context_window: Some(128_000),
        max_output_tokens: Some(8_192),
    }
}

#[test]
fn dedupe_providers_by_id_keeps_first_entry_order() {
    let first = test_entry();
    let mut duplicate = test_entry();
    duplicate.description = "duplicate should be dropped".to_string();
    let mut second = test_entry();
    second.id = "openai-compat:gpt-4.1".to_string();

    let providers = dedupe_providers_by_id(vec![first.clone(), duplicate, second.clone()]);

    assert_eq!(providers, vec![first, second]);
}

#[test]
fn stamped_provider_discovery_duplicates_are_deduped() {
    let payload = serde_json::json!({
        "providers": [
            {
                "id": "fake-slow",
                "description": "slow",
                "request_topic": "llm.v1.request.generate.openai-compat",
                "stream_topic": "llm.v1.stream.openai-compat",
                "capabilities": ["text"],
                "context_window": 128000,
                "max_output_tokens": 8192
            },
            {
                "id": "duplicate-name",
                "description": "duplicate",
                "request_topic": "llm.v1.request.generate.openai-compat",
                "stream_topic": "llm.v1.stream.openai-compat",
                "capabilities": ["text"],
                "context_window": 128000,
                "max_output_tokens": 8192
            }
        ]
    })
    .to_string();

    let mut providers = Vec::new();
    providers.extend(stamp_message_providers(&payload, "provider-a"));
    providers.extend(stamp_message_providers(&payload, "provider-a"));

    let providers = dedupe_providers_by_id(providers);
    let ids = providers.iter().map(|p| p.id.as_str()).collect::<Vec<_>>();

    assert_eq!(
        ids,
        vec!["openai-compat:fake-slow", "openai-compat:duplicate-name"]
    );
}

/// A `set_active_model` request carrying a `corr_id` must echo that exact
/// value into both the success and the error reply objects, so the gateway
/// can match its own reply on the routed stream and skip a racing
/// concurrent same-principal reply. The value is reflected verbatim — no
/// normalization, no regeneration.
#[test]
fn set_active_model_response_echoes_corr_id_when_present() {
    let entry = test_entry();
    let corr = "11111111-2222-3333-4444-555555555555";

    let ok = set_active_model_ok_response(&entry, Some(corr));
    assert_eq!(ok["status"], "ok");
    assert_eq!(ok["corr_id"], corr, "ok reply must echo corr_id verbatim");
    assert_eq!(
        ok["active_model"],
        serde_json::to_value(&entry).unwrap(),
        "ok reply must still carry the resolved active_model"
    );

    let err = set_active_model_error_response("unknown model", Some(corr));
    assert_eq!(err["error"], "unknown model");
    assert_eq!(
        err["corr_id"], corr,
        "error reply must echo corr_id verbatim too"
    );
}

/// Back-compat: a request without a `corr_id` (existing TUI/react/CLI
/// callers) must produce a reply with NO `corr_id` key at all — not a
/// `null`, not an empty string — so the on-wire shape those callers parse
/// is unchanged.
#[test]
fn set_active_model_response_omits_corr_id_when_absent() {
    let entry = test_entry();

    let ok = set_active_model_ok_response(&entry, None);
    assert!(
        ok.get("corr_id").is_none(),
        "ok reply must omit corr_id when the request carried none"
    );
    assert_eq!(ok["status"], "ok");

    let err = set_active_model_error_response("unknown model", None);
    assert!(
        err.get("corr_id").is_none(),
        "error reply must omit corr_id when the request carried none"
    );
    assert_eq!(err["error"], "unknown model");
}

/// `corr_id` is read with the same nested-then-top-level shape as
/// `model_id`, and is absent (`None`) when the request omits it — driving
/// the omit-on-absent reply path above.
#[test]
fn extract_request_field_mirrors_model_id_shape() {
    // Nested under `data`.
    let nested = serde_json::json!({"data": {"corr_id": "abc"}});
    assert_eq!(
        extract_request_field(&nested, "corr_id").as_deref(),
        Some("abc")
    );
    // Top level.
    let top = serde_json::json!({"corr_id": "def"});
    assert_eq!(
        extract_request_field(&top, "corr_id").as_deref(),
        Some("def")
    );
    // Absent → None (back-compat callers).
    let bare = serde_json::json!({"model_id": "openai-compat:gpt-5.4"});
    assert_eq!(extract_request_field(&bare, "corr_id"), None);
}
