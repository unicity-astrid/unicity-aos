//! Tests for the Context Engine capsule.

use super::*;

fn make_msg(id: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "role": "user",
        "content": content,
    })
}

/// Helper to build an `ipc::PollResult` from a list of (topic, payload_value) pairs.
fn make_poll_result(messages: Vec<(&str, serde_json::Value)>) -> ipc::PollResult {
    ipc::PollResult {
        messages: messages
            .into_iter()
            .map(|(topic, payload)| ipc::Message {
                topic: topic.to_string(),
                payload: serde_json::to_string(&payload).unwrap(),
                source_id: "test".to_string(),
                principal: ipc::PrincipalAttribution::System,
            })
            .collect(),
        dropped: 0,
        lagged: 0,
    }
}

// ── Merge semantics unit tests ──────────────────────────────────────

#[test]
fn merge_empty_responses_returns_defaults() {
    let result = merge_before_compaction_responses(&[]);
    assert!(!result.skip);
    assert!(result.protected_ids.is_empty());
}

#[test]
fn merge_any_skip_true_wins() {
    let responses = vec![
        BeforeCompactionHookResponse {
            skip: Some(false),
            ..Default::default()
        },
        BeforeCompactionHookResponse {
            skip: Some(true),
            ..Default::default()
        },
        BeforeCompactionHookResponse {
            skip: Some(false),
            ..Default::default()
        },
    ];
    let result = merge_before_compaction_responses(&responses);
    assert!(result.skip);
}

#[test]
fn merge_no_skip_defaults_false() {
    let responses = vec![BeforeCompactionHookResponse {
        pinned_message_ids: vec!["msg-1".to_string()],
        ..Default::default()
    }];
    let result = merge_before_compaction_responses(&responses);
    assert!(!result.skip);
}

#[test]
fn merge_pinned_ids_union() {
    let responses = vec![
        BeforeCompactionHookResponse {
            pinned_message_ids: vec!["msg-1".to_string(), "msg-2".to_string()],
            ..Default::default()
        },
        BeforeCompactionHookResponse {
            pinned_message_ids: vec!["msg-2".to_string(), "msg-3".to_string()],
            ..Default::default()
        },
    ];
    let result = merge_before_compaction_responses(&responses);
    assert_eq!(result.protected_ids.len(), 3);
    assert!(result.protected_ids.contains("msg-1"));
    assert!(result.protected_ids.contains("msg-2"));
    assert!(result.protected_ids.contains("msg-3"));
}

#[test]
fn merge_skip_and_pinned_from_different_plugins() {
    let responses = vec![
        BeforeCompactionHookResponse {
            skip: Some(true),
            ..Default::default()
        },
        BeforeCompactionHookResponse {
            pinned_message_ids: vec!["msg-1".to_string()],
            ..Default::default()
        },
    ];
    let result = merge_before_compaction_responses(&responses);
    assert!(result.skip);
    assert!(result.protected_ids.contains("msg-1"));
}

// ── Token estimation tests ──────────────────────────────────────────

#[test]
fn token_estimation_non_zero() {
    let msg = serde_json::json!({"content": "hello world"});
    let tokens = strategy::estimate_tokens(&msg);
    assert!(tokens > 0);
}

#[test]
fn token_estimation_proportional_to_length() {
    let short = serde_json::json!({"content": "hi"});
    let long = serde_json::json!({"content": "a".repeat(1000)});
    assert!(strategy::estimate_tokens(&long) > strategy::estimate_tokens(&short));
}

#[test]
fn total_token_estimation_sums_messages() {
    let messages = vec![
        serde_json::json!({"content": "hello"}),
        serde_json::json!({"content": "world"}),
    ];
    let total = strategy::estimate_total_tokens(&messages);
    let individual: u64 = messages.iter().map(strategy::estimate_tokens).sum();
    assert_eq!(total, individual);
}

// ── Hook response deserialization tests ──────────────────────────────

#[test]
fn hook_response_deserializes_camel_case() {
    let json = r#"{
        "skip": true,
        "pinnedMessageIds": ["msg-1", "msg-2"],
        "customStrategy": "lossless"
    }"#;
    let resp: BeforeCompactionHookResponse = serde_json::from_str(json).expect("should parse");
    assert_eq!(resp.skip, Some(true));
    assert_eq!(resp.pinned_message_ids, vec!["msg-1", "msg-2"]);
    assert_eq!(resp.custom_strategy.as_deref(), Some("lossless"));
}

#[test]
fn hook_response_deserializes_snake_case_alias() {
    // OpenClaw plugins may use snake_case field names.
    let json = r#"{"protected_message_ids": ["msg-1"]}"#;
    let resp: BeforeCompactionHookResponse = serde_json::from_str(json).expect("should parse");
    assert_eq!(resp.pinned_message_ids, vec!["msg-1"]);
}

