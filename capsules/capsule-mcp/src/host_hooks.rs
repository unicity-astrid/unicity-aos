//! Authenticated host-hook ingress shared by every Oracle plugin.

use astrid_sdk::prelude::*;
use serde::{Deserialize, Serialize};

const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const TOKEN_MIN_BYTES: usize = 32;
const TOKEN_MAX_BYTES: usize = 128;

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
struct HostHookRequest {
    schema_version: u8,
    principal_id: String,
    host: String,
    session_id: String,
    event: String,
    correlation_id: String,
    route_id: String,
    delivery_id: String,
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    payload: serde_json::Value,
    token: String,
}

#[derive(Debug, Serialize)]
struct ValidatedHostHook<'a> {
    schema_version: u8,
    principal_id: &'a str,
    host: &'a str,
    session_id: &'a str,
    event: &'a str,
    correlation_id: &'a str,
    route_id: &'a str,
    delivery_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_id: Option<&'a str>,
    payload: &'a serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
struct HostHookResponse {
    schema_version: u8,
    principal_id: String,
    host: String,
    session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<String>,
    correlation_id: String,
    route_id: String,
    delivery_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<String>,
}

pub(crate) fn handle(expected_host: &str, payload: serde_json::Value) -> Result<(), SysError> {
    let request: HostHookRequest = match serde_json::from_value(payload) {
        Ok(request) => request,
        Err(error) => {
            reject(expected_host, "unknown", "malformed_payload");
            log::warn(format!(
                "aos-mcp: malformed {expected_host} hook ingress: {error}"
            ));
            return Ok(());
        }
    };
    if let Err(reason) = validate_request(expected_host, &request) {
        reject(expected_host, &request.event, reason);
        return Ok(());
    }

    let caller = match runtime::caller() {
        Ok(caller) => caller,
        Err(error) => {
            reject(expected_host, &request.event, "caller_unavailable");
            log::warn(format!(
                "aos-mcp: caller unavailable for {expected_host} hook: {error}"
            ));
            return Ok(());
        }
    };
    if caller.principal.as_deref() != Some(request.principal_id.as_str()) {
        reject(expected_host, &request.event, "principal_mismatch");
        return Ok(());
    }

    let token_key = token_key(expected_host, &request.session_id);
    if !authenticate_token(&token_key, &request)? {
        reject(expected_host, &request.event, "token_mismatch");
        return Ok(());
    }

    ipc::publish_json(
        &format!("oracle.v1.hook.validated.{expected_host}"),
        &ValidatedHostHook {
            schema_version: 1,
            principal_id: &request.principal_id,
            host: &request.host,
            session_id: &request.session_id,
            event: &request.event,
            correlation_id: &request.correlation_id,
            route_id: &request.route_id,
            delivery_id: &request.delivery_id,
            turn_id: request.turn_id.as_deref(),
            workspace_id: request.workspace_id.as_deref(),
            payload: &request.payload,
        },
    )
}

pub(crate) fn relay_response(payload: serde_json::Value) -> Result<(), SysError> {
    let response: HostHookResponse = match serde_json::from_value(payload) {
        Ok(response) => response,
        Err(error) => {
            reject("unknown", "unknown", "malformed_response");
            log::warn(format!(
                "aos-mcp: malformed host-hook bridge response: {error}"
            ));
            return Ok(());
        }
    };
    let event = response.event.as_deref().unwrap_or("unknown");
    if let Err(reason) = validate_response_shape(&response) {
        reject(&response.host, event, reason);
        return Ok(());
    }

    let caller = match runtime::caller() {
        Ok(caller) => caller,
        Err(error) => {
            reject(&response.host, event, "caller_unavailable");
            log::warn(format!(
                "aos-mcp: caller unavailable for host-hook response: {error}"
            ));
            return Ok(());
        }
    };
    if caller.principal.as_deref() != Some(response.principal_id.as_str()) {
        reject(&response.host, event, "principal_mismatch");
        return Ok(());
    }

    let token_key = token_key(&response.host, &response.session_id);
    let Some(token) = kv::get_bytes_opt(&token_key)? else {
        reject(&response.host, event, "unknown_session_route");
        return Ok(());
    };
    let Ok(token) = std::str::from_utf8(&token) else {
        reject(&response.host, event, "invalid_stored_route");
        return Ok(());
    };
    if derive_route_id(&response.host, &response.session_id, token) != response.route_id {
        reject(&response.host, event, "route_mismatch");
        return Ok(());
    }

    ipc::publish_json(
        &format!("astrid.v1.response.{}", response.delivery_id),
        &response,
    )?;

    if matches!(response.event.as_deref(), Some("stop" | "session_end"))
        && let Err(error) = kv::delete(&token_key)
    {
        log::warn(format!(
            "aos-mcp: could not retire {} hook route: {error}",
            response.host
        ));
    }
    Ok(())
}

