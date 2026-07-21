use super::*;

#[test]
fn readiness_capacity_is_derived_from_poll_contract() {
    assert_eq!(max_polled_clients(16), 239);
    assert_eq!(max_polled_clients(255), 0);
    assert_eq!(max_polled_clients(300), 0);
}

// --- principal format validation ---

#[test]
fn valid_principals_accepted() {
    assert!(is_valid_principal("default"));
    assert!(is_valid_principal("alice"));
    assert!(is_valid_principal("user_01-A"));
    assert!(is_valid_principal("x")); // 1 char
    assert!(is_valid_principal(&"a".repeat(64))); // boundary: 64 chars
}

#[test]
fn invalid_principals_rejected() {
    assert!(!is_valid_principal("")); // empty
    assert!(!is_valid_principal(&"a".repeat(65))); // too long
    assert!(!is_valid_principal("has space"));
    assert!(!is_valid_principal("dot.sep"));
    assert!(!is_valid_principal("slash/x"));
    assert!(!is_valid_principal("emoji\u{1f600}"));
}

// --- ingress binding state machine ---

#[test]
fn first_message_with_valid_principal_binds() {
    assert_eq!(
        decide_ingress(None, Some("alice")),
        IngressDecision::Bind("alice".to_string())
    );
}

#[test]
fn first_message_without_principal_binds_default() {
    assert_eq!(
        decide_ingress(None, None),
        IngressDecision::Bind(DEFAULT_PRINCIPAL.to_string())
    );
}

#[test]
fn first_message_with_invalid_principal_drops_and_stays_unbound() {
    assert_eq!(
        decide_ingress(None, Some("bad principal")),
        IngressDecision::Drop {
            reason: DropReason::InvalidPrincipal("bad principal".to_string())
        }
    );
}

#[test]
fn bound_connection_without_principal_forwards_as_bound() {
    // Auto-attribution: un-stamped traffic rides the bound principal.
    assert_eq!(
        decide_ingress(Some("alice"), None),
        IngressDecision::ForwardAs("alice".to_string())
    );
}

#[test]
fn bound_connection_matching_principal_forwards() {
    assert_eq!(
        decide_ingress(Some("alice"), Some("alice")),
        IngressDecision::ForwardAs("alice".to_string())
    );
}

#[test]
fn bound_connection_conflicting_principal_drops_without_rebind() {
    // Conflict drops the message and yields no Bind/ForwardAs, so the
    // caller never mutates the binding.
    assert_eq!(
        decide_ingress(Some("alice"), Some("mallory")),
        IngressDecision::Drop {
            reason: DropReason::PrincipalConflict {
                bound: "alice".to_string(),
                claimed: "mallory".to_string(),
            }
        }
    );
}

#[test]
fn post_conflict_connection_still_forwards_as_original() {
    // After a conflict (binding unchanged), a subsequent matching/empty
    // message still forwards under the original principal.
    let binding = Some("alice");
    let _conflict = decide_ingress(binding, Some("mallory"));
    assert_eq!(
        decide_ingress(binding, None),
        IngressDecision::ForwardAs("alice".to_string())
    );
    assert_eq!(
        decide_ingress(binding, Some("alice")),
        IngressDecision::ForwardAs("alice".to_string())
    );
}

// --- outbound demux decision (principal axis) ---

#[test]
fn principaled_message_delivers_only_to_matching_bound_client() {
    assert!(should_deliver(Some("alice"), None, Some("alice"), None));
    assert!(!should_deliver(Some("alice"), None, Some("bob"), None));
}

#[test]
fn principaled_message_not_delivered_to_unbound_client() {
    assert!(!should_deliver(Some("alice"), None, None, None));
}

#[test]
fn unprincipaled_message_delivers_to_everyone() {
    assert!(should_deliver(None, None, Some("alice"), None));
    assert!(should_deliver(None, None, None, None)); // even an unbound client
}

// --- outbound demux decision (session axis: multi-session cross-talk) ---

#[test]
fn session_scoped_message_delivers_only_to_matching_session() {
    assert!(should_deliver(
        Some("default"),
        Some("S1"),
        Some("default"),
        Some("S1")
    ));
}

#[test]
fn session_scoped_message_not_delivered_across_sessions_of_same_principal() {
    // THE cross-talk fix: same principal, different session -> dropped.
    assert!(!should_deliver(
        Some("default"),
        Some("S1"),
        Some("default"),
        Some("S2")
    ));
}

#[test]
fn session_scoped_message_not_delivered_to_sessionless_client() {
    // A connection that has not started a chat session receives no
    // session-scoped traffic.
    assert!(!should_deliver(
        Some("default"),
        Some("S1"),
        Some("default"),
        None
    ));
}

#[test]
fn non_session_message_keeps_principal_only_routing() {
    // Correlated/system responses (no session_id) reach every same-principal
    // connection regardless of its session; a different principal is still
    // excluded.
    assert!(should_deliver(
        Some("default"),
        None,
        Some("default"),
        Some("S1")
    ));
    assert!(should_deliver(Some("default"), None, Some("default"), None));
    assert!(!should_deliver(
        Some("default"),
        None,
        Some("alice"),
        Some("S1")
    ));
}

