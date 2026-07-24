#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![allow(missing_docs)]

//! Durable trace and evaluation archive capsule for Unicity AOS.
//!
//! Gives agents a typed capability to record inspectable experience —
//! harness candidates, task outcomes, scores, and cost — instead of losing
//! it at the end of a session. This is the gap named directly in
//! `docs/meta-harness.md`: "a unified inventory of harness artifacts,
//! durable trace/evaluation archives" is called out as the next thing a
//! meta-harness needs to make world changes genuinely evaluable rather than
//! guessed at.
//!
//! Records are appended as newline-delimited JSON to a single project-local
//! file (`cwd://{cwd_dir}/trace.jsonl`, sharing the `cwd_dir` convention
//! `aos-memory` uses). Each record captures one unit of experience: a
//! harness candidate's source/reasoning, a task outcome, a score or
//! qualitative note, and cost — the fields `docs/meta-harness.md` asks
//! evaluation to retain.
//!
//! Three tools:
//! - `record_trace` — append a trace or evaluation record, returns its id.
//! - `list_traces` — filtered, summarized listing (bounded, newest first).
//! - `get_trace` — fetch one full record by id.
//!
//! # Design notes
//!
//! **Append, don't rewrite the world.** The archive is meant to accumulate
//! across sessions and candidates. There is no `delete`/`update` tool by
//! design — trace data is retained evidence, not scratch state. An operator
//! who needs to prune the file can do so with `aos-fs` directly.
//!
//! **Bounded reads.** `list_traces` never returns full records (metadata is
//! omitted) and is capped at [`MAX_LIST_LIMIT`] to avoid a single call
//! flooding the agent's context window as the archive grows across a long
//! meta-harness search run.
//!
//! **No prompt injection.** Unlike `aos-memory`, this capsule never writes
//! into the system prompt. It is a plain callable capability — an agent (or
//! a Forge-built proposer) reaches for it deliberately when recording or
//! inspecting experience, matching the "raw, selectively inspectable
//! experience" principle from the meta-harness research.

mod time;

use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::{Deserialize, Serialize};

/// Path (relative to the VFS root) the archive is stored under.
const ARCHIVE_FILE: &str = "trace.jsonl";

/// Last-resort project folder name if the distro did not set `cwd_dir`.
const DEFAULT_CWD_DIR: &str = ".astrid";

/// Hard cap on serialized `metadata` size per record (8 KB). Rejected rather
/// than silently truncated — truncating structured JSON can produce invalid
/// data, and the caller is better positioned to decide what to drop.
const MAX_METADATA_BYTES: usize = 8 * 1024;

/// Hard cap on `summary` length (4 KB). Truncated (not rejected) since it is
/// free text and a shortened summary is still useful.
const MAX_SUMMARY_BYTES: usize = 4 * 1024;

/// Default number of records `list_traces` returns when `limit` is omitted.
const DEFAULT_LIST_LIMIT: usize = 20;

/// Maximum number of records `list_traces` will return in one call,
/// regardless of the requested `limit`.
const MAX_LIST_LIMIT: usize = 100;

/// The kinds of experience this capsule archives.
const VALID_KINDS: [&str; 2] = ["trace", "evaluation"];

/// Trace/evaluation archive capsule.
#[derive(Default)]
pub struct TraceArchive;

/// One archived unit of experience.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TraceRecord {
    /// Unique id assigned at record time (UUID v4).
    id: String,
    /// RFC 3339 timestamp assigned at record time.
    ts: String,
    /// `"trace"` (a candidate's raw execution trace) or `"evaluation"` (a
    /// scored/qualitative judgment of a candidate or task outcome).
    kind: String,
    /// Identifier for the harness/agent candidate this record belongs to,
    /// if any. Lets a proposer group prior experience by candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_id: Option<String>,
    /// Pointer to the source this record is about (a file path, commit,
    /// capsule name, or other caller-defined reference).
    #[serde(skip_serializing_if = "Option::is_none")]
    source_ref: Option<String>,
    /// Free-text summary of what happened. Always present.
    summary: String,
    /// Free-text task outcome (e.g. "passed", "regressed p50 latency 12%").
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    /// Numeric score, if the evaluation produced one.
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    /// Cost of producing this record (tokens, dollars, seconds — caller's
    /// unit of choice; not interpreted by this capsule).
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<f64>,
    /// Free-form tags for filtering (e.g. `["retrieval", "regression"]`).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tags: Vec<String>,
    /// Arbitrary structured payload (raw trace data, diffs, evidence).
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

/// Summary view of a [`TraceRecord`] returned by `list_traces`. Omits
/// `metadata` so a listing call cannot blow the context window.
#[derive(Serialize)]
struct TraceSummary<'a> {
    id: &'a str,
    ts: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_id: Option<&'a str>,
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tags: &'a [String],
}