fn authenticate_token(key: &str, request: &HostHookRequest) -> Result<bool, SysError> {
    match kv::get_bytes_opt(key)? {
        Some(expected) => Ok(tokens_match(request.token.as_bytes(), &expected)),
        None if can_register(&request.event) => {
            if kv::cas(key, None, request.token.as_bytes())? {
                Ok(true)
            } else {
                Ok(kv::get_bytes_opt(key)?
                    .as_deref()
                    .is_some_and(|expected| tokens_match(request.token.as_bytes(), expected)))
            }
        }
        None => Ok(false),
    }
}

fn can_register(event: &str) -> bool {
    matches!(event, "session_start" | "user_prompt_submit")
}

fn token_key(host: &str, session: &str) -> String {
    format!("oracle.hook_token.{host}.{session}")
}

fn validate_request(expected_host: &str, request: &HostHookRequest) -> Result<(), &'static str> {
    if request.schema_version != 1 {
        return Err("unsupported_schema");
    }
    if request.host != expected_host || !is_host(&request.host) {
        return Err("host_mismatch");
    }
    validate_route_fields(
        &request.session_id,
        &request.event,
        &request.correlation_id,
        &request.route_id,
        &request.delivery_id,
    )?;
    if request.token.len() < TOKEN_MIN_BYTES
        || request.token.len() > TOKEN_MAX_BYTES
        || !request
            .token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("invalid_token_shape");
    }
    if derive_route_id(&request.host, &request.session_id, &request.token) != request.route_id {
        return Err("route_mismatch");
    }
    if request
        .workspace_id
        .as_deref()
        .is_some_and(|workspace| !is_segment(workspace, 128))
        || request
            .turn_id
            .as_deref()
            .is_some_and(|turn| turn.is_empty() || turn.len() > 256)
    {
        return Err("invalid_metadata");
    }
    if serde_json::to_vec(&request.payload)
        .map_or(true, |payload| payload.len() > MAX_PAYLOAD_BYTES)
    {
        return Err("payload_too_large");
    }
    Ok(())
}

fn validate_response_shape(response: &HostHookResponse) -> Result<(), &'static str> {
    if response.schema_version != 1 {
        return Err("unsupported_schema");
    }
    if !is_host(&response.host) {
        return Err("unsupported_host");
    }
    if response
        .event
        .as_deref()
        .is_some_and(|event| !is_segment(event, 128))
    {
        return Err("invalid_event");
    }
    validate_delivery_fields(
        &response.session_id,
        &response.correlation_id,
        &response.route_id,
        &response.delivery_id,
    )?;
    if response
        .context
        .as_ref()
        .is_some_and(|context| context.len() > MAX_CONTEXT_BYTES)
    {
        return Err("context_too_large");
    }
    Ok(())
}

fn validate_route_fields(
    session_id: &str,
    event: &str,
    correlation_id: &str,
    route_id: &str,
    delivery_id: &str,
) -> Result<(), &'static str> {
    if !is_segment(event, 128) {
        return Err("invalid_segment");
    }
    validate_delivery_fields(session_id, correlation_id, route_id, delivery_id)
}

fn validate_delivery_fields(
    session_id: &str,
    correlation_id: &str,
    route_id: &str,
    delivery_id: &str,
) -> Result<(), &'static str> {
    if !is_segment(session_id, 128) || !is_segment(delivery_id, 128) {
        return Err("invalid_segment");
    }
    if !is_lower_hex(correlation_id, 32) || !is_lower_hex(route_id, 64) {
        return Err("invalid_route");
    }
    if delivery_id != format!("{route_id}-{correlation_id}") {
        return Err("delivery_mismatch");
    }
    Ok(())
}

fn is_host(host: &str) -> bool {
    matches!(host, "codex" | "claude" | "grok")
}