// --- payload session extraction ---

#[test]
fn payload_session_id_extracts_top_level_string() {
    let v = serde_json::json!({
        "type": "agent_response",
        "text": "hi",
        "is_final": true,
        "session_id": "S1"
    });
    assert_eq!(payload_session_id(&v), Some("S1"));
}

#[test]
fn payload_session_id_none_when_absent_or_not_a_string() {
    assert_eq!(payload_session_id(&serde_json::json!({"text": "hi"})), None);
    assert_eq!(
        payload_session_id(&serde_json::json!({"session_id": 5})),
        None
    );
    assert_eq!(
        payload_session_id(&serde_json::json!("not-an-object")),
        None
    );
}

// --- chat-topic session scoping (review: bootstrap + spoof safety) ---

#[test]
fn ingress_binds_session_only_from_chat_prompt() {
    let with_sid = serde_json::json!({"type": "user_input", "session_id": "S1"});
    assert_eq!(
        ingress_session_bind(CHAT_REQUEST_TOPIC, &with_sid),
        Some("S1")
    );
    // A non-prompt topic never retargets the connection's session, even when
    // its payload carries a session_id (same-principal spoof guard).
    assert_eq!(
        ingress_session_bind("session.v1.request.create", &with_sid),
        None
    );
    assert_eq!(
        ingress_session_bind("astrid.v1.admin.agent.list", &with_sid),
        None
    );
    // A prompt with no session_id leaves the binding unchanged.
    assert_eq!(
        ingress_session_bind(
            CHAT_REQUEST_TOPIC,
            &serde_json::json!({"type": "user_input"})
        ),
        None
    );
}

#[test]
fn outbound_scopes_session_only_for_chat_response() {
    let with_sid = serde_json::json!({"type": "agent_response", "session_id": "S1"});
    assert_eq!(
        outbound_session_scope(CHAT_RESPONSE_TOPIC, &with_sid),
        Some("S1")
    );
    // A correlated reply that happens to carry a session_id is NOT
    // session-gated.
    assert_eq!(
        outbound_session_scope("session.v1.response.create.abc", &with_sid),
        None
    );
    assert_eq!(
        outbound_session_scope("registry.v1.response.x", &with_sid),
        None
    );
    // A chat response without a session_id routes by principal alone.
    assert_eq!(
        outbound_session_scope(
            CHAT_RESPONSE_TOPIC,
            &serde_json::json!({"type": "agent_response"})
        ),
        None
    );
}

#[test]
fn outbound_scopes_session_for_streamed_deltas() {
    // A streamed token must route only to the connection on its session,
    // exactly like the terminal response — otherwise a same-principal
    // connection on a different session would see another session's tokens.
    let delta =
        serde_json::json!({"type": "agent_response", "session_id": "S1", "is_final": false});
    assert_eq!(outbound_session_scope(CHAT_DELTA_TOPIC, &delta), Some("S1"));
    // No session_id on a delta falls back to principal routing.
    assert_eq!(
        outbound_session_scope(
            CHAT_DELTA_TOPIC,
            &serde_json::json!({"type": "agent_response"})
        ),
        None
    );
}

// --- streamed-reply reconciliation (TUI renders the reply exactly once) ---

fn delta(session: &str, text: &str) -> serde_json::Value {
    serde_json::json!({"type": "agent_response", "session_id": session, "text": text, "is_final": false})
}
fn final_resp(session: &str, text: &str) -> serde_json::Value {
    serde_json::json!({"type": "agent_response", "session_id": session, "text": text, "is_final": true})
}
fn body(v: &serde_json::Value) -> &str {
    v.get("text").and_then(|t| t.as_str()).unwrap_or("")
}

#[test]
fn reconcile_delta_is_forwarded_verbatim_and_accumulated() {
    let mut accum = HashMap::new();
    let mut p = delta("S1", "He");
    reconcile_stream_payload(CHAT_DELTA_TOPIC, &mut p, &mut accum);
    // The delta reaches the TUI unchanged (it appends it live)...
    assert_eq!(body(&p), "He");
    // ...and is recorded so the terminal can be reconciled against it.
    assert_eq!(accum.get("S1").map(String::as_str), Some("He"));
}

#[test]
fn reconcile_terminal_after_deltas_sends_empty_remainder() {
    let mut accum = HashMap::new();
    for tok in ["He", "llo", " world"] {
        let mut d = delta("S1", tok);
        reconcile_stream_payload(CHAT_DELTA_TOPIC, &mut d, &mut accum);
    }
    // The terminal carries the full authoritative text; the TUI already has
    // it from the deltas, so the body is rewritten to nothing (just flushes).
    let mut term = final_resp("S1", "Hello world");
    reconcile_stream_payload(CHAT_RESPONSE_TOPIC, &mut term, &mut accum);
    assert_eq!(body(&term), "");
    // The accumulator is dropped once the turn closes.
    assert!(!accum.contains_key("S1"));
}

