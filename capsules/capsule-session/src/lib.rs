#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

//! Session capsule for Unicity AOS.
//!
//! Dumb, trustworthy store for conversation history. Holds clean messages:
//! what the user said, what the assistant replied, what tools returned.
//! Never transforms anything. Clean in, clean out.
//!
//! The react loop (or any future replacement) appends messages at turn
//! boundaries and fetches history when building LLM requests. Prompt
//! builder injections, system prompt assembly, context compaction -
//! those are ephemeral per-turn transforms that never touch session.
//!
//! # Session chaining
//!
//! Sessions form a linked list via `parent_session_id`. When a session
//! is cleared or compacted, a new session is created pointing back to
//! the old one. History is never silently truncated.
//!
//! # Thread management
//!
//! Beyond the raw append/fetch path, the capsule exposes thread-management
//! verbs (`get_meta` / `update` / `delete` / `search`) plus agent-callable
//! introspection tools (`list_threads` / `get_thread` / `search_conversations`)
//! and an operator `session` CLI command. Every surface shares one set of
//! internal operations (`do_list` / `do_get_meta` / `do_update` / `do_delete`
//! / `do_search`) so the logic is written once and the wire/agent/operator
//! views never drift.
//!
//! All operations run under the invoking principal: the kernel scopes this
//! capsule's KV namespace per principal, so there is no cross-principal
//! enumeration, read, mutation, or delete path. Mutations fan out a
//! `session.v1.event.<kind>` notification, stamped by the bus with the acting
//! principal, so other devices update their thread list live.

use astrid_sdk::prelude::*;
use astrid_sdk::schemars::{self, JsonSchema};
use astrid_sdk::types::{ContentPart, Message, MessageContent, MessageRole};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// KV key prefix for session data.
const SESSION_KEY_PREFIX: &str = "session.data";

/// Default session ID.
const DEFAULT_SESSION_ID: &str = "default";

/// Current schema version for `SessionData`.
///
/// - v0: pre-versioning (only `messages`).
/// - v1: added `schema_version` + `parent_session_id`.
/// - v2: added `created_at` / `updated_at` timestamps, then `title` /
///   `archived` / `meta` thread-management fields (all additive,
///   `#[serde(default)]`, no version bump).
const SESSION_DATA_SCHEMA_VERSION: u32 = 2;

/// Default page size for `handle_list` when the request omits `limit`.
const DEFAULT_LIST_LIMIT: u32 = 50;

/// Hard cap on the `handle_list` page size, mirrored by the gateway. Bounds
/// the per-request KV reads (one blob load per listed session).
const MAX_LIST_LIMIT: u32 = 200;

/// Default number of search hits returned when a search request omits `limit`.
const DEFAULT_SEARCH_LIMIT: u32 = 20;

/// Hard cap on the number of search hits returned in one page.
const MAX_SEARCH_LIMIT: u32 = 100;

/// Maximum number of session keys scanned in a single `do_search` call before
/// the scan yields and hands the client a cursor to resume from. Bounds the
/// per-call work (one blob load per scanned key) regardless of store size.
const SEARCH_KEY_SCAN_BUDGET: u32 = 200;

/// Best-effort cap on the number of keys counted for the `total` field of a
/// list page. Beyond this the count is reported as absent rather than paying
/// an unbounded full-namespace scan.
const LIST_TOTAL_COUNT_CAP: usize = 1024;

/// Maximum length, in characters, of the session preview snippet.
const PREVIEW_MAX_CHARS: usize = 80;

/// Maximum length, in characters, of a search snippet excerpt.
const SNIPPET_MAX_CHARS: usize = 120;

/// Maximum size, in bytes, of the opaque client `meta` string. The capsule
/// never interprets `meta`, but bounds it so a client cannot use the store as
/// unbounded scratch space.
const META_MAX_BYTES: usize = 8192;

/// Maximum number of CAS retry attempts before giving up.
///
/// Concurrent writers to the same session ID race on `kv::cas`. Eight
/// retries is generous for the realistic concurrency level (a single
/// react loop per session) and bounds worst-case latency under
/// adversarial contention.
const CAS_RETRY_LIMIT: u32 = 8;

/// Build the KV key for a session's data.
fn session_key(session_id: &str) -> String {
    format!("{SESSION_KEY_PREFIX}.{session_id}")
}

/// The KV key prefix, including the trailing separator, shared by every
/// session data key. Used to enumerate the principal's sessions via
/// [`kv::list_keys_page`].
fn session_key_prefix() -> String {
    format!("{SESSION_KEY_PREFIX}.")
}

/// Current wall-clock time as Unix epoch seconds, or `None` if the host
/// clock is unavailable. Timestamps are best-effort: a missing clock leaves
/// them unset rather than failing the write.
fn now_unix() -> Option<u64> {
    astrid_sdk::time::now()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// Extract the displayable text of a single message, if any. Plain text
/// returns directly; a multi-part message returns its first text part; tool
/// call / tool result messages carry no user-facing prose and return `None`.
fn message_text(message: &Message) -> Option<&str> {
    match &message.content {
        MessageContent::Text(s) => Some(s.as_str()),
        MessageContent::MultiPart(parts) => parts.iter().find_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        }),
        MessageContent::ToolCalls(_) | MessageContent::ToolResult(_) => None,
    }
}

/// Derive a short preview from the first user message in `messages`,
/// truncated to [`PREVIEW_MAX_CHARS`] characters. Returns `None` when there
/// is no user message carrying extractable text (e.g. only tool traffic).
fn session_preview(messages: &[Message]) -> Option<String> {
    let first_user = messages.iter().find(|m| m.role == MessageRole::User)?;
    let text = message_text(first_user)?;
    Some(truncate_chars(text.trim(), PREVIEW_MAX_CHARS))
}

/// Derive a preview from the LAST message carrying extractable text (any
/// role), truncated to [`PREVIEW_MAX_CHARS`] characters. This is the
/// recent-activity snippet a thread-list row shows. Returns `None` when no
/// trailing message carries text (e.g. the conversation ends on a tool call).
fn session_last_message_preview(messages: &[Message]) -> Option<String> {
    let text = messages.iter().rev().find_map(message_text)?;
    Some(truncate_chars(text.trim(), PREVIEW_MAX_CHARS))
}

/// Truncate `s` to at most `max` characters on a char boundary, appending an
/// ellipsis if any characters were dropped.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Build the JSON metadata summary for one session (no transcript body).
///
/// Shape is the frozen `session-summary` wire contract the gateway mirrors:
/// `session_id`, `title`, `preview`, `last_message_preview`, `message_count`,
/// `created_at`, `updated_at`, `archived`, `parent_session_id`, `meta`.
fn session_summary_json(session_id: &str, data: &SessionData) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "title": data.title,
        "preview": session_preview(&data.messages),
        "last_message_preview": session_last_message_preview(&data.messages),
        "message_count": data.messages.len(),
        "created_at": data.created_at,
        "updated_at": data.updated_at,
        "archived": data.archived,
        "parent_session_id": data.parent_session_id,
        "meta": data.meta,
    })
}

/// A patch for one optional string attribute, carrying presence so the
/// distinction between "leave unchanged" and "clear" survives.
///
/// Derived from the raw request JSON by [`string_patch_from`]: an absent key
/// is [`Patch::Keep`], a present empty string clears the field
/// (`Set(None)`), and a present non-empty string sets it (`Set(Some(_))`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Patch<T> {
    /// Field absent from the request: leave the stored value unchanged.
    Keep,
    /// Field present in the request: replace the stored value with this.
    Set(T),
}

/// Derive a string-attribute [`Patch`] from a raw request payload by key
/// presence: absent key → [`Patch::Keep`], present `""` → `Set(None)` (clear),
/// present non-empty → `Set(Some(_))`.
fn string_patch_from(payload: &serde_json::Value, key: &str) -> Patch<Option<String>> {
    match payload.get(key).and_then(|v| v.as_str()) {
        None => Patch::Keep,
        Some("") => Patch::Set(None),
        Some(s) => Patch::Set(Some(s.to_string())),
    }
}

/// Derive a boolean-attribute [`Patch`] from a raw request payload by key
/// presence: absent key → [`Patch::Keep`], present bool → `Set(b)`.
fn bool_patch_from(payload: &serde_json::Value, key: &str) -> Patch<bool> {
    match payload.get(key).and_then(serde_json::Value::as_bool) {
        None => Patch::Keep,
        Some(b) => Patch::Set(b),
    }
}

/// Apply the update patches to `data` in place (pure logic, no I/O). Returns
/// an error if the new `meta` exceeds [`META_MAX_BYTES`]. Does **not** stamp
/// timestamps — the caller bumps `updated_at` after a successful patch.
fn apply_update_patch(
    data: &mut SessionData,
    title: &Patch<Option<String>>,
    archived: &Patch<bool>,
    meta: &Patch<Option<String>>,
) -> Result<(), SysError> {
    // Validate the meta size bound before mutating anything so a rejected
    // oversize patch leaves the session untouched.
    if let Patch::Set(Some(new_meta)) = meta
        && new_meta.len() > META_MAX_BYTES
    {
        return Err(SysError::ApiError(format!(
            "session meta exceeds {META_MAX_BYTES} bytes (got {})",
            new_meta.len()
        )));
    }

    if let Patch::Set(t) = title {
        data.title = t.clone();
    }
    if let Patch::Set(a) = archived {
        data.archived = *a;
    }
    if let Patch::Set(m) = meta {
        data.meta = m.clone();
    }
    Ok(())
}