fn derive_route_id(host: &str, session: &str, token: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"unicity-aos-hook-route-v1\0");
    hasher.update(host.as_bytes());
    hasher.update(b"\0");
    hasher.update(session.as_bytes());
    hasher.update(b"\0");
    hasher.update(token.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn tokens_match(claimed: &[u8], expected: &[u8]) -> bool {
    if claimed.len() != expected.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in claimed.iter().zip(expected) {
        difference |= left ^ right;
    }
    difference == 0
}

fn is_segment(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn reject(host: &str, event: &str, reason: &str) {
    if let Err(error) = ipc::publish_json(
        "astrid.v1.audit.hook_ingress_rejected",
        &serde_json::json!({
            "host": bounded_audit(host),
            "event": bounded_audit(event),
            "reason": reason,
        }),
    ) {
        log::warn(format!(
            "aos-mcp: could not audit rejected hook message: {error}"
        ));
    }
}

fn bounded_audit(value: &str) -> &str {
    let end = value.floor_char_boundary(value.len().min(128));
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> HostHookRequest {
        let token = "a".repeat(64);
        let route_id = derive_route_id("codex", "codex-session", &token);
        let correlation_id = "b".repeat(32);
        HostHookRequest {
            schema_version: 1,
            principal_id: "codex-code".to_owned(),
            host: "codex".to_owned(),
            session_id: "codex-session".to_owned(),
            event: "user_prompt_submit".to_owned(),
            delivery_id: format!("{route_id}-{correlation_id}"),
            correlation_id,
            route_id,
            turn_id: Some("turn-one".to_owned()),
            workspace_id: Some("workspace-one".to_owned()),
            payload: serde_json::json!({"prompt": "hello"}),
            token,
        }
    }

    fn response(request: &HostHookRequest) -> HostHookResponse {
        HostHookResponse {
            schema_version: 1,
            principal_id: request.principal_id.clone(),
            host: request.host.clone(),
            session_id: request.session_id.clone(),
            event: Some(request.event.clone()),
            correlation_id: request.correlation_id.clone(),
            route_id: request.route_id.clone(),
            delivery_id: request.delivery_id.clone(),
            context: Some("same-turn context".to_owned()),
        }
    }

    #[test]
    fn validates_exact_request_route_binding() {
        let mut value = request();
        assert!(validate_request("codex", &value).is_ok());
        value.route_id = "c".repeat(64);
        assert_eq!(validate_request("codex", &value), Err("delivery_mismatch"));
    }

    #[test]
    fn validates_exact_response_route_binding() {
        let request = request();
        let mut value = response(&request);
        assert!(validate_response_shape(&value).is_ok());
        assert_eq!(
            derive_route_id(&value.host, &value.session_id, &request.token),
            value.route_id
        );
        value.delivery_id = format!("{}-{}", "c".repeat(64), value.correlation_id);
        assert_eq!(validate_response_shape(&value), Err("delivery_mismatch"));
    }

    #[test]
    fn response_event_is_additive_for_staggered_upgrades() {
        let request = request();
        let mut value = response(&request);
        value.event = None;
        assert!(validate_response_shape(&value).is_ok());
    }

    #[test]
    fn response_accepts_future_additive_fields() {
        let request = request();
        let mut value = serde_json::to_value(response(&request)).expect("serialize response");
        value["future_metadata"] = serde_json::json!({"revision": 2});
        let decoded: HostHookResponse =
            serde_json::from_value(value).expect("deserialize additive response");
        assert!(validate_response_shape(&decoded).is_ok());
    }

    #[test]
    fn request_accepts_future_additive_fields() {
        let mut value = serde_json::to_value(request()).expect("serialize request");
        value["future_metadata"] = serde_json::json!({"revision": 2});
        let decoded: HostHookRequest =
            serde_json::from_value(value).expect("deserialize additive request");
        assert!(validate_request("codex", &decoded).is_ok());
    }

    #[test]
    fn token_comparison_is_exact() {
        assert!(tokens_match(b"same", b"same"));
        assert!(!tokens_match(b"same", b"sand"));
        assert!(!tokens_match(b"short", b"longer"));
    }

    #[test]
    fn only_session_entry_events_register_routes() {
        assert!(can_register("session_start"));
        assert!(can_register("user_prompt_submit"));
        assert!(!can_register("pre_tool_use"));
        assert!(!can_register("stop"));
    }

    #[test]
    fn segments_reject_topic_smuggling() {
        assert!(is_segment("codex-session_1", 128));
        assert!(!is_segment("codex.session", 128));
        assert!(!is_segment("../session", 128));
    }

    #[test]
    fn validated_shape_does_not_contain_token() {
        let request = request();
        let value = serde_json::to_value(ValidatedHostHook {
            schema_version: 1,
            principal_id: &request.principal_id,
            host: &request.host,
            session_id: &request.session_id,
            event: &request.event,
            correlation_id: &request.correlation_id,
            route_id: &request.route_id,
            delivery_id: &request.delivery_id,
            turn_id: request.turn_id.as_deref(),
            workspace_id: request.workspace_id.as_deref(),
            payload: &request.payload,
        })
        .expect("serialize validated hook");
        assert!(value.get("token").is_none());
    }
}