#[test]
fn hook_response_deserializes_empty() {
    let resp: BeforeCompactionHookResponse = serde_json::from_str("{}").expect("should parse");
    assert!(!resp.has_any_field());
    assert_eq!(resp.skip, None);
    assert!(resp.pinned_message_ids.is_empty());
}

#[test]
fn hook_response_has_any_field_detects_each() {
    assert!(
        BeforeCompactionHookResponse {
            skip: Some(false),
            ..Default::default()
        }
        .has_any_field()
    );

    assert!(
        BeforeCompactionHookResponse {
            pinned_message_ids: vec!["x".into()],
            ..Default::default()
        }
        .has_any_field()
    );

    assert!(
        BeforeCompactionHookResponse {
            custom_strategy: Some("x".into()),
            ..Default::default()
        }
        .has_any_field()
    );

    assert!(!BeforeCompactionHookResponse::default().has_any_field());
}

// ── Payload serialization round-trip tests ──────────────────────────

#[test]
fn compact_request_round_trips() {
    let json = r#"{
        "session_id": "abc-123",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 100000,
        "target_tokens": 50000
    }"#;
    let req: CompactRequest = serde_json::from_str(json).expect("should parse");
    assert_eq!(req.session_id, "abc-123");
    assert_eq!(req.max_tokens, 100_000);
    assert_eq!(req.target_tokens, 50_000);
    assert_eq!(req.messages.len(), 1);
}

#[test]
fn compact_response_serializes() {
    let resp = CompactResponse {
        messages: vec![make_msg("msg-0", "hello")],
        compacted: true,
        messages_removed: 5,
        strategy: "summarize_and_truncate".to_string(),
    };
    let json = serde_json::to_value(&resp).expect("should serialize");
    assert_eq!(json["compacted"], true);
    assert_eq!(json["messages_removed"], 5);
    assert_eq!(json["strategy"], "summarize_and_truncate");
    assert_eq!(json["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn estimate_request_round_trips() {
    let json = r#"{"messages": [{"content": "test"}]}"#;
    let req: EstimateRequest = serde_json::from_str(json).expect("should parse");
    assert_eq!(req.messages.len(), 1);
}

#[test]
fn estimate_response_serializes() {
    let resp = EstimateResponse {
        estimated_tokens: 42,
    };
    let json = serde_json::to_value(&resp).expect("should serialize");
    assert_eq!(json["estimated_tokens"], 42);
}

#[test]
fn before_compaction_payload_includes_response_topic() {
    let payload = BeforeCompactionPayload {
        session_id: "sess-1".to_string(),
        messages: vec![],
        message_count: 0,
        estimated_tokens: 0,
        max_tokens: 100_000,
        response_topic: "context_engine.v1.hook_response.compact-123-0".to_string(),
    };
    let json = serde_json::to_value(&payload).expect("serialize");
    assert_eq!(
        json["response_topic"],
        "context_engine.v1.hook_response.compact-123-0"
    );
}

#[test]
fn after_compaction_payload_serializes() {
    let payload = AfterCompactionPayload {
        session_id: "sess-1".to_string(),
        messages_before: 42,
        messages_after: 20,
        tokens_before: 95_000,
        tokens_after: 45_000,
        strategy_used: "summarize_and_truncate".to_string(),
    };
    let json = serde_json::to_value(&payload).expect("serialize");
    assert_eq!(json["session_id"], "sess-1");
    assert_eq!(json["messages_before"], 42);
    assert_eq!(json["messages_after"], 20);
    assert_eq!(json["tokens_before"], 95_000);
    assert_eq!(json["tokens_after"], 45_000);
    assert_eq!(json["strategy_used"], "summarize_and_truncate");
}

// ── parse_hook_responses tests ──────────────────────────────────────

#[test]
fn parse_hook_responses_from_poll_result() {
    let result = make_poll_result(vec![
        (
            "context_engine.v1.hook_response.compact-1",
            serde_json::json!({"pinnedMessageIds": ["msg-1", "msg-2"]}),
        ),
        (
            "context_engine.v1.hook_response.compact-1",
            serde_json::json!({"skip": true}),
        ),
    ]);
    let responses = parse_hook_responses(&result);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0].pinned_message_ids, vec!["msg-1", "msg-2"]);
    assert_eq!(responses[1].skip, Some(true));
}

#[test]
fn parse_hook_responses_nested_in_custom_data() {
    let result = make_poll_result(vec![(
        "hook",
        serde_json::json!({
            "data": {
                "skip": true,
                "pinnedMessageIds": ["msg-99"]
            }
        }),
    )]);
    let responses = parse_hook_responses(&result);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].skip, Some(true));
    assert_eq!(responses[0].pinned_message_ids, vec!["msg-99"]);
}

#[test]
fn parse_hook_responses_empty_poll_result() {
    let result = ipc::PollResult {
        messages: vec![],
        dropped: 0,
        lagged: 0,
    };
    assert!(parse_hook_responses(&result).is_empty());
}