/// The kind of lifecycle change a `session.v1.event.<kind>` reports.
#[derive(Debug, Clone, Copy)]
enum SessionEventKind {
    /// A new thread was created (e.g. via `clear`).
    Created,
    /// A thread's metadata changed (title / archived / meta).
    Updated,
    /// A thread was hard-deleted.
    Deleted,
}

impl SessionEventKind {
    /// The wire token used both for the `kind` field and the topic suffix.
    fn as_str(self) -> &'static str {
        match self {
            SessionEventKind::Created => "created",
            SessionEventKind::Updated => "updated",
            SessionEventKind::Deleted => "deleted",
        }
    }
}

/// Publish a `session.v1.event.<kind>` lifecycle notification.
///
/// Fire-and-forget, no correlation: published during the invocation so the bus
/// stamps the acting principal, scoping a per-principal subscriber's live feed
/// to its own threads. Best-effort — a failed publish is logged, never a hard
/// error, so a notification failure cannot wedge the mutation that produced it.
fn publish_session_event(kind: SessionEventKind, session_id: &str, summary: serde_json::Value) {
    let topic = format!("session.v1.event.{}", kind.as_str());
    let payload = serde_json::json!({
        "kind": kind.as_str(),
        "session_id": session_id,
        "summary": summary,
    });
    if let Err(e) = ipc::publish_json(&topic, &payload) {
        log::warn(format!(
            "failed to publish session.v1.event.{} for '{session_id}': {e}",
            kind.as_str()
        ));
    }
}

/// Search one session's messages for a lowercase `query` substring (pure
/// logic, no I/O). Returns `None` if no message matches, otherwise the match
/// count and a truncated snippet excerpt around the FIRST match.
///
/// `query_lower` must already be lowercased by the caller (done once per
/// search, not once per message).
fn search_messages(messages: &[Message], query_lower: &str) -> Option<(u32, Option<String>)> {
    let mut match_count: u32 = 0;
    let mut snippet: Option<String> = None;

    for message in messages {
        let Some(text) = message_text(message) else {
            continue;
        };
        let text_lower = text.to_lowercase();
        if let Some(pos) = text_lower.find(query_lower) {
            match_count = match_count.saturating_add(1);
            if snippet.is_none() {
                snippet = Some(snippet_around(text, &text_lower, pos));
            }
        }
    }

    if match_count == 0 {
        None
    } else {
        Some((match_count, snippet))
    }
}

/// Build a truncated excerpt around a match. `byte_pos` is the byte offset of
/// the match within `text_lower` (which is `text.to_lowercase()`); ASCII-safe
/// for the common case, and lowercasing preserves byte length for ASCII. To
/// stay correct for non-ASCII we anchor on the original `text` by character
/// position derived from the lowercased prefix length, then take a window of
/// [`SNIPPET_MAX_CHARS`] characters centred on the match.
fn snippet_around(text: &str, text_lower: &str, byte_pos: usize) -> String {
    // Character index of the match start within the lowercased text. Lowercase
    // can change byte length per codepoint but not the codepoint *count* for
    // the casing pairs we care about, so the char index transfers to `text`.
    let char_pos = text_lower[..byte_pos].chars().count();
    let total_chars = text.chars().count();

    // Centre a SNIPPET_MAX_CHARS window on the match.
    let half = SNIPPET_MAX_CHARS / 2;
    let start = char_pos.saturating_sub(half);
    let end = (start + SNIPPET_MAX_CHARS).min(total_chars);
    let start = end.saturating_sub(SNIPPET_MAX_CHARS);

    let excerpt: String = text.chars().skip(start).take(end - start).collect();
    let excerpt = excerpt.trim().to_string();

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(&excerpt);
    if end < total_chars {
        out.push('…');
    }
    out
}

/// Persistent conversation session data.
///
/// Schema-versioned for forward-compatible deserialization. On load
/// ([`SessionData::load`]) the blob self-heals to the current schema:
/// - v0 (legacy, no version field) and v1 (no timestamps): stamp to the
///   current version and re-save via best-effort CAS. Timestamps stay `None`
///   (genuinely unknown for pre-v2 data) and populate on the next write.
/// - current version: use as-is.
/// - Unknown future version: log error, start fresh (fail secure).
///
/// The thread-management fields (`title` / `archived` / `meta`) are additive
/// within schema v2 — they carry `#[serde(default)]` so a pre-management v2
/// blob loads with them defaulted, no version bump required.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionData {
    /// Schema version. Defaults to 0 for pre-versioning data.
    #[serde(default)]
    schema_version: u32,
    /// Previous session in the chain, if this session was created
    /// via clear or compaction.
    #[serde(default)]
    parent_session_id: Option<String>,
    /// Unix epoch seconds when this session was first written. `None` for
    /// pre-v2 sessions (genuinely unknown); populated on the next write.
    #[serde(default)]
    created_at: Option<u64>,
    /// Unix epoch seconds of the most recent write (append or clear). `None`
    /// for pre-v2 sessions until their next write.
    #[serde(default)]
    updated_at: Option<u64>,
    /// Operator/user-set thread title. `None` until set via update; the
    /// display-name fallback chain is `title` → `preview` → session id.
    #[serde(default)]
    title: Option<String>,
    /// Whether the thread is archived (hidden from the default list, data
    /// retained). Toggled via update.
    #[serde(default)]
    archived: bool,
    /// Opaque client-defined JSON (tags, pin, per-device read-state, …),
    /// stored verbatim and never interpreted, size-bounded by
    /// [`META_MAX_BYTES`]. The forward-compat escape hatch for per-thread
    /// attributes.
    #[serde(default)]
    meta: Option<String>,
    /// Clean conversation message history.
    messages: Vec<Message>,
}

impl Default for SessionData {
    fn default() -> Self {
        Self {
            schema_version: SESSION_DATA_SCHEMA_VERSION,
            parent_session_id: None,
            created_at: None,
            updated_at: None,
            title: None,
            archived: false,
            meta: None,
            messages: Vec::new(),
        }
    }
}

impl SessionData {
    /// Apply schema migration to deserialized data (pure logic, no I/O).
    ///
    /// Returns `Ok((data, needs_save))` if migration succeeded or was
    /// unnecessary. `needs_save` is true when the version was bumped.
    /// Returns `Err(fresh_default)` if the version is unrecognized
    /// (fail secure). The error is boxed: `SessionData` carries a transcript
    /// `Vec` and is large, so a bare `Result<_, Self>` trips
    /// `clippy::result_large_err`.
    fn migrate(mut self) -> Result<(Self, bool), Box<Self>> {
        match self.schema_version {
            // v0 (pre-versioning) and v1 (pre-timestamps) both upgrade to the
            // current version by stamping it. Timestamps are left None —
            // genuinely unknown for pre-v2 data — and populate on next write.
            // The thread-management fields default in via serde.
            v if v < SESSION_DATA_SCHEMA_VERSION => {
                self.schema_version = SESSION_DATA_SCHEMA_VERSION;
                Ok((self, true))
            }
            v if v == SESSION_DATA_SCHEMA_VERSION => Ok((self, false)),
            _ => Err(Box::new(Self::default())),
        }
    }

    /// Stamp timestamps for a write at `now` (Unix epoch seconds): sets
    /// `created_at` on the first write to this session and always refreshes
    /// `updated_at`.
    fn touch(&mut self, now: u64) {
        self.created_at.get_or_insert(now);
        self.updated_at = Some(now);
    }

    /// Load session data from KV, applying schema migration as needed.
    ///
    /// Returns `(data, raw_bytes)` where `raw_bytes` is the exact KV
    /// payload that was read (or `None` for a missing key). Callers use
    /// `raw_bytes` as the `expected` value in [`kv::cas`] to detect
    /// concurrent writers. Returns `Self::default()` with `raw_bytes =
    /// None` if the key is absent.
    fn load(session_id: &str) -> (Self, Option<Vec<u8>>) {
        let key = session_key(session_id);
        let raw = match kv::get_bytes_opt(&key) {
            Ok(b) => b,
            Err(e) => {
                log::error(format!("Failed to load session bytes, starting fresh: {e}"));
                return (Self::default(), None);
            }
        };

        let Some(bytes) = raw else {
            // Key is absent: fresh default with no expected bytes (CAS
            // will use `expected = None` for create-if-absent semantics).
            return (Self::default(), None);
        };

        let data = serde_json::from_slice::<Self>(&bytes).unwrap_or_else(|e| {
            log::error(format!("Failed to parse session data, starting fresh: {e}"));
            Self::default()
        });

        match data.migrate() {
            Ok((migrated, needs_save)) => {
                // If version was bumped (v0 -> v1), persist the migration
                // via CAS so we don't clobber a concurrent writer. Best-
                // effort: in-memory data is usable either way, and the
                // next modification will retry the migration if this one
                // races out.
                if needs_save {
                    let migrated_bytes = match serde_json::to_vec(&migrated) {
                        Ok(b) => b,
                        Err(e) => {
                            log::warn(format!("Failed to serialize migrated session: {e}"));
                            return (migrated, Some(bytes));
                        }
                    };
                    match kv::cas(&key, Some(&bytes), &migrated_bytes) {
                        Ok(true) => (migrated, Some(migrated_bytes)),
                        Ok(false) => {
                            // Lost the race to another writer; their value
                            // is now authoritative. Caller will pick it up
                            // on its own retry loop.
                            log::debug(format!(
                                "session '{session_id}' migration CAS lost \
                                 race; another writer migrated first"
                            ));
                            (migrated, Some(bytes))
                        }
                        Err(e) => {
                            log::warn(format!("Failed to CAS-save session after migration: {e}"));
                            (migrated, Some(bytes))
                        }
                    }
                } else {
                    (migrated, Some(bytes))
                }
            }
            Err(fresh) => {
                log::error(format!(
                    "Session '{session_id}' has unknown schema version \
                         (expected {SESSION_DATA_SCHEMA_VERSION}), starting fresh"
                ));
                (*fresh, Some(bytes))
            }
        }
    }

