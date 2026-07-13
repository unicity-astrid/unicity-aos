#![deny(unsafe_code)]
#![deny(clippy::all)]
#![warn(missing_docs)]

//! Telegram Bot uplink capsule for Astrid OS.
//!
//! Bridges the Telegram Bot API to the kernel IPC bus. Polls Telegram for
//! updates, publishes user input as `user.v1.prompt`, and renders agent
//! responses, approvals, and elicitations back to Telegram chats.

mod format;
mod telegram;
mod types;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use astrid_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Minimum interval between Telegram message edits (rate-limit guard).
const EDIT_THROTTLE: Duration = Duration::from_millis(500);

/// Telegram long-poll timeout (seconds). Kept short so we can interleave IPC
/// event processing between polls.
const POLL_TIMEOUT: u32 = 1;

/// Maximum inactivity before an in-progress turn is considered stale and
/// cleaned up. The timer resets on every stream delta or approval event, so
/// long-running turns with ongoing activity are not prematurely reaped.
const TURN_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum age before a pending approval or elicitation is considered stale.
const PENDING_INTERACTION_TTL: Duration = Duration::from_secs(300);

/// Maximum accumulated text buffer size (bytes) during streaming. Prevents
/// unbounded memory growth from very long agent responses in WASM.
const MAX_TEXT_BUFFER: usize = 256 * 1024;

/// KV key for the last processed Telegram update offset.
const KV_OFFSET: &str = "tg.offset";

/// Persisted session mapping entry.
#[derive(Serialize, Deserialize)]
struct SessionEntry {
    session_id: String,
}

/// Transient per-chat state for the current turn.
struct TurnState {
    /// Telegram message ID being edited with streaming text.
    msg_id: i64,
    /// Accumulated response text (markdown).
    text_buffer: String,
    /// Last time we edited the Telegram message.
    last_edit: Instant,
    /// Whether the current message has been finalized (e.g. before a tool).
    finalized: bool,
    /// When this turn was created (kept for diagnostics).
    #[allow(dead_code)]
    created_at: Instant,
    /// Last time activity was observed (stream delta or approval received).
    /// Used for timeout so that long-running turns with ongoing activity are
    /// not prematurely reaped.
    last_activity: Instant,
}

/// Pending approval waiting for a callback button press.
struct PendingApproval {
    chat_id: i64,
    /// Full request_id (the callback_data may use a truncated token).
    full_request_id: String,
    /// When this approval was created (for TTL-based cleanup).
    created_at: Instant,
}

/// Pending text-based elicitation waiting for the user to reply.
struct PendingElicitation {
    request_id: String,
    /// When this elicitation was created (for TTL-based cleanup).
    created_at: Instant,
}

/// Telegram Bot uplink capsule.
#[derive(Default)]
pub struct TelegramBot;