#[test]
fn parse_hook_responses_ignores_unrelated_payload() {
    let result = make_poll_result(vec![(
        "hook",
        serde_json::json!({"status": "ok", "unrelated": 42}),
    )]);
    assert!(parse_hook_responses(&result).is_empty());
}

#[test]
fn parse_hook_responses_mixed_valid_and_invalid() {
    let result = make_poll_result(vec![
        ("hook", serde_json::json!({"status": "irrelevant"})),
        ("hook", serde_json::json!({"pinnedMessageIds": ["msg-1"]})),
    ]);
    let responses = parse_hook_responses(&result);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].pinned_message_ids, vec!["msg-1"]);
}

// ── target_tokens clamping test ─────────────────────────────────────

#[test]
fn target_tokens_clamped_to_max_tokens() {
    // Verify that the clamp logic (target_tokens.min(max_tokens)) works.
    let max_tokens: u64 = 100_000;
    let target_tokens: u64 = 200_000;
    let clamped = target_tokens.min(max_tokens);
    assert_eq!(
        clamped, 100_000,
        "target_tokens should be clamped to max_tokens"
    );

    // When target <= max, no clamping occurs.
    let target_tokens: u64 = 50_000;
    let clamped = target_tokens.min(max_tokens);
    assert_eq!(
        clamped, 50_000,
        "target_tokens within range should be unchanged"
    );

    // Verify the strategy respects the clamped value by running compaction
    // with a low max_tokens -- messages should be removed.
    let messages: Vec<serde_json::Value> = (0..20)
        .map(|i| {
            make_msg(
                &format!("msg-{i}"),
                &format!("Message content number {i} with padding"),
            )
        })
        .collect();

    let over_budget_target: u64 = 200_000;
    let real_max: u64 = 10;
    let effective_target = over_budget_target.min(real_max);

    let result = strategy::summarize_and_truncate(
        &messages,
        effective_target,
        &std::collections::HashSet::new(),
        5,
    );
    assert!(
        result.len() < messages.len(),
        "compaction should use clamped target, not the original over-budget one"
    );
}

// ── Topic filtering tests ───────────────────────────────────────────

#[test]
fn should_dispatch_compact_topic() {
    assert!(should_dispatch_topic("context_engine.v1.compact"));
}

#[test]
fn should_dispatch_estimate_topic() {
    assert!(should_dispatch_topic("context_engine.v1.estimate_tokens"));
}

#[test]
fn should_not_dispatch_own_response_topics() {
    assert!(!should_dispatch_topic("context_engine.v1.response.compact"));
    assert!(!should_dispatch_topic(
        "context_engine.v1.response.estimate_tokens"
    ));
}

#[test]
fn should_not_dispatch_hook_response_topics() {
    assert!(!should_dispatch_topic(
        "context_engine.v1.hook_response.compact-123"
    ));
}

#[test]
fn should_not_dispatch_interceptor_topics() {
    assert!(!should_dispatch_topic(
        "context_engine.v1.hook.before_compaction"
    ));
    assert!(!should_dispatch_topic(
        "context_engine.v1.hook.after_compaction"
    ));
}

// ── hook_recv_timeout (before-compaction fan-out window) ────────────────

#[test]
fn hook_timeout_zero_responders_uses_first_window_then_breaks() {
    // No response yet, fresh loop: wait the first-response window (250).
    assert_eq!(hook_recv_timeout(false, 0, 2000, 250, 100), Some(250));
    // Partway through the window: only the remaining first-response budget,
    // measured from the loop start (NOT reset to 250).
    assert_eq!(hook_recv_timeout(false, 200, 1800, 250, 100), Some(50));
    // Window spent with still no response → break (the no-responder backstop),
    // even though the outer hook_timeout_ms budget remains.
    assert_eq!(hook_recv_timeout(false, 250, 1750, 250, 100), None);
    assert_eq!(hook_recv_timeout(false, 400, 1600, 250, 100), None);
}

#[test]
fn hook_timeout_after_first_response_uses_idle_grace() {
    // Once a response has arrived, switch to the short idle-grace window.
    assert_eq!(hook_recv_timeout(true, 300, 1700, 250, 100), Some(100));
    assert_eq!(hook_recv_timeout(true, 1950, 50, 250, 100), Some(50)); // clamped to remaining
}

#[test]
fn hook_timeout_clamped_to_remaining_and_outer_cap() {
    // First-response window clamps to the remaining outer budget.
    assert_eq!(hook_recv_timeout(false, 0, 40, 250, 100), Some(40));
    // Outer budget exhausted → break regardless of state.
    assert_eq!(hook_recv_timeout(false, 0, 0, 250, 100), None);
    assert_eq!(hook_recv_timeout(true, 0, 0, 250, 100), None);
}