    /// Atomically read-modify-write session data using [`kv::cas`].
    ///
    /// Retries up to [`CAS_RETRY_LIMIT`] times if a concurrent writer
    /// wins the swap. The `mutate` closure is called fresh on every
    /// retry so logic that derives state (e.g. truncation, dedup) sees
    /// the current stored value. Returns the post-write [`SessionData`]
    /// so callers that need the merged history (e.g. append-before-read)
    /// avoid a second `load`.
    fn modify_atomic<F>(session_id: &str, mut mutate: F) -> Result<Self, SysError>
    where
        F: FnMut(&mut Self),
    {
        let key = session_key(session_id);

        for attempt in 0..CAS_RETRY_LIMIT {
            let (mut data, expected) = Self::load(session_id);
            mutate(&mut data);
            let new_bytes = serde_json::to_vec(&data)?;
            let expected_slice = expected.as_deref();

            if kv::cas(&key, expected_slice, &new_bytes)? {
                return Ok(data);
            }

            log::debug(format!(
                "session '{session_id}' CAS attempt {} lost race; retrying",
                attempt + 1
            ));
        }

        Err(SysError::ApiError(format!(
            "session '{session_id}' write contended for {CAS_RETRY_LIMIT} attempts; \
             giving up to avoid unbounded retry"
        )))
    }

    /// Atomically read-modify-write only when the session already exists.
    ///
    /// Like [`modify_atomic`](Self::modify_atomic) but never creates a new
    /// key: if the session is absent (no stored bytes), returns `Ok(None)`
    /// without writing. Used by `do_update` so an update to a non-existent
    /// thread is a no-op, never a stealth create. Returns the post-write
    /// data on success.
    fn modify_existing<F>(session_id: &str, mut mutate: F) -> Result<Option<Self>, SysError>
    where
        F: FnMut(&mut Self) -> Result<(), SysError>,
    {
        let key = session_key(session_id);

        for attempt in 0..CAS_RETRY_LIMIT {
            let (mut data, expected) = Self::load(session_id);
            // Absent key: `load` returns a default with `expected == None`.
            // Refuse to create — updates only touch existing threads.
            let Some(expected_bytes) = expected else {
                return Ok(None);
            };

            mutate(&mut data)?;
            let new_bytes = serde_json::to_vec(&data)?;

            if kv::cas(&key, Some(&expected_bytes), &new_bytes)? {
                return Ok(Some(data));
            }

            log::debug(format!(
                "session '{session_id}' update CAS attempt {} lost race; retrying",
                attempt + 1
            ));
        }

        Err(SysError::ApiError(format!(
            "session '{session_id}' update contended for {CAS_RETRY_LIMIT} attempts; \
             giving up to avoid unbounded retry"
        )))
    }
}

// ---------------------------------------------------------------------------
// Shared internal operations
//
// Every surface (IPC verbs, agent #[tool]s, and the operator CLI command)
// routes through these functions so the logic is written exactly once.
// ---------------------------------------------------------------------------

/// One page of session summaries: `(summaries, next_cursor, total)`. `total`
/// is the principal's thread count when cheaply known, else `None`.
type ListPage = (Vec<serde_json::Value>, Option<String>, Option<u32>);

/// Whether a session is visible given the caller's archive preference: active
/// threads always show; archived threads only when `include_archived`.
fn is_visible(archived: bool, include_archived: bool) -> bool {
    !archived || include_archived
}

/// Clamp a requested list `limit` into `[1, MAX_LIST_LIMIT]`, defaulting to
/// [`DEFAULT_LIST_LIMIT`] when absent.
fn clamp_list_limit(limit: Option<u32>) -> u32 {
    limit
        .map(|l| l.clamp(1, MAX_LIST_LIMIT))
        .unwrap_or(DEFAULT_LIST_LIMIT)
}

/// Clamp a requested search `limit` into `[1, MAX_SEARCH_LIMIT]`, defaulting
/// to [`DEFAULT_SEARCH_LIMIT`] when absent.
fn clamp_search_limit(limit: Option<u32>) -> u32 {
    limit
        .map(|l| l.clamp(1, MAX_SEARCH_LIMIT))
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
}

/// Enumerate the principal's sessions and build a page of metadata summaries.
///
/// Returns `(summaries, next_cursor, total)`. `total` is the principal's
/// thread count when cheaply known (`<= LIST_TOTAL_COUNT_CAP`), else `None`.
/// Archived threads are filtered out unless `include_archived`.
fn do_list(include_archived: bool, cursor: Option<&str>, limit: u32) -> Result<ListPage, SysError> {
    let prefix = session_key_prefix();
    let page = kv::list_keys_page(&prefix, cursor, limit)?;

    let mut sessions = Vec::with_capacity(page.keys.len());
    for key in &page.keys {
        // `list_keys_page` returns full KV keys; strip the prefix back to the
        // session id. A non-matching key can't occur (we queried by this
        // prefix) but is skipped defensively.
        let Some(session_id) = key.strip_prefix(&prefix) else {
            continue;
        };
        // `load` self-heals each blob's schema as a side effect.
        let (data, _) = SessionData::load(session_id);
        if !is_visible(data.archived, include_archived) {
            continue;
        }
        sessions.push(session_summary_json(session_id, &data));
    }

    // Best-effort total: count via the bounded `list_keys` (capped server-side
    // at 1024). If the namespace is larger than the cap the call errors with
    // `too-large` — report `None` rather than failing the list.
    let total = match kv::list_keys(&prefix) {
        Ok(keys) if keys.len() <= LIST_TOTAL_COUNT_CAP => u32::try_from(keys.len()).ok(),
        _ => None,
    };

    Ok((sessions, page.next_cursor, total))
}

/// Fetch one thread's metadata summary, or `None` if no such thread exists in
/// the caller's namespace. Never creates the key.
fn do_get_meta(session_id: &str) -> Result<Option<serde_json::Value>, SysError> {
    let key = session_key(session_id);
    // Distinguish "absent" from "empty": only build a summary for a key that
    // actually exists — `update`/`get_meta` must never materialise a thread.
    if kv::get_bytes_opt(&key)?.is_none() {
        return Ok(None);
    }
    let (data, _) = SessionData::load(session_id);
    Ok(Some(session_summary_json(session_id, &data)))
}

/// Patch a thread's mutable metadata (title / archived / meta), bump
/// `updated_at`, publish an `updated` lifecycle event, and return the updated
/// summary. Returns `Ok(None)` if the thread does not exist (no create, no
/// event). Rejects an oversize `meta` before any write.
fn do_update(
    session_id: &str,
    title: &Patch<Option<String>>,
    archived: &Patch<bool>,
    meta: &Patch<Option<String>>,
) -> Result<Option<serde_json::Value>, SysError> {
    // Fail fast on an oversize meta before touching KV: validate against a
    // throwaway copy so we never start a CAS loop we know will reject.
    apply_update_patch(&mut SessionData::default(), title, archived, meta)?;

    let now = now_unix();
    let updated = SessionData::modify_existing(session_id, |data| {
        apply_update_patch(data, title, archived, meta)?;
        if let Some(now) = now {
            data.touch(now);
        }
        Ok(())
    })?;

    let Some(data) = updated else {
        return Ok(None);
    };

    let summary = session_summary_json(session_id, &data);
    publish_session_event(SessionEventKind::Updated, session_id, summary.clone());
    Ok(Some(summary))
}

/// Hard-purge a thread (transcript and metadata) from the caller's namespace.
/// Returns `true` if a thread existed and was deleted, `false` if absent.
/// Publishes a `deleted` lifecycle event only when something was deleted.
fn do_delete(session_id: &str) -> Result<bool, SysError> {
    let key = session_key(session_id);
    // Check presence first so we report `deleted=false` for an absent key and
    // never emit a spurious event. `kv::delete` is idempotent on its own.
    if kv::get_bytes_opt(&key)?.is_none() {
        return Ok(false);
    }
    kv::delete(&key)?;
    publish_session_event(
        SessionEventKind::Deleted,
        session_id,
        serde_json::Value::Null,
    );
    Ok(true)
}