impl<'a> From<&'a TraceRecord> for TraceSummary<'a> {
    fn from(r: &'a TraceRecord) -> Self {
        Self {
            id: &r.id,
            ts: &r.ts,
            kind: &r.kind,
            candidate_id: r.candidate_id.as_deref(),
            summary: &r.summary,
            outcome: r.outcome.as_deref(),
            score: r.score,
            tags: &r.tags,
        }
    }
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct RecordTraceArgs {
    /// `"trace"` or `"evaluation"`. Defaults to `"trace"`.
    pub kind: Option<String>,
    /// Candidate id this record belongs to, if part of a harness search.
    pub candidate_id: Option<String>,
    /// Pointer to the source this record is about.
    pub source_ref: Option<String>,
    /// What happened, in plain language. Required.
    pub summary: String,
    /// Task outcome, in plain language.
    pub outcome: Option<String>,
    /// Numeric score, if any.
    pub score: Option<f64>,
    /// Cost of producing this record, in the caller's unit of choice.
    pub cost: Option<f64>,
    /// Tags for later filtering.
    pub tags: Option<Vec<String>>,
    /// Arbitrary structured payload (max 8 KB serialized).
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ListTracesArgs {
    /// Filter to `"trace"` or `"evaluation"` records only.
    pub kind: Option<String>,
    /// Filter to records with this exact `candidate_id`.
    pub candidate_id: Option<String>,
    /// Filter to records that carry this tag.
    pub tag: Option<String>,
    /// Only return records with `ts >= since` (RFC 3339, lexicographic
    /// comparison is safe since timestamps are zero-padded).
    pub since: Option<String>,
    /// Max records to return (default 20, hard cap 100). Newest first.
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct GetTraceArgs {
    /// The record id returned by `record_trace`.
    pub id: String,
}

/// Directory the archive file lives in, `cwd://{cwd_dir}`.
fn archive_dir() -> String {
    let dir = env::var("cwd_dir");
    format!("cwd://{}", dir.as_deref().unwrap_or(DEFAULT_CWD_DIR))
}

/// Full path to the archive file.
fn archive_path() -> String {
    format!("{}/{ARCHIVE_FILE}", archive_dir())
}

/// Truncate `s` to `max_bytes` on a UTF-8 char boundary, appending a marker
/// if truncation occurred.
fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let end = s.floor_char_boundary(max_bytes);
    format!("{}... [truncated, {} bytes total]", &s[..end], s.len())
}

/// Read every well-formed record currently in the archive.
///
/// Malformed lines (should not occur outside manual file tampering) are
/// skipped rather than failing the whole read, so one bad line cannot brick
/// the archive.
fn read_all() -> Result<Vec<TraceRecord>, SysError> {
    let path = archive_path();
    if !fs::exists(&path)? {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    Ok(content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<TraceRecord>(l).ok())
        .collect())
}

/// Append `record` to the archive, creating the project directory and file
/// if this is the first write.
fn append(record: &TraceRecord) -> Result<(), SysError> {
    let dir = archive_dir();
    if !fs::exists(&dir)? {
        fs::create_dir(&dir)?;
    }

    let path = archive_path();
    let mut content = if fs::exists(&path)? {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content
        .push_str(&serde_json::to_string(record).map_err(|e| SysError::ApiError(e.to_string()))?);
    content.push('\n');

    fs::write(&path, content.as_bytes())?;
    Ok(())
}

/// Trace/evaluation archive tools: record, list, and fetch inspectable
/// experience for meta-harness style search and ordinary agent work.
#[capsule]
impl TraceArchive {
    /// Record a trace or evaluation entry in the durable archive. Returns
    /// the assigned `id` and `ts` as a JSON object. Use `kind: "trace"` for
    /// a raw execution record and `kind: "evaluation"` for a scored or
    /// qualitative judgment. Records are append-only — there is no
    /// edit/delete tool, since archived experience is meant to stay intact.
    #[astrid::tool("record_trace", mutable)]
    pub fn record_trace(&self, args: RecordTraceArgs) -> Result<String, SysError> {
        let kind = args.kind.unwrap_or_else(|| "trace".to_string());
        if !VALID_KINDS.contains(&kind.as_str()) {
            return Err(SysError::ApiError(format!(
                "invalid kind '{kind}': expected one of {VALID_KINDS:?}"
            )));
        }

        let summary = args.summary.trim();
        if summary.is_empty() {
            return Err(SysError::ApiError("summary must not be empty".into()));
        }
        let summary = truncate(summary, MAX_SUMMARY_BYTES);

        let metadata = match &args.metadata {
            Some(v) => {
                let encoded =
                    serde_json::to_string(v).map_err(|e| SysError::ApiError(e.to_string()))?;
                if encoded.len() > MAX_METADATA_BYTES {
                    return Err(SysError::ApiError(format!(
                        "metadata is {} bytes, limit is {MAX_METADATA_BYTES} bytes; shrink it or store the large payload via aos-fs and pass a source_ref instead",
                        encoded.len()
                    )));
                }
                Some(v.clone())
            }
            None => None,
        };

        let record = TraceRecord {
            id: uuid::Uuid::new_v4().to_string(),
            ts: time::now_rfc3339(),
            kind,
            candidate_id: args.candidate_id,
            source_ref: args.source_ref,
            summary,
            outcome: args.outcome,
            score: args.score,
            cost: args.cost,
            tags: args.tags.unwrap_or_default(),
            metadata,
        };

        append(&record)?;

        serde_json::to_string(&serde_json::json!({ "id": record.id, "ts": record.ts }))
            .map_err(|e| SysError::ApiError(e.to_string()))
    }

    /// List archived records, newest first, filtered by `kind`,
    /// `candidate_id`, `tag`, and/or `since`. Returns summaries (no
    /// `metadata`) capped at `limit` (default 20, max 100) to keep results
    /// bounded as the archive grows.
    #[astrid::tool("list_traces")]
    pub fn list_traces(&self, args: ListTracesArgs) -> Result<String, SysError> {
        let limit = args.limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT);

        let mut records = read_all()?;
        records.sort_by(|a, b| b.ts.cmp(&a.ts));

        let filtered: Vec<TraceSummary<'_>> = records
            .iter()
            .filter(|r| args.kind.as_deref().is_none_or(|k| r.kind == k))
            .filter(|r| {
                args.candidate_id
                    .as_deref()
                    .is_none_or(|c| r.candidate_id.as_deref() == Some(c))
            })
            .filter(|r| {
                args.tag
                    .as_deref()
                    .is_none_or(|t| r.tags.iter().any(|tag| tag == t))
            })
            .filter(|r| args.since.as_deref().is_none_or(|s| r.ts.as_str() >= s))
            .take(limit)
            .map(TraceSummary::from)
            .collect();

        serde_json::to_string(&filtered).map_err(|e| SysError::ApiError(e.to_string()))
    }

    /// Fetch one full record (including `metadata`) by `id`.
    #[astrid::tool("get_trace")]
    pub fn get_trace(&self, args: GetTraceArgs) -> Result<String, SysError> {
        let records = read_all()?;
        let record = records
            .into_iter()
            .find(|r| r.id == args.id)
            .ok_or_else(|| SysError::ApiError(format!("no trace record with id '{}'", args.id)))?;

        serde_json::to_string(&record).map_err(|e| SysError::ApiError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn truncate_long_adds_marker() {
        let input = "a".repeat(50);
        let out = truncate(&input, 10);
        assert!(out.starts_with(&"a".repeat(10)));
        assert!(out.contains("[truncated, 50 bytes total]"));
    }

    #[test]
    fn truncate_at_char_boundary() {
        let input = "\u{1F600}".repeat(20); // 4 bytes each = 80 bytes
        let out = truncate(&input, 10);
        // floor_char_boundary(10) for 4-byte chars = 8
        assert!(out.starts_with(&"\u{1F600}".repeat(2)));
    }

    #[test]
    fn valid_kinds_contains_trace_and_evaluation() {
        assert!(VALID_KINDS.contains(&"trace"));
        assert!(VALID_KINDS.contains(&"evaluation"));
        assert!(!VALID_KINDS.contains(&"bogus"));
    }

    #[test]
    fn trace_summary_from_record_omits_metadata() {
        let record = TraceRecord {
            id: "abc".into(),
            ts: "2026-01-01T00:00:00.000Z".into(),
            kind: "trace".into(),
            candidate_id: Some("c1".into()),
            source_ref: None,
            summary: "did a thing".into(),
            outcome: Some("passed".into()),
            score: Some(0.9),
            cost: None,
            tags: vec!["retrieval".into()],
            metadata: Some(serde_json::json!({"big": "payload"})),
        };
        let summary = TraceSummary::from(&record);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("payload"));
        assert!(json.contains("did a thing"));
    }

    #[test]
    fn record_serializes_without_optional_fields() {
        let record = TraceRecord {
            id: "abc".into(),
            ts: "2026-01-01T00:00:00.000Z".into(),
            kind: "trace".into(),
            candidate_id: None,
            source_ref: None,
            summary: "minimal".into(),
            outcome: None,
            score: None,
            cost: None,
            tags: Vec::new(),
            metadata: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("candidate_id"));
        assert!(!json.contains("tags"));
        assert!(!json.contains("metadata"));

        // Round-trips through the archive's line-based reader.
        let parsed: TraceRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "abc");
        assert!(parsed.tags.is_empty());
    }
}
