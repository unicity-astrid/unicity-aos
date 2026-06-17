//! Pragmatic Capsule.toml linting for `validate_manifest` plus the
//! installed-capsule diagnostics shared by `capsule_doctor`.
//!
//! These target the COMMON mistakes a new author hits — not a re-implementation
//! of the kernel's full manifest parser.

use serde::Serialize;
use toml::Value as Toml;

/// A single lint finding, serialized to JSON for the LLM.
#[derive(Debug, Serialize)]
pub(crate) struct Finding {
    /// "error" | "warn" | "info"
    pub level: &'static str,
    pub message: String,
    pub fix: String,
}

impl Finding {
    fn err(message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            level: "error",
            message: message.into(),
            fix: fix.into(),
        }
    }
    fn warn(message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            level: "warn",
            message: message.into(),
            fix: fix.into(),
        }
    }
    fn info(message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            level: "info",
            message: message.into(),
            fix: fix.into(),
        }
    }
}

/// Looks MAJOR.MINOR.PATCH-ish (all numeric, exactly three parts).
fn is_semverish(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// A topic segment is malformed if the key has empty segments
/// (leading/trailing/consecutive dots).
fn has_empty_segments(topic: &str) -> bool {
    topic.is_empty() || topic.starts_with('.') || topic.ends_with('.') || topic.contains("..")
}

/// Run all manifest lints. Returns the findings in roughly severity order.
pub(crate) fn validate_manifest(toml_src: &str) -> Vec<Finding> {
    let root: Toml = match toml_src.parse() {
        Ok(t) => t,
        Err(e) => {
            return vec![Finding::err(
                format!("Capsule.toml is not valid TOML: {e}"),
                "Fix the syntax error reported above; the rest of the lint can't run until it parses.",
            )];
        }
    };

    let mut out = Vec::new();
    check_package(&root, &mut out);
    check_component(&root, &mut out);
    check_env(&root, &mut out);
    let (pub_keys, sub_keys) = collect_topics(&root, &mut out);
    check_tool_bus(&sub_keys, &pub_keys, &root, &mut out);
    check_topic_shapes(&pub_keys, &mut out);
    out
}

fn check_package(root: &Toml, out: &mut Vec<Finding>) {
    let pkg = root.get("package");
    let name = pkg.and_then(|p| p.get("name")).and_then(Toml::as_str);
    if name.is_none_or(str::is_empty) {
        out.push(Finding::err(
            "[package].name is missing or empty.",
            "Add `name = \"my-capsule\"` under [package].",
        ));
    }
    match pkg.and_then(|p| p.get("version")).and_then(Toml::as_str) {
        Some(v) if is_semverish(v) => {}
        Some(v) => out.push(Finding::err(
            format!("[package].version = \"{v}\" is not MAJOR.MINOR.PATCH."),
            "Use a three-part numeric version like \"0.1.0\".",
        )),
        None => out.push(Finding::err(
            "[package].version is missing.",
            "Add `version = \"0.1.0\"` under [package].",
        )),
    }
}

fn check_component(root: &Toml, out: &mut Vec<Finding>) {
    let comps = root.get("component").and_then(Toml::as_array);
    let has_file = comps.is_some_and(|arr| {
        arr.iter().any(|c| {
            c.get("file")
                .and_then(Toml::as_str)
                .is_some_and(|f| !f.is_empty())
        })
    });
    if !has_file {
        out.push(Finding::err(
            "No [[component]] with a `file` was found.",
            "Add a [[component]] table with `id`, `file = \"my_capsule.wasm\"`, `type = \"executable\"`.",
        ));
    }
}

fn check_env(root: &Toml, out: &mut Vec<Finding>) {
    let Some(env) = root.get("env").and_then(Toml::as_table) else {
        return;
    };
    for (key, val) in env {
        if val.get("scope").and_then(Toml::as_str) == Some("shared") {
            out.push(Finding::warn(
                format!("[env].{key} sets scope = \"shared\", which is silently ignored."),
                "Remove `scope`; shared/operator-only env scope is not honoured from the manifest.",
            ));
        }
    }
}

/// Collect the `[publish]` and `[subscribe]` topic keys, warning if a table is
/// empty (fail-closed: the capsule then can't publish/subscribe at all).
fn collect_topics(root: &Toml, out: &mut Vec<Finding>) -> (Vec<String>, Vec<String>) {
    let pub_keys = table_keys(root, "publish");
    let sub_keys = table_keys(root, "subscribe");
    if pub_keys.is_empty() {
        out.push(Finding::warn(
            "[publish] is empty or missing — the capsule cannot publish any event.",
            "Add the tool-bus publish keys `tool.v1.execute.*.result` and `tool.v1.response.describe.*`.",
        ));
    }
    if sub_keys.is_empty() {
        out.push(Finding::warn(
            "[subscribe] is empty or missing — the capsule cannot receive any event.",
            "Add one `tool.v1.execute.<tool>` per tool plus `tool.v1.request.describe`.",
        ));
    }
    (pub_keys, sub_keys)
}

fn table_keys(root: &Toml, table: &str) -> Vec<String> {
    root.get(table)
        .and_then(Toml::as_table)
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

/// Check the mandatory tool-bus wiring: each `tool.v1.execute.<x>` subscribe has
/// a handler, the two mandatory publish keys exist, and the describe request is
/// subscribed.
fn check_tool_bus(sub_keys: &[String], pub_keys: &[String], root: &Toml, out: &mut Vec<Finding>) {
    let sub_table = root.get("subscribe").and_then(Toml::as_table);
    let mut saw_execute_tool = false;

    for key in sub_keys {
        let Some(tool) = key.strip_prefix("tool.v1.execute.") else {
            continue;
        };
        // The `*.result` publish key would also strip; skip non-tool shapes.
        if tool.contains('*') || tool.contains('.') {
            continue;
        }
        saw_execute_tool = true;
        let has_handler = sub_table
            .and_then(|t| t.get(key))
            .and_then(|v| v.get("handler"))
            .and_then(Toml::as_str)
            .is_some_and(|h| !h.is_empty());
        if !has_handler {
            out.push(Finding::err(
                format!("Subscribe `{key}` has no `handler`."),
                format!("Add `handler = \"tool_execute_{tool}\"` to the `{key}` row."),
            ));
        }
    }

    if saw_execute_tool {
        for required in ["tool.v1.execute.*.result", "tool.v1.response.describe.*"] {
            if !pub_keys.iter().any(|k| k == required) {
                out.push(Finding::err(
                    format!("Mandatory publish key `{required}` is missing."),
                    format!("Add `\"{required}\" = {{ wit = ... }}` to [publish]; tool results/describe break without it."),
                ));
            }
        }
        if !sub_keys.iter().any(|k| k == "tool.v1.request.describe") {
            out.push(Finding::err(
                "Subscribe `tool.v1.request.describe` is missing — tools won't be discoverable.",
                "Add `\"tool.v1.request.describe\" = { wit = ..., handler = \"tool_describe\" }` to [subscribe].",
            ));
        }
    }
}

/// Flag malformed topic keys: empty segments, or absurd depth (>8 segments).
fn check_topic_shapes(pub_keys: &[String], out: &mut Vec<Finding>) {
    for key in pub_keys {
        if has_empty_segments(key) {
            out.push(Finding::err(
                format!("Topic `{key}` has empty segments (leading/trailing/consecutive dots)."),
                "Remove the stray dots; every segment between dots must be non-empty.",
            ));
        }
        if key.split('.').count() > 8 {
            out.push(Finding::warn(
                format!("Topic `{key}` has more than 8 segments — likely a mistake."),
                "Flatten the topic; deep topic trees usually indicate a naming error.",
            ));
        }
    }
    if out.iter().all(|f| f.level != "error") {
        out.push(Finding::info(
            "No blocking manifest errors found.",
            "Build with `astrid capsule build`, then install with `astrid capsule install`.",
        ));
    }
}