/// Search the caller's transcripts for a case-insensitive substring, bounded
/// per call.
///
/// Scans session keys from `cursor`, loading each and matching `query` against
/// every message's extractable text. Accumulates until `limit` hits OR
/// [`SEARCH_KEY_SCAN_BUDGET`] keys are scanned, then sets `next_cursor` to the
/// last-scanned key if more keys remain, else `None`. Archived threads are
/// skipped unless `include_archived`. An empty `query` matches nothing.
fn do_search(
    query: &str,
    include_archived: bool,
    cursor: Option<&str>,
    limit: u32,
) -> Result<(Vec<serde_json::Value>, Option<String>), SysError> {
    let query_lower = query.to_lowercase();
    if query_lower.is_empty() {
        return Ok((Vec::new(), None));
    }

    let prefix = session_key_prefix();
    let mut results = Vec::new();
    let mut scanned: u32 = 0;
    let mut page_cursor = cursor.map(str::to_string);

    // Walk KV pages until we hit the result limit or exhaust the key-scan
    // budget. Each page load is itself bounded; the budget bounds total work.
    // `break 'scan` carries the resume cursor: the last key scanned, so the
    // next call continues from there.
    let next_cursor = 'scan: loop {
        let page = kv::list_keys_page(&prefix, page_cursor.as_deref(), MAX_LIST_LIMIT)?;
        if page.keys.is_empty() {
            // No more keys: a complete scan, no further cursor.
            break 'scan None;
        }

        for key in &page.keys {
            let Some(session_id) = key.strip_prefix(&prefix) else {
                continue;
            };
            scanned = scanned.saturating_add(1);

            let (data, _) = SessionData::load(session_id);
            if is_visible(data.archived, include_archived)
                && let Some((match_count, snippet)) = search_messages(&data.messages, &query_lower)
            {
                results.push(serde_json::json!({
                    "session_id": session_id,
                    "title": data.title,
                    "snippet": snippet,
                    "match_count": match_count,
                    "updated_at": data.updated_at,
                }));
                if results.len() >= limit as usize {
                    // Hit the result limit. Resume from this key next call.
                    break 'scan Some(key.clone());
                }
            }

            if scanned >= SEARCH_KEY_SCAN_BUDGET {
                // Spent the per-call scan budget. Resume from this key.
                break 'scan Some(key.clone());
            }
        }

        match page.next_cursor {
            // More pages remain within this call's budget: continue scanning.
            Some(next) => page_cursor = Some(next),
            // Reached the end of the namespace: complete scan, no cursor.
            None => break 'scan None,
        }
    };

    Ok((results, next_cursor))
}

/// Tail the last `limit` messages of a transcript when a cap is requested,
/// else the whole transcript. Returns the messages as a JSON array. The tail
/// keeps the agent's `get_thread` bounded for context safety.
fn transcript_json(messages: &[Message], limit: Option<u32>) -> serde_json::Value {
    let slice: &[Message] = match limit {
        Some(n) => {
            let n = n as usize;
            let start = messages.len().saturating_sub(n);
            &messages[start..]
        }
        None => messages,
    };
    serde_json::json!(slice)
}

// ---------------------------------------------------------------------------
// Agent tool argument types
// ---------------------------------------------------------------------------

/// Arguments for the `list_threads` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListThreadsArgs {
    /// Include archived threads in the listing. Defaults to false (active
    /// threads only).
    #[serde(default)]
    pub include_archived: Option<bool>,
    /// Maximum number of threads to return. Defaults to 50; the host caps the
    /// effective value.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments for the `get_thread` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetThreadArgs {
    /// The thread (session) identifier to fetch the transcript of.
    pub session_id: String,
    /// If set, return only the most recent `limit` messages (context safety).
    /// Absent returns the full transcript.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments for the `search_conversations` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchConversationsArgs {
    /// Case-insensitive substring to search for across message text.
    pub query: String,
    /// Maximum number of hits to return. Defaults to 20; the host caps the
    /// effective value.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Include archived threads in the search. Defaults to false.
    #[serde(default)]
    pub include_archived: Option<bool>,
}

/// Session capsule. Dumb store with session chaining, thread management, and
/// agent-callable introspection.
///
/// # Security note
///
/// Session isolation (restricting which capsules can read/write which
/// session IDs) is enforced at the kernel's topic ACL layer plus the
/// per-principal KV namespace, not within this capsule. Every operation runs
/// under the invoking principal: there is no fixed/owner principal and no
/// cross-principal enumeration, read, mutation, or delete path.
#[derive(Default)]
pub struct Session;

#[capsule]
impl Session {
    /// Handles `session.append` events.
    ///
    /// Appends one or more messages to the conversation history.
    /// Fire-and-forget - no response published.
    ///
    /// The react capsule uses `append_before_read` on `get_messages` for
    /// atomic appends. This standalone handler exists as a public API for
    /// other capsules that need to inject messages without reading history
    /// (e.g. system notifications, external integrations).
    #[astrid::interceptor("handle_append")]
    pub fn handle_append(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let messages: Vec<Message> = payload
            .get("messages")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| SysError::ApiError(format!("Failed to parse messages: {e}")))?
            .unwrap_or_default();

        if messages.is_empty() {
            return Ok(());
        }