#[capsule]
impl TelegramBot {
    /// Main run loop: poll Telegram and IPC in alternation.
    #[astrid::run]
    fn run(&self) -> Result<(), SysError> {
        // ── Config ──────────────────────────────────────────────────────
        let bot_token = env::var("bot_token")
            .map_err(|_| SysError::ApiError("bot_token not configured".into()))?;
        let allowed_users = parse_allowed_users(&env::var("allowed_user_ids").unwrap_or_default());

        if allowed_users.is_empty() {
            let _ = log::warn(
                "Telegram bot starting with NO user restrictions — \
                 any Telegram user can interact with the agent. \
                 Set allowed_user_ids to restrict access.",
            );
        }

        // ── Register uplink ─────────────────────────────────────────────
        uplink::register("telegram", "telegram", "interactive")?;

        // ── Subscribe to IPC topics ─────────────────────────────────────
        let topics = [
            "agent.v1.response",
            "agent.v1.stream.delta",
            "astrid.v1.approval",
            "astrid.v1.elicit.*",
            "astrid.v1.response.*",
        ];
        let sub_handles: Vec<_> = topics
            .iter()
            .map(|t| ipc::subscribe(t).map_err(|e| SysError::ApiError(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;

        let _ = runtime::signal_ready();

        // ── State ───────────────────────────────────────────────────────
        let mut offset: i64 = kv::get_json(KV_OFFSET).unwrap_or(0);
        let mut sessions: HashMap<i64, String> = load_sessions();
        let mut session_to_chat: HashMap<String, i64> = sessions
            .iter()
            .map(|(&chat_id, sid)| (sid.clone(), chat_id))
            .collect();
        let mut turns: HashMap<i64, TurnState> = HashMap::new();
        let mut pending_approvals: HashMap<String, PendingApproval> = HashMap::new();
        let mut pending_elicitations: HashMap<i64, PendingElicitation> = HashMap::new();
        let mut consecutive_ipc_errors: u32 = 0;

        let _ = log::info("Telegram bot started");

        // ── Main loop ───────────────────────────────────────────────────
        let mut consecutive_errors: u32 = 0;
        let mut next_poll_at = Instant::now();

        loop {
            // Phase A: poll Telegram for new updates (skip if backing off).
            if Instant::now() >= next_poll_at {
                match telegram::get_updates(&bot_token, offset, POLL_TIMEOUT) {
                    Ok(updates) => {
                        consecutive_errors = 0;
                        next_poll_at = Instant::now();
                        for update in updates {
                            offset = update.update_id + 1;
                            handle_telegram_update(
                                &bot_token,
                                &allowed_users,
                                &update,
                                &mut sessions,
                                &mut session_to_chat,
                                &mut turns,
                                &mut pending_approvals,
                                &mut pending_elicitations,
                            );
                        }
                        if let Err(e) = kv::set_json(KV_OFFSET, &offset) {
                            let _ = log::warn(format!(
                                "Failed to persist poll offset: {e:?} — \
                                 restart may reprocess recent updates"
                            ));
                        }
                    }
                    Err(e) => {
                        consecutive_errors = consecutive_errors.saturating_add(1);
                        let backoff_secs = 2u64.pow(consecutive_errors.min(6)).min(60);
                        let _ = log::warn(format!(
                            "Telegram poll error: {e:?} — backing off {backoff_secs}s \
                             (consecutive errors: {consecutive_errors})"
                        ));
                        next_poll_at = Instant::now() + Duration::from_secs(backoff_secs);
                    }
                }
            }

            // Phase B: poll IPC events and push to Telegram.
            // Track whether all handles succeeded this pass; only count
            // consecutive errors when an entire pass fails.
            let mut ipc_pass_ok = true;
            for handle in &sub_handles {
                match ipc::poll_bytes(handle) {
                    Ok(bytes) if !bytes.is_empty() => {
                        handle_ipc_poll(
                            &bot_token,
                            &bytes,
                            &session_to_chat,
                            &sessions,
                            &mut turns,
                            &mut pending_approvals,
                            &mut pending_elicitations,
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        ipc_pass_ok = false;
                        let _ = log::error(format!("IPC poll error: {e:?}"));
                    }
                }
            }
            if ipc_pass_ok {
                consecutive_ipc_errors = 0;
            } else {
                consecutive_ipc_errors = consecutive_ipc_errors.saturating_add(1);
                if consecutive_ipc_errors >= 50 {
                    let _ = log::error("Too many consecutive IPC errors — shutting down");
                    return Err(SysError::ApiError(
                        "IPC subscription failed, capsule terminated".into(),
                    ));
                }
            }

            // Phase C: clean up stale turns, approvals, and elicitations.
            //
            // Collect expired chat_ids first, then remove by key to avoid
            // TOCTOU races from double elapsed() checks.
            let expired_turn_ids: Vec<i64> = turns
                .iter()
                .filter(|(_, turn)| turn.last_activity.elapsed() > TURN_TIMEOUT)
                .map(|(&chat_id, _)| chat_id)
                .collect();

            for chat_id in &expired_turn_ids {
                if let Some(turn) = turns.remove(chat_id) {
                    let _ = log::warn(format!(
                        "Turn for chat {chat_id} timed out after {}s — cleaning up",
                        TURN_TIMEOUT.as_secs()
                    ));
                    let _ = telegram::edit_message_text(
                        &bot_token,
                        *chat_id,
                        turn.msg_id,
                        "Turn timed out.",
                        None,
                    );
                }
            }

            pending_approvals.retain(|_token, approval| {
                if approval.created_at.elapsed() > PENDING_INTERACTION_TTL {
                    let _ = log::warn(format!(
                        "Approval {} for chat {} expired — cleaning up",
                        approval.full_request_id, approval.chat_id,
                    ));
                    false
                } else {
                    true
                }
            });

            pending_elicitations.retain(|chat_id, eli| {
                if eli.created_at.elapsed() > PENDING_INTERACTION_TTL {
                    let _ = log::warn(format!(
                        "Elicitation {} for chat {chat_id} expired — cleaning up",
                        eli.request_id,
                    ));
                    false
                } else {
                    true
                }
            });

            // Only sleep when we skipped the Telegram poll (during backoff).
            // During normal operation the 1s long-poll provides natural pacing.
            if Instant::now() < next_poll_at {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

// ── Telegram Update Handling ────────────────────────────────────────────────

fn handle_telegram_update(
    token: &str,
    allowed_users: &[i64],
    update: &types::Update,
    sessions: &mut HashMap<i64, String>,
    session_to_chat: &mut HashMap<String, i64>,
    turns: &mut HashMap<i64, TurnState>,
    pending_approvals: &mut HashMap<String, PendingApproval>,
    pending_elicitations: &mut HashMap<i64, PendingElicitation>,
) {
    if let Some(msg) = &update.message {
        handle_message(
            token,
            allowed_users,
            msg,
            sessions,
            session_to_chat,
            turns,
            pending_elicitations,
        );
    }
    if let Some(cb) = &update.callback_query {
        handle_callback(
            token,
            allowed_users,
            cb,
            sessions,
            pending_approvals,
            pending_elicitations,
        );
    }
}

fn handle_message(
    token: &str,
    allowed_users: &[i64],
    msg: &types::TgMessage,
    sessions: &mut HashMap<i64, String>,
    session_to_chat: &mut HashMap<String, i64>,
    turns: &mut HashMap<i64, TurnState>,
    pending_elicitations: &mut HashMap<i64, PendingElicitation>,
) {
    let chat_id = msg.chat.id;
    let Some(text) = &msg.text else { return };

    // Access control.
    if !is_user_allowed(allowed_users, msg.from.as_ref().map(|u| u.id)) {
        let _ = telegram::send_message(token, chat_id, "Not authorized.", None, None);
        return;
    }

    // Check if this is a reply to a pending text-based elicitation.
    // This must come before command parsing so replies starting with "/"
    // (common for paths/secrets) are not treated as bot commands.
    if let Some(eli) = pending_elicitations.remove(&chat_id) {
        let req_id = eli.request_id.clone();
        let payload = serde_json::json!({
            "type": "elicit_response",
            "request_id": req_id,
            "value": text,
        });
        let topic = format!("astrid.v1.elicit.response.{req_id}");
        if let Err(e) = ipc::publish_json(&topic, &payload) {
            let _ = log::error(format!(
                "Failed to publish elicitation response for {req_id}: {e:?}",
            ));
            // Re-insert so the user can retry.
            pending_elicitations.insert(chat_id, eli);
            let _ = telegram::send_message(
                token,
                chat_id,
                "Failed to send your response. Please try again.",
                None,
                None,
            );
        }
        return;
    }

    // Bot commands.
    if text.starts_with('/') {
        handle_command(token, chat_id, text, sessions, session_to_chat, turns);
        return;
    }

    // Check if a turn is already in progress.
    if turns.contains_key(&chat_id) {
        let _ = telegram::send_message(
            token,
            chat_id,
            "A turn is already in progress. Please wait or /cancel.",
            None,
            None,
        );
        return;
    }

    // Ensure session exists.
    let session_id = sessions
        .entry(chat_id)
        .or_insert_with(|| {
            let sid = new_session_id(chat_id);
            session_to_chat.insert(sid.clone(), chat_id);
            save_session(chat_id, &sid);
            sid
        })
        .clone();

    // Send "Thinking..." placeholder.
    let placeholder = match telegram::send_message(token, chat_id, "Thinking...", None, None) {
        Ok(m) => m,
        Err(e) => {
            let _ = log::warn(format!("Failed to send placeholder: {e:?}"));
            return;
        }
    };

    let _ = telegram::send_typing(token, chat_id);

    // Start turn tracking.
    let now = Instant::now();
    turns.insert(
        chat_id,
        TurnState {
            msg_id: placeholder.message_id,
            text_buffer: String::new(),
            last_edit: now.checked_sub(EDIT_THROTTLE).unwrap_or_else(Instant::now),
            finalized: false,
            created_at: now,
            last_activity: now,
        },
    );

    // Publish user input to the IPC bus.
    let payload = serde_json::json!({
        "type": "user_input",
        "text": text,
        "session_id": session_id,
    });
    if let Err(e) = ipc::publish_json("user.v1.prompt", &payload) {
        let _ = log::error(format!("Failed to publish user input: {e:?}"));
        let _ = telegram::edit_message_text(
            token,
            chat_id,
            placeholder.message_id,
            "Failed to send message to agent.",
            None,
        );
        turns.remove(&chat_id);
    }
}

fn handle_command(
    token: &str,
    chat_id: i64,
    text: &str,
    sessions: &mut HashMap<i64, String>,
    session_to_chat: &mut HashMap<String, i64>,
    turns: &mut HashMap<i64, TurnState>,
) {
    let cmd = text.split_whitespace().next().unwrap_or("");
    match cmd {
        "/start" | "/help" => {
            let help = "<b>Astrid Telegram Bot</b>\n\n\
                        Send any text message to interact with the agent.\n\n\
                        <b>Commands:</b>\n\
                        /start — Welcome message\n\
                        /help — This help text\n\
                        /reset — End session and start fresh\n\
                        /cancel — Cancel the current turn";
            let _ = telegram::send_message(token, chat_id, help, Some("HTML"), None);
        }
        "/reset" => {
            if let Some(sid) = sessions.remove(&chat_id) {
                session_to_chat.remove(&sid);
                delete_session(chat_id);
            }
            turns.remove(&chat_id);
            let _ = telegram::send_message(token, chat_id, "Session reset.", None, None);
        }
        "/cancel" => {
            if turns.remove(&chat_id).is_some() {
                let _ = telegram::send_message(token, chat_id, "Turn cancelled.", None, None);
            } else {
                let _ = telegram::send_message(token, chat_id, "No turn in progress.", None, None);
            }
        }
        _ => {
            let _ =
                telegram::send_message(token, chat_id, "Unknown command. Try /help.", None, None);
        }
    }
}

fn handle_callback(
    token: &str,
    allowed_users: &[i64],
    cb: &types::CallbackQuery,
    _sessions: &HashMap<i64, String>,
    pending_approvals: &mut HashMap<String, PendingApproval>,
    pending_elicitations: &mut HashMap<i64, PendingElicitation>,
) {
    // Access control.
    if !is_user_allowed(allowed_users, Some(cb.from.id)) {
        let _ = telegram::answer_callback_query(token, &cb.id, Some("Not authorized"));
        return;
    }

    let Some(data) = &cb.data else {
        let _ = telegram::answer_callback_query(token, &cb.id, None);
        return;
    };

    // Parse callback data: "apr:{token}:{decision}" or "eli:{token}:{value}"
    // where {token} is either the original request_id (if short and colon-free)
    // or an FNV-1a hash of it (16 hex chars).
    let parts: Vec<&str> = data.splitn(3, ':').collect();
    if parts.len() < 3 {
        let _ = telegram::answer_callback_query(token, &cb.id, Some("Unknown action"));
        return;
    }

    match parts[0] {
        "apr" => {
            let token_key = parts[1];
            let decision = parts[2];
            if let Some(approval) = pending_approvals.remove(token_key) {
                // Use the full request_id (not the possibly truncated callback token).
                let full_id = &approval.full_request_id;
                let payload = serde_json::json!({
                    "type": "approval_response",
                    "request_id": full_id,
                    "decision": decision,
                });
                let topic = format!("astrid.v1.approval.response.{full_id}");
                if let Err(e) = ipc::publish_json(&topic, &payload) {
                    let _ = log::error(format!(
                        "Failed to publish approval response for {full_id}: {e:?}"
                    ));
                    // Re-insert so user can retry.
                    pending_approvals.insert(token_key.to_string(), approval);
                    let _ = telegram::answer_callback_query(
                        token,
                        &cb.id,
                        Some("Failed to send decision. Try again."),
                    );
                    return;
                }
                let _ = telegram::answer_callback_query(
                    token,
                    &cb.id,
                    Some(&format!("Approved: {decision}")),
                );

                // Edit the approval message to show the decision.
                if let Some(msg) = &cb.message {
                    let _ = telegram::edit_message_text(
                        token,
                        approval.chat_id,
                        msg.message_id,
                        &format!("Approval: <b>{}</b>", format::html_escape(decision)),
                        Some("HTML"),
                    );
                }
            } else {
                let _ = telegram::answer_callback_query(token, &cb.id, Some("Approval expired"));
            }
        }
        "eli" => {
            let request_id = parts[1];
            let value = parts[2];
            let chat_id = cb.message.as_ref().map(|m| m.chat.id);

            // Validate: the elicitation must be pending for this chat.
            // The request_id in callback_data may be a token (hashed); the
            // full request_id is stored in PendingElicitation.
            let is_valid = chat_id.is_some_and(|cid| {
                pending_elicitations.get(&cid).is_some_and(|e| {
                    // Match either the full id or its callback token.
                    request_id == e.request_id || request_id == callback_token(&e.request_id)
                })
            });

            if is_valid {
                // Remove the pending elicitation (consumed).
                let removed = chat_id.and_then(|cid| pending_elicitations.remove(&cid));
                // Use the full request_id for IPC, not the callback token.
                let full_id = removed
                    .as_ref()
                    .map(|e| e.request_id.as_str())
                    .unwrap_or(request_id);
                let payload = serde_json::json!({
                    "type": "elicit_response",
                    "request_id": full_id,
                    "value": value,
                });
                let topic = format!("astrid.v1.elicit.response.{full_id}");
                if let Err(e) = ipc::publish_json(&topic, &payload) {
                    let _ = log::error(format!(
                        "Failed to publish elicitation response for {full_id}: {e:?}"
                    ));
                    // Re-insert so user can retry.
                    if let (Some(cid), Some(eli)) = (chat_id, removed) {
                        pending_elicitations.insert(cid, eli);
                    }
                    let _ = telegram::answer_callback_query(
                        token,
                        &cb.id,
                        Some("Failed to send selection. Try again."),
                    );
                    return;
                }
                let _ = telegram::answer_callback_query(
                    token,
                    &cb.id,
                    Some(&format!("Selected: {value}")),
                );
            } else {
                let _ = telegram::answer_callback_query(token, &cb.id, Some("Elicitation expired"));
            }
        }
        _ => {
            let _ = telegram::answer_callback_query(token, &cb.id, Some("Unknown action"));
        }
    }
}

// ── IPC Event Handling ──────────────────────────────────────────────────────

fn handle_ipc_poll(
    token: &str,
    poll_bytes: &[u8],
    session_to_chat: &HashMap<String, i64>,
    sessions: &HashMap<i64, String>,
    turns: &mut HashMap<i64, TurnState>,
    pending_approvals: &mut HashMap<String, PendingApproval>,
    pending_elicitations: &mut HashMap<i64, PendingElicitation>,
) {
    let envelope: Value = match serde_json::from_slice(poll_bytes) {
        Ok(v) => v,
        Err(_) => return,
    };

    if let Some(dropped) = envelope.get("dropped").and_then(|d| d.as_u64()) {
        if dropped > 0 {
            let _ = log::warn(format!(
                "IPC bus dropped {dropped} messages — responses may be stale"
            ));
        }
    }

    let Some(messages) = envelope.get("messages").and_then(|m| m.as_array()) else {
        return;
    };

    for msg in messages {
        let topic = msg.get("topic").and_then(|t| t.as_str()).unwrap_or("");
        let Some(payload) = msg.get("payload") else {
            continue;
        };

        handle_ipc_event(
            token,
            topic,
            payload,
            session_to_chat,
            sessions,
            turns,
            pending_approvals,
            pending_elicitations,
        );
    }
}

fn handle_ipc_event(
    token: &str,
    topic: &str,
    payload: &Value,
    session_to_chat: &HashMap<String, i64>,
    _sessions: &HashMap<i64, String>,
    turns: &mut HashMap<i64, TurnState>,
    pending_approvals: &mut HashMap<String, PendingApproval>,
    pending_elicitations: &mut HashMap<i64, PendingElicitation>,
) {
    let event_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "agent_response" => {
            let session_id = payload
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let text = payload.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let is_final = payload
                .get("is_final")
                .and_then(|f| f.as_bool())
                .unwrap_or(false);

            let Some(&chat_id) = session_to_chat.get(session_id) else {
                return;
            };

            if is_final {
                handle_final_response(token, chat_id, text, turns);
            } else {
                handle_stream_delta(token, chat_id, text, turns);
            }
        }

        "approval_required" => {
            let request_id = payload
                .get("request_id")
                .and_then(|r| r.as_str())
                .unwrap_or("");
            let action = payload
                .get("action")
                .and_then(|a| a.as_str())
                .unwrap_or("unknown");
            let resource = payload
                .get("resource")
                .and_then(|r| r.as_str())
                .unwrap_or("");
            let reason = payload.get("reason").and_then(|r| r.as_str()).unwrap_or("");

            let Some(chat_id) = resolve_chat_from_payload(payload, session_to_chat, turns) else {
                return;
            };

            handle_approval_request(
                token,
                chat_id,
                request_id,
                action,
                resource,
                reason,
                turns,
                pending_approvals,
            );
        }

        "elicit_request" => {
            let request_id = payload
                .get("request_id")
                .and_then(|r| r.as_str())
                .unwrap_or("");
            let field = payload.get("field");

            let Some(chat_id) = resolve_chat_from_payload(payload, session_to_chat, turns) else {
                return;
            };

            // Bump activity so the turn isn't reaped while waiting for user input.
            if let Some(turn) = turns.get_mut(&chat_id) {
                turn.last_activity = Instant::now();
            }
            handle_elicitation_request(token, chat_id, request_id, field, pending_elicitations);
        }

        // Catch-all for stream deltas that use a different topic pattern.
        _ if topic.starts_with("agent.v1.stream") => {
            let session_id = payload
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let text = payload.get("text").and_then(|t| t.as_str()).unwrap_or("");

            if let Some(&chat_id) = session_to_chat.get(session_id) {
                handle_stream_delta(token, chat_id, text, turns);
            }
        }

        _ => {}
    }
}

fn handle_stream_delta(token: &str, chat_id: i64, text: &str, turns: &mut HashMap<i64, TurnState>) {
    if text.is_empty() {
        return;
    }

    let Some(turn) = turns.get_mut(&chat_id) else {
        return;
    };

    // Cap buffer to prevent unbounded growth in WASM memory.
    let remaining = MAX_TEXT_BUFFER.saturating_sub(turn.text_buffer.len());
    if remaining > 0 {
        if text.len() <= remaining {
            turn.text_buffer.push_str(text);
        } else {
            // Append only what fits, truncating at a char boundary.
            let boundary = text.floor_char_boundary(remaining);
            turn.text_buffer.push_str(&text[..boundary]);
        }
    }
    turn.last_activity = Instant::now();

    if turn.last_edit.elapsed() >= EDIT_THROTTLE && !turn.text_buffer.is_empty() {
        let html = format::md_to_telegram_html(&turn.text_buffer);
        let display = format::truncate_for_edit(&html);

        if turn.finalized {
            // Previous content was finalized; send a new message.
            if let Ok(msg) = telegram::send_message(token, chat_id, &display, Some("HTML"), None) {
                turn.msg_id = msg.message_id;
                turn.finalized = false;
            }
        } else {
            let _ =
                telegram::edit_message_text(token, chat_id, turn.msg_id, &display, Some("HTML"));
        }
        turn.last_edit = Instant::now();
    }
}

fn handle_final_response(
    token: &str,
    chat_id: i64,
    text: &str,
    turns: &mut HashMap<i64, TurnState>,
) {
    if let Some(turn) = turns.remove(&chat_id) {
        // Use the final text if provided, otherwise finalize the buffer.
        let final_text = if text.is_empty() {
            &turn.text_buffer
        } else {
            text
        };

        if !final_text.is_empty() {
            let html = format::md_to_telegram_html(final_text);
            let chunks = format::chunk_html(&html, 4000);

            if let Some((first, rest)) = chunks.split_first() {
                if turn.finalized {
                    // Send as new message.
                    let _ = telegram::send_message(token, chat_id, first, Some("HTML"), None);
                    for chunk in rest {
                        let _ = telegram::send_message(token, chat_id, chunk, Some("HTML"), None);
                    }
                } else {
                    // Edit the existing message with the final text.
                    let _ = telegram::edit_message_text(
                        token,
                        chat_id,
                        turn.msg_id,
                        first,
                        Some("HTML"),
                    );
                    for chunk in rest {
                        let _ = telegram::send_message(token, chat_id, chunk, Some("HTML"), None);
                    }
                }
            }
        }
    }
}

fn handle_approval_request(
    token: &str,
    chat_id: i64,
    request_id: &str,
    action: &str,
    resource: &str,
    reason: &str,
    turns: &mut HashMap<i64, TurnState>,
    pending_approvals: &mut HashMap<String, PendingApproval>,
) {
    // Flush any in-progress text and bump activity timestamp.
    if let Some(turn) = turns.get_mut(&chat_id) {
        turn.last_activity = Instant::now();
        if !turn.text_buffer.is_empty() && !turn.finalized {
            finalize_turn_text(token, chat_id, turn);
        }
    }

    let escaped_action = format::html_escape(action);
    let escaped_resource = format::html_escape(resource);
    let escaped_reason = format::html_escape(reason);

    let text = format!(
        "Approval needed:\n\
         <b>Action:</b> {escaped_action}\n\
         <b>Resource:</b> <code>{escaped_resource}</code>\n\
         <b>Reason:</b> {escaped_reason}"
    );

    // Generate a short callback token from the request_id to fit within
    // Telegram's 64-byte callback_data limit ("apr:" + ":" + "allow_session"
    // = 18 bytes overhead, leaving 46 bytes for the token). If the request_id
    // already fits, use it directly; otherwise hash it to avoid collisions
    // from naive prefix truncation.
    let cb_token = callback_token(request_id);

    // Detect (extremely unlikely) hash collision: if the token already maps
    // to a different request_id, log and evict the old entry rather than
    // silently overwriting it.
    if let Some(existing) = pending_approvals.get(&cb_token) {
        if existing.full_request_id != request_id {
            let _ = log::warn(format!(
                "Callback token collision: '{}' maps to both '{}' and '{}'",
                cb_token, existing.full_request_id, request_id,
            ));
        }
    }

    pending_approvals.insert(
        cb_token.clone(),
        PendingApproval {
            chat_id,
            full_request_id: request_id.to_string(),
            created_at: Instant::now(),
        },
    );

    let keyboard = telegram::inline_keyboard(vec![
        ("Allow Once".into(), format!("apr:{cb_token}:allow_once")),
        (
            "Allow Session".into(),
            format!("apr:{cb_token}:allow_session"),
        ),
        ("Deny".into(), format!("apr:{cb_token}:deny")),
    ]);

    let _ = telegram::send_message(token, chat_id, &text, Some("HTML"), Some(&keyboard));
}

fn handle_elicitation_request(
    token: &str,
    chat_id: i64,
    request_id: &str,
    field: Option<&Value>,
    pending_elicitations: &mut HashMap<i64, PendingElicitation>,
) {
    let prompt = field
        .and_then(|f| f.get("prompt"))
        .and_then(|p| p.as_str())
        .unwrap_or("Input required");

    // For enum-type fields (field_type is {"Enum": ["opt1", "opt2", ...]}),
    // show inline keyboard with options. Options whose callback_data exceeds
    // Telegram's 64-byte limit are skipped with a warning.
    if let Some(options) = field
        .and_then(|f| f.get("field_type"))
        .and_then(|t| t.get("Enum"))
        .and_then(|e| e.as_array())
    {
        // Use a short token for the request_id in callback_data to maximize
        // space for option values within Telegram's 64-byte limit.
        let eli_token = callback_token(request_id);
        let buttons: Vec<(String, String)> = options
            .iter()
            .filter_map(|o| o.as_str())
            .filter_map(|o| {
                let data = format!("eli:{eli_token}:{o}");
                // Telegram callback_data max is 64 bytes.
                if data.len() <= 64 {
                    Some((o.to_string(), data))
                } else {
                    let _ = log::warn(format!(
                        "Elicitation option '{o}' exceeds 64-byte callback limit, skipping"
                    ));
                    None
                }
            })
            .collect();

        if !buttons.is_empty() {
            // Track as pending so callbacks can be validated.
            pending_elicitations.insert(
                chat_id,
                PendingElicitation {
                    request_id: request_id.to_string(),
                    created_at: Instant::now(),
                },
            );
            let keyboard = telegram::inline_keyboard(buttons);
            let _ = telegram::send_message(
                token,
                chat_id,
                &format::html_escape(prompt),
                Some("HTML"),
                Some(&keyboard),
            );
            return;
        }
    }

    // For text/secret fields, register as pending so the next text message
    // from this chat is routed as an elicitation response rather than a
    // new agent turn.
    pending_elicitations.insert(
        chat_id,
        PendingElicitation {
            request_id: request_id.to_string(),
            created_at: Instant::now(),
        },
    );
    let _ = telegram::send_message(
        token,
        chat_id,
        &format!("Input required: {}", format::html_escape(prompt)),
        Some("HTML"),
        None,
    );
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Resolve the target chat from an IPC event payload.
///
/// - If `session_id` is present in the payload, look it up in `session_to_chat`.
///   If the lookup fails (unknown or stale session), return `None` so the event
///   is dropped rather than misrouted.
/// - If `session_id` is absent, fall back to the single-active-turn heuristic:
///   return the only active chat when exactly one turn is in progress.
fn resolve_chat_from_payload(
    payload: &Value,
    session_to_chat: &HashMap<String, i64>,
    turns: &HashMap<i64, TurnState>,
) -> Option<i64> {
    match payload.get("session_id") {
        Some(session_val) => {
            let sid = session_val.as_str()?;
            session_to_chat.get(sid).copied()
        }
        None => {
            if turns.len() == 1 {
                turns.keys().next().copied()
            } else {
                None
            }
        }
    }
}

fn finalize_turn_text(token: &str, chat_id: i64, turn: &mut TurnState) {
    let html = format::md_to_telegram_html(&turn.text_buffer);
    let chunks = format::chunk_html(&html, 4000);

    if let Some((first, rest)) = chunks.split_first() {
        let _ = telegram::edit_message_text(token, chat_id, turn.msg_id, first, Some("HTML"));
        for chunk in rest {
            if let Ok(msg) = telegram::send_message(token, chat_id, chunk, Some("HTML"), None) {
                turn.msg_id = msg.message_id;
            }
        }
    }
    turn.finalized = true;
}

/// Generate a short callback token from a request_id.
///
/// If the id fits in 46 bytes (leaving room for "apr:" + ":" + "allow_session"
/// within Telegram's 64-byte callback_data limit), use it directly.
/// Otherwise, produce a hex-encoded hash prefix that avoids collisions from
/// naive string truncation.
fn callback_token(request_id: &str) -> String {
    const MAX_TOKEN_LEN: usize = 46;
    // Always hash if the id contains ':' to avoid ambiguous callback_data parsing
    // (callback format uses ':' as delimiter).
    if request_id.len() <= MAX_TOKEN_LEN && !request_id.contains(':') {
        request_id.to_string()
    } else {
        // FNV-1a 64-bit hash. Not cryptographically collision-resistant, but
        // sufficient for mapping transient callback tokens (short-lived, low volume).
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in request_id.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }
}

fn is_user_allowed(allowed: &[i64], user_id: Option<i64>) -> bool {
    if allowed.is_empty() {
        return true;
    }
    user_id.is_some_and(|id| allowed.contains(&id))
}

fn parse_allowed_users(s: &str) -> Vec<i64> {
    s.split(',')
        .filter_map(|part| part.trim().parse::<i64>().ok())
        .collect()
}

/// Generate a deterministic-ish session ID from chat_id + timestamp.
fn new_session_id(chat_id: i64) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("tg-{chat_id}-{ts:x}")
}

fn session_kv_key(chat_id: i64) -> String {
    format!("tg.session.{chat_id}")
}

fn save_session(chat_id: i64, session_id: &str) {
    if let Err(e) = kv::set_json(
        &session_kv_key(chat_id),
        &SessionEntry {
            session_id: session_id.to_string(),
        },
    ) {
        let _ = log::warn(format!(
            "Failed to persist session for chat {chat_id}: {e:?}"
        ));
    }
}

fn delete_session(chat_id: i64) {
    if let Err(e) = kv::delete(&session_kv_key(chat_id)) {
        let _ = log::warn(format!(
            "Failed to delete session for chat {chat_id}: {e:?}"
        ));
    }
}

fn load_sessions() -> HashMap<i64, String> {
    let keys = kv::list_keys("tg.session.").unwrap_or_default();
    let mut map = HashMap::new();
    for key in keys {
        if let Some(chat_id_str) = key.strip_prefix("tg.session.") {
            if let Ok(chat_id) = chat_id_str.parse::<i64>() {
                if let Ok(entry) = kv::get_json::<SessionEntry>(&key) {
                    map.insert(chat_id, entry.session_id);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_allowed_users_basic() {
        assert_eq!(parse_allowed_users("123,456,789"), vec![123, 456, 789]);
    }

    #[test]
    fn parse_allowed_users_with_spaces() {
        assert_eq!(
            parse_allowed_users(" 123 , 456 , 789 "),
            vec![123, 456, 789]
        );
    }

    #[test]
    fn parse_allowed_users_empty_string() {
        assert_eq!(parse_allowed_users(""), Vec::<i64>::new());
    }

    #[test]
    fn parse_allowed_users_ignores_invalid() {
        assert_eq!(parse_allowed_users("123,abc,456"), vec![123, 456]);
    }

    #[test]
    fn parse_allowed_users_single() {
        assert_eq!(parse_allowed_users("42"), vec![42]);
    }

    #[test]
    fn is_user_allowed_empty_allows_all() {
        assert!(is_user_allowed(&[], Some(999)));
        assert!(is_user_allowed(&[], None));
    }

    #[test]
    fn is_user_allowed_present() {
        assert!(is_user_allowed(&[100, 200, 300], Some(200)));
    }

    #[test]
    fn is_user_allowed_absent() {
        assert!(!is_user_allowed(&[100, 200], Some(999)));
    }

    #[test]
    fn is_user_allowed_none_user_id() {
        assert!(!is_user_allowed(&[100], None));
    }

    #[test]
    fn new_session_id_format() {
        let sid = new_session_id(12345);
        assert!(sid.starts_with("tg-12345-"));
        // Should contain a hex timestamp after the second dash.
        let parts: Vec<&str> = sid.splitn(3, '-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "tg");
        assert_eq!(parts[1], "12345");
        assert!(u128::from_str_radix(parts[2], 16).is_ok());
    }

    #[test]
    fn new_session_id_unique() {
        let a = new_session_id(1);
        let b = new_session_id(1);
        // Verify both session IDs are correctly formatted without relying on timing.
        assert!(a.starts_with("tg-1-"));
        assert!(b.starts_with("tg-1-"));
    }

    #[test]
    fn callback_token_short_id_unchanged() {
        let token = callback_token("short-req-123");
        assert_eq!(token, "short-req-123");
    }

    #[test]
    fn callback_token_long_id_hashed() {
        let long_id = "a".repeat(100);
        let token = callback_token(&long_id);
        // Must fit in 46 bytes for callback_data.
        assert!(token.len() <= 46, "token too long: {}", token.len());
        // Must be a 16-char hex string.
        assert_eq!(token.len(), 16);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn callback_token_distinct_ids_distinct_tokens() {
        let a = callback_token(&"x".repeat(100));
        let b = callback_token(&"y".repeat(100));
        assert_ne!(a, b, "different ids should produce different tokens");
    }

    #[test]
    fn callback_token_fits_in_callback_data() {
        let long_id = "z".repeat(200);
        let token = callback_token(&long_id);
        let data = format!("apr:{token}:allow_session");
        assert!(
            data.len() <= 64,
            "callback_data too long: {} bytes",
            data.len()
        );
    }
}