#[test]
fn reconcile_terminal_without_deltas_keeps_full_text() {
    // A non-streaming provider emits only a terminal response (no deltas).
    // The TUI has nothing buffered, so the full text must pass through.
    let mut accum = HashMap::new();
    let mut term = final_resp("S1", "the whole reply");
    reconcile_stream_payload(CHAT_RESPONSE_TOPIC, &mut term, &mut accum);
    assert_eq!(body(&term), "the whole reply");
}

#[test]
fn reconcile_terminal_fills_tail_after_dropped_delta() {
    // A trailing delta was dropped (the proxy logs such drops separately):
    // the accumulated prefix is shorter than the authoritative full text, so
    // the terminal must carry the missing tail to complete the reply.
    let mut accum = HashMap::new();
    let mut d = delta("S1", "He");
    reconcile_stream_payload(CHAT_DELTA_TOPIC, &mut d, &mut accum);
    let mut term = final_resp("S1", "Hello");
    reconcile_stream_payload(CHAT_RESPONSE_TOPIC, &mut term, &mut accum);
    assert_eq!(body(&term), "llo");
}

#[test]
fn reconcile_terminal_on_mismatch_sends_empty_not_duplicate() {
    // A non-prefix mismatch (a mid-stream drop) can't be cleanly appended to
    // what the TUI already rendered, so prefer an empty body over re-sending
    // the whole reply on top of the streamed text (visible duplication).
    let mut accum = HashMap::new();
    let mut d = delta("S1", "Hi there");
    reconcile_stream_payload(CHAT_DELTA_TOPIC, &mut d, &mut accum);
    let mut term = final_resp("S1", "Hello");
    reconcile_stream_payload(CHAT_RESPONSE_TOPIC, &mut term, &mut accum);
    assert_eq!(body(&term), "");
}

#[test]
fn reconcile_is_session_scoped_across_concurrent_streams() {
    // Two sessions stream concurrently; each terminal reconciles against only
    // its own accumulated tokens.
    let mut accum = HashMap::new();
    for (s, t) in [("S1", "alpha"), ("S2", "beta"), ("S1", "-1"), ("S2", "-2")] {
        let mut d = delta(s, t);
        reconcile_stream_payload(CHAT_DELTA_TOPIC, &mut d, &mut accum);
    }
    let mut t1 = final_resp("S1", "alpha-1");
    let mut t2 = final_resp("S2", "beta-2");
    reconcile_stream_payload(CHAT_RESPONSE_TOPIC, &mut t1, &mut accum);
    reconcile_stream_payload(CHAT_RESPONSE_TOPIC, &mut t2, &mut accum);
    assert_eq!(body(&t1), "");
    assert_eq!(body(&t2), "");
    assert!(accum.is_empty());
}

#[test]
fn reconcile_ignores_non_final_response() {
    // Only the terminal (is_final) closes a turn; a non-final response on the
    // response topic (defensive — react publishes is_final:true) is left as-is
    // and does not consume the accumulator.
    let mut accum = HashMap::new();
    let mut d = delta("S1", "He");
    reconcile_stream_payload(CHAT_DELTA_TOPIC, &mut d, &mut accum);
    let mut not_final = serde_json::json!({"type": "agent_response", "session_id": "S1", "text": "x", "is_final": false});
    reconcile_stream_payload(CHAT_RESPONSE_TOPIC, &mut not_final, &mut accum);
    assert_eq!(body(&not_final), "x");
    assert_eq!(accum.get("S1").map(String::as_str), Some("He"));
}

#[test]
fn correlated_reply_with_session_id_is_not_dropped_for_unbound_session_client() {
    // Regression for the bootstrap case raised in review: a correlated /
    // session-creation reply carries a session_id, but the requesting
    // connection has not bound a session yet (client_session = None). Because
    // the reply is not chat-scoped, its outbound scope is None, so the
    // principal gate alone governs and it is delivered (not dropped).
    let reply = serde_json::json!({"type": "session_created", "session_id": "S_new"});
    let scope = outbound_session_scope("session.v1.response.create.abc", &reply);
    assert_eq!(scope, None);
    assert!(should_deliver(
        Some("default"),
        scope,
        Some("default"),
        None
    ));
}

// --- attribution target mapping ---

#[test]
fn attribution_target_extracts_principal_for_verified_and_claimed() {
    assert_eq!(
        attribution_target(&ipc::PrincipalAttribution::Verified("alice".to_string())),
        Some("alice")
    );
    assert_eq!(
        attribution_target(&ipc::PrincipalAttribution::Claimed("bob".to_string())),
        Some("bob")
    );
}

#[test]
fn attribution_target_is_none_for_system() {
    assert_eq!(attribution_target(&ipc::PrincipalAttribution::System), None);
}