        // Atomic append: `kv::cas` guarantees we never clobber a
        // concurrent writer's appends. The closure is re-run on each
        // retry against the freshly-loaded value, so all messages from
        // both writers end up in the final list.
        let now = now_unix();
        SessionData::modify_atomic(session_id, |data| {
            data.messages.extend(messages.iter().cloned());
            if let Some(now) = now {
                data.touch(now);
            }
        })
        .map(|_| ())
    }

    /// Extracts and validates `correlation_id` from a request payload.
    ///
    /// The correlation_id is interpolated into per-request scoped reply
    /// topics as a single dot-separated segment. Rejects empty values and
    /// values containing dots (which would add extra segments, breaking
    /// the ACL pattern match).
    fn require_correlation_id<'a>(
        payload: &'a serde_json::Value,
        request_name: &str,
    ) -> Result<&'a str, SysError> {
        payload
            .get("correlation_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && !s.contains('.'))
            .ok_or_else(|| {
                SysError::ApiError(format!(
                    "{request_name} request missing or invalid correlation_id \
                     (must be non-empty, no dots)"
                ))
            })
    }

    /// Handles `session.request.get_messages` events.
    ///
    /// Returns the conversation history to the requester via a per-request
    /// scoped reply topic (`session.v1.response.get_messages.<correlation_id>`).
    /// This prevents cross-instance response theft under concurrent load.
    ///
    /// Supports an optional `append_before_read` field containing messages
    /// to append atomically before returning the history. This eliminates
    /// the race between a separate `session.append` and `get_messages`.
    #[astrid::interceptor("handle_get_messages")]
    pub fn handle_get_messages(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let correlation_id = Self::require_correlation_id(&payload, "get_messages")?;

        // Atomic append-before-read: if the requester provides messages to
        // append, store them first so the returned history includes them.
        // `kv::cas` makes the read-modify-write race-free against any
        // concurrent `handle_append` on the same session.
        let data = if let Some(append_msgs) = payload.get("append_before_read").cloned() {
            let msgs: Vec<Message> = serde_json::from_value(append_msgs).map_err(|e| {
                SysError::ApiError(format!("Failed to parse append_before_read: {e}"))
            })?;
            if msgs.is_empty() {
                SessionData::load(session_id).0
            } else {
                let now = now_unix();
                SessionData::modify_atomic(session_id, |data| {
                    data.messages.extend(msgs.iter().cloned());
                    if let Some(now) = now {
                        data.touch(now);
                    }
                })?
            }
        } else {
            SessionData::load(session_id).0
        };

        // correlation_id is redundant with the scoped topic but retained
        // in the payload for observability (log inspection, debugging).
        let reply_topic = format!("session.v1.response.get_messages.{correlation_id}");
        ipc::publish_json(
            &reply_topic,
            &serde_json::json!({
                "correlation_id": correlation_id,
                "messages": data.messages,
            }),
        )
    }

    /// Handles `session.v1.request.clear` events.
    ///
    /// Creates a new session with `parent_session_id` pointing to the
    /// old one. The old session's data is left intact in KV for history
    /// traversal. Returns the new session ID via a per-request scoped
    /// reply topic (`session.v1.response.clear.<correlation_id>`), and fans
    /// out a `created` lifecycle event for the new session.
    #[astrid::interceptor("handle_clear")]
    pub fn handle_clear(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let old_session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let correlation_id = Self::require_correlation_id(&payload, "clear")?;

        let new_session_id = Uuid::new_v4().to_string();

        let now = now_unix();
        let new_data = SessionData {
            schema_version: SESSION_DATA_SCHEMA_VERSION,
            parent_session_id: Some(old_session_id.to_string()),
            created_at: now,
            updated_at: now,
            title: None,
            archived: false,
            meta: None,
            messages: Vec::new(),
        };
        let new_bytes = serde_json::to_vec(&new_data)?;
        // Create-if-absent: a UUID v4 collision with an existing session
        // key is astronomically unlikely but still fail-secure rather
        // than silently overwrite. `cas(key, None, ...)` returns false
        // if the key already exists.
        let created = kv::cas(&session_key(&new_session_id), None, &new_bytes)?;
        if !created {
            return Err(SysError::ApiError(format!(
                "session '{new_session_id}' UUID collision detected; \
                 refusing to overwrite existing session"
            )));
        }

        log::info(format!(
            "Session cleared: '{old_session_id}' -> '{new_session_id}' \
                 (old session preserved)"
        ));

        // Fan out a `created` event for the brand-new session so other devices
        // see the fresh thread appear live.
        publish_session_event(
            SessionEventKind::Created,
            &new_session_id,
            session_summary_json(&new_session_id, &new_data),
        );

        let reply_topic = format!("session.v1.response.clear.{correlation_id}");
        ipc::publish_json(
            &reply_topic,
            &serde_json::json!({
                "correlation_id": correlation_id,
                "new_session_id": new_session_id,
                "old_session_id": old_session_id,
            }),
        )
    }

    /// Handles `session.v1.request.list` events.
    ///
    /// Enumerates the invoking principal's sessions and returns a paginated
    /// page of metadata summaries (id, title, previews, message count,
    /// timestamps, archived flag, parent, meta — no transcript bodies) via the
    /// per-request scoped reply topic `session.v1.response.list.<correlation_id>`.
    ///
    /// Pagination follows the KV key cursor: pages are ordered by session
    /// key, and `next_cursor` is the opaque cursor for the following page (or
    /// absent on the last page). Each summary carries `updated_at` so callers
    /// can present threads by recency. Archived threads are filtered out
    /// unless `include_archived` is true.
    ///
    /// # Per-principal scope
    ///
    /// The kernel scopes this capsule's KV namespace to the invoking
    /// principal, so [`kv::list_keys_page`] only ever returns the caller's
    /// own session keys. There is no cross-principal enumeration path: a
    /// caller cannot observe another principal's threads, even by id.
    #[astrid::interceptor("handle_list")]
    pub fn handle_list(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let correlation_id = Self::require_correlation_id(&payload, "list")?;

        let cursor = payload.get("cursor").and_then(|v| v.as_str());
        let limit = clamp_list_limit(
            payload
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .map(|l| u32::try_from(l.min(u64::from(u32::MAX))).unwrap_or(u32::MAX)),
        );
        let include_archived = payload
            .get("include_archived")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let (sessions, next_cursor, total) = do_list(include_archived, cursor, limit)?;

        let reply_topic = format!("session.v1.response.list.{correlation_id}");
        ipc::publish_json(
            &reply_topic,
            &serde_json::json!({
                "correlation_id": correlation_id,
                "sessions": sessions,
                "next_cursor": next_cursor,
                "total": total,
            }),
        )
    }

    /// Handles `session.v1.request.get_meta` events.
    ///
    /// Returns one thread's metadata summary (or `null` if no such thread in
    /// the caller's namespace — never creates the key) via the per-request
    /// scoped reply topic `session.v1.response.get_meta.<correlation_id>`.
    #[astrid::interceptor("handle_get_meta")]
    pub fn handle_get_meta(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let correlation_id = Self::require_correlation_id(&payload, "get_meta")?;
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let session = do_get_meta(session_id)?;

        let reply_topic = format!("session.v1.response.get_meta.{correlation_id}");
        ipc::publish_json(
            &reply_topic,
            &serde_json::json!({
                "correlation_id": correlation_id,
                "session": session,
            }),
        )
    }

    /// Handles `session.v1.request.update` events.
    ///
    /// Patches a thread's mutable metadata with PATCH-by-presence semantics:
    /// a field whose key is absent from the request is left unchanged; a
    /// present `title`/`meta` set to `""` clears it; a present `archived`
    /// bool sets it. An update to a non-existent thread writes nothing and
    /// replies `session: null` (no stealth create). Rejects an oversize
    /// `meta`. On success bumps `updated_at` and fans out an `updated` event.
    /// Replies on `session.v1.response.update.<correlation_id>`.
    #[astrid::interceptor("handle_update")]
    pub fn handle_update(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let correlation_id = Self::require_correlation_id(&payload, "update")?;
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        // Presence-by-key from the raw payload: absent = keep, present = set.
        let title = string_patch_from(&payload, "title");
        let meta = string_patch_from(&payload, "meta");
        let archived = bool_patch_from(&payload, "archived");

        let session = do_update(session_id, &title, &archived, &meta)?;

        let reply_topic = format!("session.v1.response.update.{correlation_id}");
        ipc::publish_json(
            &reply_topic,
            &serde_json::json!({
                "correlation_id": correlation_id,
                "session": session,
            }),
        )
    }

    /// Handles `session.v1.request.delete` events.
    ///
    /// Hard-purges a thread (transcript + metadata) from the caller's
    /// namespace, irreversibly. Replies `deleted: false` if the key was
    /// absent (no event). On a real deletion fans out a `deleted` event.
    /// Replies on `session.v1.response.delete.<correlation_id>`.
    #[astrid::interceptor("handle_delete")]
    pub fn handle_delete(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let correlation_id = Self::require_correlation_id(&payload, "delete")?;
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let deleted = do_delete(session_id)?;

        let reply_topic = format!("session.v1.response.delete.{correlation_id}");
        ipc::publish_json(
            &reply_topic,
            &serde_json::json!({
                "correlation_id": correlation_id,
                "deleted": deleted,
            }),
        )
    }

    /// Handles `session.v1.request.search` events.
    ///
    /// Case-insensitive substring search across the caller's transcripts,
    /// bounded per call (result limit and key-scan budget). Skips archived
    /// threads unless `include_archived`. Replies with the hits and an opaque
    /// `next_cursor` (absent when the scan completed) on
    /// `session.v1.response.search.<correlation_id>`.
    #[astrid::interceptor("handle_search")]
    pub fn handle_search(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let correlation_id = Self::require_correlation_id(&payload, "search")?;
        let query = payload.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let cursor = payload.get("cursor").and_then(|v| v.as_str());
        let include_archived = payload
            .get("include_archived")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let limit = clamp_search_limit(
            payload
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .map(|l| u32::try_from(l.min(u64::from(u32::MAX))).unwrap_or(u32::MAX)),
        );

        let (results, next_cursor) = do_search(query, include_archived, cursor, limit)?;

        let reply_topic = format!("session.v1.response.search.{correlation_id}");
        ipc::publish_json(
            &reply_topic,
            &serde_json::json!({
                "correlation_id": correlation_id,
                "results": results,
                "next_cursor": next_cursor,
            }),
        )
    }

    // -- Agent-callable introspection tools (read-only) --

    /// List your conversation threads with their metadata: id, title or
    /// auto-preview, message count, timestamps, and archived state. Use this
    /// to find a past conversation before reading or searching it. Read-only.
    #[astrid::tool("list_threads")]
    pub fn list_threads(&self, args: ListThreadsArgs) -> Result<serde_json::Value, SysError> {
        let include_archived = args.include_archived.unwrap_or(false);
        let limit = clamp_list_limit(args.limit.or(Some(DEFAULT_LIST_LIMIT)));
        let (sessions, next_cursor, _total) = do_list(include_archived, None, limit)?;
        Ok(serde_json::json!({
            "sessions": sessions,
            "next_cursor": next_cursor,
        }))
    }

    /// Read the transcript of one conversation thread by its session id. Pass
    /// `limit` to fetch only the most recent N messages — recommended for
    /// long threads to stay within your context window. Read-only.
    #[astrid::tool("get_thread")]
    pub fn get_thread(&self, args: GetThreadArgs) -> Result<serde_json::Value, SysError> {
        let (data, _) = SessionData::load(&args.session_id);
        Ok(serde_json::json!({
            "session_id": args.session_id,
            "messages": transcript_json(&data.messages, args.limit),
        }))
    }

    /// Search your past conversations for a word or phrase (case-insensitive
    /// substring match across message text). Returns matching threads with a
    /// snippet around the first hit. Read-only.
    #[astrid::tool("search_conversations")]
    pub fn search_conversations(
        &self,
        args: SearchConversationsArgs,
    ) -> Result<serde_json::Value, SysError> {
        let include_archived = args.include_archived.unwrap_or(false);
        let limit = clamp_search_limit(args.limit.or(Some(DEFAULT_SEARCH_LIMIT)));
        let (results, next_cursor) = do_search(&args.query, include_archived, None, limit)?;
        Ok(serde_json::json!({
            "results": results,
            "next_cursor": next_cursor,
        }))
    }

    /// Handles the operator `session` CLI command.
    ///
    /// Dispatches on the first whitespace-delimited token after the command
    /// name: `list`, `show`, `rename`, `archive`, `unarchive`, `delete`,
    /// `search`. Reuses the shared `do_*` operations, so the CLI view never
    /// diverges from the IPC verbs. Runs under the invoking principal (the
    /// local operator). Emits a formatted text response on `agent.v1.response`.
    #[astrid::interceptor("handle_command")]
    pub fn handle_command(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let session_id = payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SESSION_ID);

        let output = match Self::run_session_command(text) {
            Ok(s) => s,
            Err(e) => format!("Error: {e}"),
        };

        ipc::publish_json(
            "agent.v1.response",
            &serde_json::json!({
                "type": "agent_response",
                "text": output,
                "is_final": true,
                "session_id": session_id,
            }),
        )
    }

    /// Parse and run a `session <subcommand> …` command line, returning the
    /// formatted text output. Pure dispatch over the shared `do_*` ops; the
    /// host-call legwork lives in those.
    fn run_session_command(text: &str) -> Result<String, SysError> {
        let mut tokens = text.split_whitespace();
        // First token is the command name itself (`session`); skip it.
        let _command = tokens.next();
        let sub = tokens.next().unwrap_or("");
        let rest: Vec<&str> = tokens.collect();

        match sub {
            "list" | "" => Self::cmd_list(&rest),
            "show" => Self::cmd_show(&rest),
            "rename" => Self::cmd_rename(&rest),
            "archive" => Self::cmd_set_archived(&rest, true),
            "unarchive" => Self::cmd_set_archived(&rest, false),
            "delete" => Self::cmd_delete(&rest),
            "search" => Self::cmd_search(&rest),
            other => Ok(format!(
                "Unknown session subcommand '{other}'. \
                 Try: list, show, rename, archive, unarchive, delete, search."
            )),
        }
    }

    /// `session list [--all] [--limit N]`
    fn cmd_list(args: &[&str]) -> Result<String, SysError> {
        let (flags, _positional) = parse_args(args);
        let include_archived = flags.all;
        let limit = clamp_list_limit(flags.limit);

        let (sessions, _next_cursor, total) = do_list(include_archived, None, limit)?;
        if sessions.is_empty() {
            return Ok("No sessions.".to_string());
        }

        let mut out = String::new();
        if let Some(total) = total {
            out.push_str(&format!("{total} session(s):\n"));
        }
        for s in &sessions {
            let id = s["session_id"].as_str().unwrap_or("?");
            let name = s["title"]
                .as_str()
                .or_else(|| s["preview"].as_str())
                .unwrap_or("(no title)");
            let count = s["message_count"].as_u64().unwrap_or(0);
            let updated = s["updated_at"]
                .as_u64()
                .map(|u| u.to_string())
                .unwrap_or_else(|| "-".to_string());
            let archived = if s["archived"].as_bool().unwrap_or(false) {
                " [archived]"
            } else {
                ""
            };
            out.push_str(&format!(
                "{id}  {name}  ({count} msgs, updated {updated}){archived}\n"
            ));
        }
        Ok(out.trim_end().to_string())
    }

    /// `session show <id> [--limit N]`
    fn cmd_show(args: &[&str]) -> Result<String, SysError> {
        let (flags, positional) = parse_args(args);
        let Some(id) = positional.first() else {
            return Ok("Usage: session show <id> [--limit N]".to_string());
        };

        let (data, _) = SessionData::load(id);
        if data.messages.is_empty() {
            return Ok(format!(
                "Session '{id}' has no messages (or does not exist)."
            ));
        }

        let messages: &[Message] = match flags.limit {
            Some(n) => {
                let start = data.messages.len().saturating_sub(n as usize);
                &data.messages[start..]
            }
            None => &data.messages,
        };

        let mut out = String::new();
        for m in messages {
            let role = match m.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };
            let body = message_text(m).unwrap_or("(non-text content)");
            out.push_str(&format!("{role}: {body}\n"));
        }
        Ok(out.trim_end().to_string())
    }

    /// `session rename <id> <title…>`
    fn cmd_rename(args: &[&str]) -> Result<String, SysError> {
        let (_flags, positional) = parse_args(args);
        let Some((id, title_parts)) = positional.split_first() else {
            return Ok("Usage: session rename <id> <title…>".to_string());
        };
        if title_parts.is_empty() {
            return Ok("Usage: session rename <id> <title…>".to_string());
        }
        let title = title_parts.join(" ");

        let patched = do_update(
            id,
            &Patch::Set(Some(title.clone())),
            &Patch::Keep,
            &Patch::Keep,
        )?;
        match patched {
            Some(_) => Ok(format!("Renamed session '{id}' to: {title}")),
            None => Ok(format!("No such session: '{id}'")),
        }
    }

    /// `session archive <id>` / `session unarchive <id>`
    fn cmd_set_archived(args: &[&str], archived: bool) -> Result<String, SysError> {
        let (_flags, positional) = parse_args(args);
        let Some(id) = positional.first() else {
            let verb = if archived { "archive" } else { "unarchive" };
            return Ok(format!("Usage: session {verb} <id>"));
        };

        let patched = do_update(id, &Patch::Keep, &Patch::Set(archived), &Patch::Keep)?;
        match patched {
            Some(_) => {
                let verb = if archived { "Archived" } else { "Unarchived" };
                Ok(format!("{verb} session '{id}'"))
            }
            None => Ok(format!("No such session: '{id}'")),
        }
    }

    /// `session delete <id>`
    fn cmd_delete(args: &[&str]) -> Result<String, SysError> {
        let (_flags, positional) = parse_args(args);
        let Some(id) = positional.first() else {
            return Ok("Usage: session delete <id>".to_string());
        };

        let deleted = do_delete(id)?;
        if deleted {
            Ok(format!("Deleted session '{id}' (irreversible)."))
        } else {
            Ok(format!("No such session: '{id}'"))
        }
    }

    /// `session search <query…> [--all] [--limit N]`
    fn cmd_search(args: &[&str]) -> Result<String, SysError> {
        let (flags, positional) = parse_args(args);
        if positional.is_empty() {
            return Ok("Usage: session search <query…> [--all] [--limit N]".to_string());
        }
        let query = positional.join(" ");
        let include_archived = flags.all;
        let limit = clamp_search_limit(flags.limit);

        let (results, _next_cursor) = do_search(&query, include_archived, None, limit)?;
        if results.is_empty() {
            return Ok(format!("No matches for '{query}'."));
        }

        let mut out = format!("{} match(es) for '{query}':\n", results.len());
        for r in &results {
            let id = r["session_id"].as_str().unwrap_or("?");
            let name = r["title"].as_str().unwrap_or("(no title)");
            let count = r["match_count"].as_u64().unwrap_or(0);
            let snippet = r["snippet"].as_str().unwrap_or("");
            out.push_str(&format!("{id}  {name}  ({count} hits)  {snippet}\n"));
        }
        Ok(out.trim_end().to_string())
    }
}

/// Parsed CLI flags shared across `session` subcommands.
#[derive(Debug, Default)]
struct CommandFlags {
    /// `--all`: include archived threads.
    all: bool,
    /// `--limit N`: cap the result count.
    limit: Option<u32>,
}

/// Split raw argument tokens into recognised flags and positional arguments.
///
/// Recognises `--all` (boolean) and `--limit N` (consumes the next token as a
/// number). Unknown `--flags` and `--yes`-style confirmations are dropped (the
/// delete confirmation is handled by the uplink, not here). Everything else is
/// a positional argument, in order.
fn parse_args<'a>(args: &[&'a str]) -> (CommandFlags, Vec<&'a str>) {
    let mut flags = CommandFlags::default();
    let mut positional = Vec::new();
    let mut iter = args.iter().copied();

    while let Some(tok) = iter.next() {
        match tok {
            "--all" => flags.all = true,
            "--limit" => {
                if let Some(n) = iter.next() {
                    flags.limit = n.parse::<u32>().ok();
                }
            }
            // Drop confirmation / unknown flags; positional args carry meaning.
            "--yes" | "-y" => {}
            other if other.starts_with("--") => {}
            other => positional.push(other),
        }
    }

    (flags, positional)
}

// ---------------------------------------------------------------------------
// Tests (serde-level + pure logic, no host functions)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use astrid_sdk::types::ToolCall;

    /// Pre-versioning (v0) data with only `messages` deserializes correctly.
    /// The `schema_version` defaults to 0 and `parent_session_id` defaults to None.
    #[test]
    fn test_session_data_v0_backward_compat() {
        let v0_json = r#"{"messages":[]}"#;
        let data: SessionData = serde_json::from_str(v0_json).unwrap();
        assert_eq!(data.schema_version, 0);
        assert!(data.parent_session_id.is_none());
        assert!(data.messages.is_empty());
    }

    /// v1 data round-trips through serde (migration is applied by `load`,
    /// not by serde, so the version is preserved verbatim here).
    #[test]
    fn test_session_data_v1_round_trip() {
        let data = SessionData {
            schema_version: 1,
            parent_session_id: Some("old-session-abc".into()),
            created_at: None,
            updated_at: None,
            title: None,
            archived: false,
            meta: None,
            messages: Vec::new(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let loaded: SessionData = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.schema_version, 1);
        assert_eq!(loaded.parent_session_id.as_deref(), Some("old-session-abc"));
    }

    /// A v1-shaped blob (no timestamp fields) deserializes with
    /// `created_at`/`updated_at` defaulting to `None`.
    #[test]
    fn test_session_data_v1_json_defaults_timestamps_none() {
        let v1_json = r#"{"schema_version":1,"parent_session_id":null,"messages":[]}"#;
        let data: SessionData = serde_json::from_str(v1_json).unwrap();
        assert_eq!(data.schema_version, 1);
        assert!(data.created_at.is_none());
        assert!(data.updated_at.is_none());
    }

    /// A v1-shaped blob (no management fields) deserializes with
    /// `title`/`archived`/`meta` defaulting.
    #[test]
    fn test_session_data_v1_json_defaults_management_fields() {
        let v1_json = r#"{"schema_version":1,"parent_session_id":null,"messages":[]}"#;
        let data: SessionData = serde_json::from_str(v1_json).unwrap();
        assert!(data.title.is_none());
        assert!(!data.archived);
        assert!(data.meta.is_none());
    }

    /// v2 data with timestamps and management fields round-trips correctly.
    #[test]
    fn test_session_data_v2_round_trip() {
        let data = SessionData {
            schema_version: 2,
            parent_session_id: None,
            created_at: Some(1_719_000_000),
            updated_at: Some(1_719_000_100),
            title: Some("My thread".into()),
            archived: true,
            meta: Some("{\"pinned\":true}".into()),
            messages: Vec::new(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let loaded: SessionData = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.created_at, Some(1_719_000_000));
        assert_eq!(loaded.updated_at, Some(1_719_000_100));
        assert_eq!(loaded.title.as_deref(), Some("My thread"));
        assert!(loaded.archived);
        assert_eq!(loaded.meta.as_deref(), Some("{\"pinned\":true}"));
    }

    /// Unknown future version deserializes without error (serde doesn't
    /// know about version semantics - the `load()` function handles that).
    #[test]
    fn test_session_data_future_version_deserializes() {
        let future_json = r#"{"schema_version":99,"messages":[],"extra_field":"ignored"}"#;
        let data: SessionData = serde_json::from_str(future_json).unwrap();
        assert_eq!(data.schema_version, 99);
        assert!(data.messages.is_empty());
    }

    /// Default SessionData has the current schema version and defaulted fields.
    #[test]
    fn test_session_data_default_has_current_version() {
        let data = SessionData::default();
        assert_eq!(data.schema_version, SESSION_DATA_SCHEMA_VERSION);
        assert!(data.parent_session_id.is_none());
        assert!(data.title.is_none());
        assert!(!data.archived);
        assert!(data.meta.is_none());
    }

    /// v0 data migrates to current version and signals needs_save.
    #[test]
    fn test_migrate_v0_stamps_to_current() {
        let v0_json = r#"{"messages":[]}"#;
        let data: SessionData = serde_json::from_str(v0_json).unwrap();
        assert_eq!(data.schema_version, 0);

        let (migrated, needs_save) = data.migrate().expect("v0 should migrate successfully");
        assert_eq!(migrated.schema_version, SESSION_DATA_SCHEMA_VERSION);
        assert!(needs_save, "v0 -> v1 migration should signal needs_save");
        assert!(migrated.messages.is_empty());
    }

    /// Current version data passes through migrate unchanged.
    #[test]
    fn test_migrate_current_version_is_noop() {
        let data = SessionData::default();
        let (migrated, needs_save) = data.migrate().expect("current version should pass");
        assert_eq!(migrated.schema_version, SESSION_DATA_SCHEMA_VERSION);
        assert!(!needs_save, "current version should not signal needs_save");
    }

    /// Unknown future version fails migration (fail secure).
    #[test]
    fn test_migrate_unknown_version_fails_secure() {
        let data = SessionData {
            schema_version: 99,
            parent_session_id: Some("old".into()),
            created_at: Some(1),
            updated_at: Some(2),
            title: Some("t".into()),
            archived: true,
            meta: Some("m".into()),
            messages: Vec::new(),
        };
        let fresh = data
            .migrate()
            .expect_err("unknown version should fail migration");
        assert_eq!(fresh.schema_version, SESSION_DATA_SCHEMA_VERSION);
        assert!(
            fresh.parent_session_id.is_none(),
            "fresh default has no parent"
        );
        assert!(fresh.title.is_none(), "fresh default has no title");
        assert!(!fresh.archived, "fresh default is not archived");
    }

    /// v1 data (no timestamps/management fields) migrates to the current
    /// version, signals needs_save, and leaves the new fields defaulted.
    #[test]
    fn test_migrate_v1_to_current_defaults_new_fields() {
        let v1_json = r#"{"schema_version":1,"parent_session_id":null,"messages":[]}"#;
        let data: SessionData = serde_json::from_str(v1_json).unwrap();
        assert_eq!(data.schema_version, 1);

        let (migrated, needs_save) = data.migrate().expect("v1 should migrate");
        assert_eq!(migrated.schema_version, SESSION_DATA_SCHEMA_VERSION);
        assert!(
            needs_save,
            "v1 -> current migration should signal needs_save"
        );
        assert!(migrated.created_at.is_none());
        assert!(migrated.updated_at.is_none());
        assert!(migrated.title.is_none());
        assert!(!migrated.archived);
        assert!(migrated.meta.is_none());
    }

    /// `touch` stamps `created_at` once (first write) and always refreshes
    /// `updated_at`.
    #[test]
    fn test_touch_sets_created_once_updates_updated() {
        let mut data = SessionData::default();
        assert!(data.created_at.is_none());

        data.touch(100);
        assert_eq!(data.created_at, Some(100));
        assert_eq!(data.updated_at, Some(100));

        data.touch(200);
        assert_eq!(data.created_at, Some(100), "created_at must not move");
        assert_eq!(data.updated_at, Some(200), "updated_at tracks last write");
    }

    // -- preview extraction --

    #[test]
    fn test_preview_first_user_message() {
        let messages = vec![
            Message::system("you are helpful"),
            Message::user("what is the capital of France?"),
            Message::assistant("Paris."),
        ];
        assert_eq!(
            session_preview(&messages).as_deref(),
            Some("what is the capital of France?")
        );
    }

    #[test]
    fn test_preview_none_without_user_message() {
        let messages = vec![Message::system("sys"), Message::assistant("hi")];
        assert!(session_preview(&messages).is_none());
    }

    #[test]
    fn test_preview_empty_history() {
        assert!(session_preview(&[]).is_none());
    }

    #[test]
    fn test_preview_multipart_extracts_text() {
        let messages = vec![Message {
            role: MessageRole::User,
            content: MessageContent::MultiPart(vec![
                ContentPart::Image {
                    data: "base64".into(),
                    media_type: "image/png".into(),
                },
                ContentPart::Text {
                    text: "describe this".into(),
                },
            ]),
        }];
        assert_eq!(session_preview(&messages).as_deref(), Some("describe this"));
    }

    #[test]
    fn test_preview_truncates_long_text() {
        let long = "x".repeat(200);
        let messages = vec![Message::user(long)];
        let preview = session_preview(&messages).expect("preview present");
        // PREVIEW_MAX_CHARS chars plus a one-char ellipsis.
        assert_eq!(preview.chars().count(), PREVIEW_MAX_CHARS + 1);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn test_truncate_chars_is_char_boundary_safe() {
        // Multi-byte characters must not be split mid-codepoint.
        let s = "é".repeat(100);
        let out = truncate_chars(&s, 10);
        assert_eq!(out.chars().count(), 11); // 10 + ellipsis
        assert!(out.ends_with('…'));
    }

    // -- last-message preview --

    #[test]
    fn test_last_message_preview_picks_last_text() {
        let messages = vec![
            Message::user("first"),
            Message::assistant("second"),
            Message::user("third and last"),
        ];
        assert_eq!(
            session_last_message_preview(&messages).as_deref(),
            Some("third and last")
        );
    }

    #[test]
    fn test_last_message_preview_skips_trailing_tool_messages() {
        // A trailing tool-call message has no extractable text; the preview
        // should fall back to the last message that does.
        let messages = vec![
            Message::user("a question"),
            Message::assistant("an answer"),
            Message::assistant_with_tools(vec![ToolCall {
                id: "1".into(),
                name: "do".into(),
                arguments: serde_json::json!({}),
            }]),
        ];
        assert_eq!(
            session_last_message_preview(&messages).as_deref(),
            Some("an answer")
        );
    }

    #[test]
    fn test_last_message_preview_none_without_text() {
        let messages = vec![Message::assistant_with_tools(vec![ToolCall {
            id: "1".into(),
            name: "do".into(),
            arguments: serde_json::json!({}),
        }])];
        assert!(session_last_message_preview(&messages).is_none());
    }

    #[test]
    fn test_last_message_preview_empty_history() {
        assert!(session_last_message_preview(&[]).is_none());
    }

    // -- summary shape (frozen wire contract) --

    #[test]
    fn test_session_summary_json_shape() {
        let data = SessionData {
            schema_version: 2,
            parent_session_id: Some("parent-1".into()),
            created_at: Some(10),
            updated_at: Some(20),
            title: Some("Trip planning".into()),
            archived: false,
            meta: Some("{\"tag\":\"work\"}".into()),
            messages: vec![Message::user("hello"), Message::assistant("hi there")],
        };
        let summary = session_summary_json("sess-1", &data);
        assert_eq!(summary["session_id"], "sess-1");
        assert_eq!(summary["title"], "Trip planning");
        assert_eq!(summary["preview"], "hello");
        assert_eq!(summary["last_message_preview"], "hi there");
        assert_eq!(summary["message_count"], 2);
        assert_eq!(summary["created_at"], 10);
        assert_eq!(summary["updated_at"], 20);
        assert_eq!(summary["archived"], false);
        assert_eq!(summary["parent_session_id"], "parent-1");
        assert_eq!(summary["meta"], "{\"tag\":\"work\"}");
    }

    #[test]
    fn test_session_summary_json_nulls_for_unset() {
        let data = SessionData::default();
        let summary = session_summary_json("sess-x", &data);
        assert!(summary["title"].is_null());
        assert!(summary["preview"].is_null());
        assert!(summary["last_message_preview"].is_null());
        assert!(summary["created_at"].is_null());
        assert!(summary["updated_at"].is_null());
        assert!(summary["parent_session_id"].is_null());
        assert!(summary["meta"].is_null());
        assert_eq!(summary["archived"], false);
        assert_eq!(summary["message_count"], 0);
    }

    /// v0 data with existing messages preserves them through migration.
    #[test]
    fn test_migrate_v0_preserves_messages() {
        let v0_json = r#"{"messages":[{"role":"user","content":"hello"}]}"#;
        let data: SessionData = serde_json::from_str(v0_json).unwrap();
        assert_eq!(data.schema_version, 0);
        assert_eq!(data.messages.len(), 1);

        let (migrated, _) = data.migrate().expect("v0 should migrate");
        assert_eq!(migrated.messages.len(), 1);
    }

    /// v0 data with parent_session_id preserves it through migration.
    #[test]
    fn test_migrate_v0_preserves_parent() {
        let v0_json = r#"{"messages":[],"parent_session_id":"parent-abc"}"#;
        let data: SessionData = serde_json::from_str(v0_json).unwrap();
        let (migrated, _) = data.migrate().expect("v0 should migrate");
        assert_eq!(migrated.parent_session_id.as_deref(), Some("parent-abc"));
    }

    // -- PATCH presence semantics (update) --

    /// Absent fields leave the stored value unchanged.
    #[test]
    fn test_patch_absent_keeps() {
        let payload = serde_json::json!({"session_id": "s"});
        assert_eq!(string_patch_from(&payload, "title"), Patch::Keep);
        assert_eq!(string_patch_from(&payload, "meta"), Patch::Keep);
        assert_eq!(bool_patch_from(&payload, "archived"), Patch::Keep);

        let mut data = SessionData {
            title: Some("keep me".into()),
            archived: true,
            meta: Some("keep meta".into()),
            ..SessionData::default()
        };
        apply_update_patch(&mut data, &Patch::Keep, &Patch::Keep, &Patch::Keep).unwrap();
        assert_eq!(data.title.as_deref(), Some("keep me"));
        assert!(data.archived);
        assert_eq!(data.meta.as_deref(), Some("keep meta"));
    }

    /// A present non-empty string sets the field.
    #[test]
    fn test_patch_present_sets() {
        let payload = serde_json::json!({"title": "New", "meta": "m", "archived": true});
        assert_eq!(
            string_patch_from(&payload, "title"),
            Patch::Set(Some("New".to_string()))
        );
        assert_eq!(
            string_patch_from(&payload, "meta"),
            Patch::Set(Some("m".to_string()))
        );
        assert_eq!(bool_patch_from(&payload, "archived"), Patch::Set(true));

        let mut data = SessionData::default();
        apply_update_patch(
            &mut data,
            &string_patch_from(&payload, "title"),
            &bool_patch_from(&payload, "archived"),
            &string_patch_from(&payload, "meta"),
        )
        .unwrap();
        assert_eq!(data.title.as_deref(), Some("New"));
        assert!(data.archived);
        assert_eq!(data.meta.as_deref(), Some("m"));
    }

    /// A present empty string clears the field to `None`.
    #[test]
    fn test_patch_empty_string_clears() {
        let payload = serde_json::json!({"title": "", "meta": ""});
        assert_eq!(string_patch_from(&payload, "title"), Patch::Set(None));
        assert_eq!(string_patch_from(&payload, "meta"), Patch::Set(None));

        let mut data = SessionData {
            title: Some("old".into()),
            meta: Some("old meta".into()),
            ..SessionData::default()
        };
        apply_update_patch(
            &mut data,
            &Patch::Set(None),
            &Patch::Keep,
            &Patch::Set(None),
        )
        .unwrap();
        assert!(data.title.is_none());
        assert!(data.meta.is_none());
    }

    /// An oversize `meta` is rejected and the session is left untouched.
    #[test]
    fn test_patch_oversize_meta_rejected() {
        let big = "x".repeat(META_MAX_BYTES + 1);
        let mut data = SessionData {
            meta: Some("original".into()),
            ..SessionData::default()
        };
        let err = apply_update_patch(
            &mut data,
            &Patch::Keep,
            &Patch::Keep,
            &Patch::Set(Some(big)),
        )
        .expect_err("oversize meta should be rejected");
        assert!(matches!(err, SysError::ApiError(_)));
        // Session unchanged: the original meta survives.
        assert_eq!(data.meta.as_deref(), Some("original"));
    }

    /// A `meta` exactly at the bound is accepted.
    #[test]
    fn test_patch_meta_at_bound_accepted() {
        let exact = "y".repeat(META_MAX_BYTES);
        let mut data = SessionData::default();
        apply_update_patch(
            &mut data,
            &Patch::Keep,
            &Patch::Keep,
            &Patch::Set(Some(exact.clone())),
        )
        .expect("meta at the bound should be accepted");
        assert_eq!(data.meta, Some(exact));
    }

    /// A non-boolean `archived` value is treated as absent (Keep), not an
    /// error — defensive against malformed clients.
    #[test]
    fn test_bool_patch_non_bool_is_keep() {
        let payload = serde_json::json!({"archived": "yes"});
        assert_eq!(bool_patch_from(&payload, "archived"), Patch::Keep);
    }

    // -- search matching (pure) --

    #[test]
    fn test_search_matches_case_insensitive() {
        let messages = vec![
            Message::user("The Quick Brown Fox"),
            Message::assistant("jumps over the lazy dog"),
        ];
        let (count, snippet) = search_messages(&messages, "quick").expect("should match");
        assert_eq!(count, 1);
        let snippet = snippet.expect("snippet present");
        assert!(snippet.to_lowercase().contains("quick"));
    }

    #[test]
    fn test_search_counts_multiple_matching_messages() {
        let messages = vec![
            Message::user("alpha beta"),
            Message::assistant("beta gamma"),
            Message::user("delta"),
        ];
        let (count, _) = search_messages(&messages, "beta").expect("should match");
        assert_eq!(count, 2, "two messages contain 'beta'");
    }

    #[test]
    fn test_search_no_match_returns_none() {
        let messages = vec![Message::user("hello world")];
        assert!(search_messages(&messages, "absent").is_none());
    }

    #[test]
    fn test_search_skips_tool_only_messages() {
        let messages = vec![Message::assistant_with_tools(vec![ToolCall {
            id: "1".into(),
            name: "secret".into(),
            arguments: serde_json::json!({"q": "secret"}),
        }])];
        // Tool-call text is not extractable, so 'secret' in the args must not
        // match — search is over user-facing message text only.
        assert!(search_messages(&messages, "secret").is_none());
    }

    #[test]
    fn test_search_snippet_includes_match_and_truncates() {
        let long = format!("{}NEEDLE{}", "a".repeat(300), "b".repeat(300));
        let messages = vec![Message::user(long)];
        let (count, snippet) = search_messages(&messages, "needle").expect("match");
        assert_eq!(count, 1);
        let snippet = snippet.expect("snippet");
        assert!(
            snippet.to_lowercase().contains("needle"),
            "snippet should contain the match: {snippet}"
        );
        // Excerpt window plus up to two ellipsis markers.
        assert!(
            snippet.chars().count() <= SNIPPET_MAX_CHARS + 2,
            "snippet too long: {} chars",
            snippet.chars().count()
        );
    }

    // -- transcript tail (get_thread) --

    #[test]
    fn test_transcript_tail_returns_last_n() {
        let messages: Vec<Message> = (0..10).map(|i| Message::user(format!("m{i}"))).collect();
        let tail = transcript_json(&messages, Some(3));
        let arr = tail.as_array().expect("array");
        assert_eq!(arr.len(), 3);
        // Last three are m7, m8, m9.
        assert_eq!(arr[0]["content"], "m7");
        assert_eq!(arr[2]["content"], "m9");
    }

    #[test]
    fn test_transcript_tail_limit_exceeds_len_returns_all() {
        let messages = vec![Message::user("only")];
        let tail = transcript_json(&messages, Some(50));
        assert_eq!(tail.as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_transcript_no_limit_returns_all() {
        let messages: Vec<Message> = (0..5).map(|i| Message::user(format!("m{i}"))).collect();
        let full = transcript_json(&messages, None);
        assert_eq!(full.as_array().unwrap().len(), 5);
    }

    // -- limit clamping --

    #[test]
    fn test_clamp_list_limit() {
        assert_eq!(clamp_list_limit(None), DEFAULT_LIST_LIMIT);
        assert_eq!(clamp_list_limit(Some(0)), 1);
        assert_eq!(clamp_list_limit(Some(10_000)), MAX_LIST_LIMIT);
        assert_eq!(clamp_list_limit(Some(25)), 25);
    }

    #[test]
    fn test_clamp_search_limit() {
        assert_eq!(clamp_search_limit(None), DEFAULT_SEARCH_LIMIT);
        assert_eq!(clamp_search_limit(Some(0)), 1);
        assert_eq!(clamp_search_limit(Some(10_000)), MAX_SEARCH_LIMIT);
        assert_eq!(clamp_search_limit(Some(7)), 7);
    }

    // -- session-event wire shape --

    #[test]
    fn test_session_event_kind_tokens() {
        assert_eq!(SessionEventKind::Created.as_str(), "created");
        assert_eq!(SessionEventKind::Updated.as_str(), "updated");
        assert_eq!(SessionEventKind::Deleted.as_str(), "deleted");
    }

    // -- CLI argument parsing --

    #[test]
    fn test_parse_args_flags_and_positionals() {
        let (flags, pos) = parse_args(&["--all", "abc", "--limit", "5", "def"]);
        assert!(flags.all);
        assert_eq!(flags.limit, Some(5));
        assert_eq!(pos, vec!["abc", "def"]);
    }

    #[test]
    fn test_parse_args_drops_yes_and_unknown_flags() {
        let (flags, pos) = parse_args(&["--yes", "id1", "--weird"]);
        assert!(!flags.all);
        assert!(flags.limit.is_none());
        assert_eq!(pos, vec!["id1"]);
    }

    #[test]
    fn test_parse_args_limit_without_value() {
        let (flags, pos) = parse_args(&["id", "--limit"]);
        assert_eq!(flags.limit, None);
        assert_eq!(pos, vec!["id"]);
    }

    // -- correlation_id validation (scoped reply topic safety) --
    // Tests exercise Session::require_correlation_id directly.

    #[test]
    fn test_correlation_id_rejects_empty() {
        let payload = serde_json::json!({ "correlation_id": "" });
        assert!(Session::require_correlation_id(&payload, "test").is_err());
    }

    #[test]
    fn test_correlation_id_rejects_missing() {
        let payload = serde_json::json!({});
        assert!(Session::require_correlation_id(&payload, "test").is_err());
    }

    #[test]
    fn test_correlation_id_rejects_dots() {
        let payload = serde_json::json!({ "correlation_id": "abc.def" });
        assert!(Session::require_correlation_id(&payload, "test").is_err());
    }

    #[test]
    fn test_correlation_id_accepts_uuid() {
        let payload =
            serde_json::json!({ "correlation_id": "550e8400-e29b-41d4-a716-446655440000" });
        assert_eq!(
            Session::require_correlation_id(&payload, "test").unwrap(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
    }
}
