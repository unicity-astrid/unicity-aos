//! OpenAI model registry — static capability catalog.
//!
//! The LIVE `GET /v1/models` API is the authority for *which* models exist;
//! this catalog is capability-enrichment (context window, max output tokens,
//! feature flags) keyed by id, plus the offline/keyless fallback list when the
//! live query is unavailable. It does not need to be exhaustive — the frontier
//! and common families suffice; unknown ids resolve to conservative defaults.
//!
//! This module is a pure capability catalog and provider-entry builder: it reads
//! no environment and applies no overrides. Env-driven overrides (model selection,
//! `max_output_tokens`, etc.) are applied by the request path in `lib.rs`.
//!
//! Last updated: 2026-06-23. Sources:
//! - https://developers.openai.com/api/docs/models
//! - https://developers.openai.com/api/docs/models/gpt-5.4
//! - https://developers.openai.com/api/docs/models/gpt-5.2

use serde::Serialize;

/// Known OpenAI model capabilities.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelInfo {
    /// Model identifier as used in the API.
    pub id: &'static str,
    /// Human-readable display name.
    pub name: &'static str,
    /// Maximum context window in tokens.
    pub context_window: u64,
    /// Default max output tokens.
    pub max_output_tokens: u64,
    /// Supports vision (image inputs).
    pub supports_vision: bool,
    /// Supports tool/function calling.
    pub supports_tools: bool,
    /// Supports structured outputs (response_format json_schema).
    pub supports_structured_output: bool,
    /// Supports reasoning effort levels (none/low/medium/high/xhigh).
    pub is_reasoning: bool,
}

/// Static registry of known OpenAI models.
///
/// Update this table when OpenAI releases new models.
/// Unknown models fall back to conservative defaults via [`lookup`].
pub(crate) static MODELS: &[ModelInfo] = &[
    // ── GPT-5.5 series (June 2026, current frontier) ─────────────
    ModelInfo {
        id: "gpt-5.5",
        name: "GPT-5.5",
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true, // supports effort: none/low/medium/high/xhigh
    },
    ModelInfo {
        id: "gpt-5.5-codex",
        name: "GPT-5.5 Codex",
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    // ── GPT-5.4 series (March 2026) ──────────────────────────────
    ModelInfo {
        id: "gpt-5.4",
        name: "GPT-5.4",
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true, // supports effort: none/low/medium/high/xhigh
    },
    ModelInfo {
        id: "gpt-5.4-mini",
        name: "GPT-5.4 Mini",
        context_window: 400_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    ModelInfo {
        id: "gpt-5.4-nano",
        name: "GPT-5.4 Nano",
        context_window: 400_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    // ── GPT-5.3 series ───────────────────────────────────────────
    ModelInfo {
        id: "gpt-5.3",
        name: "GPT-5.3 Instant",
        context_window: 400_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: false,
    },
    ModelInfo {
        id: "gpt-5.3-codex",
        name: "GPT-5.3 Codex",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    ModelInfo {
        id: "gpt-5.3-codex-spark",
        name: "GPT-5.3 Codex Spark",
        context_window: 128_000,
        max_output_tokens: 128_000,
        supports_vision: false,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: false,
    },
    // ── GPT-5.2 series (December 2025) ───────────────────────────
    ModelInfo {
        id: "gpt-5.2",
        name: "GPT-5.2",
        context_window: 400_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    ModelInfo {
        id: "gpt-5.2-codex",
        name: "GPT-5.2 Codex",
        context_window: 400_000,
        max_output_tokens: 128_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    // ── GPT-4.1 series (April 2025, still available) ─────────────
    ModelInfo {
        id: "gpt-4.1",
        name: "GPT-4.1",
        context_window: 1_048_576,
        max_output_tokens: 32_768,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: false,
    },
    ModelInfo {
        id: "gpt-4.1-mini",
        name: "GPT-4.1 Mini",
        context_window: 1_048_576,
        max_output_tokens: 32_768,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: false,
    },
    ModelInfo {
        id: "gpt-4.1-nano",
        name: "GPT-4.1 Nano",
        context_window: 1_048_576,
        max_output_tokens: 32_768,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: false,
    },
    // ── o-series reasoning models ────────────────────────────────
    ModelInfo {
        id: "o3",
        name: "o3",
        context_window: 200_000,
        max_output_tokens: 100_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    ModelInfo {
        id: "o3-mini",
        name: "o3 Mini",
        context_window: 200_000,
        max_output_tokens: 100_000,
        supports_vision: false,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    ModelInfo {
        id: "o4-mini",
        name: "o4 Mini",
        context_window: 200_000,
        max_output_tokens: 100_000,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: true,
    },
    // ── GPT-4o series (legacy, still available) ──────────────────
    ModelInfo {
        id: "gpt-4o",
        name: "GPT-4o",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: false,
    },
    ModelInfo {
        id: "gpt-4o-mini",
        name: "GPT-4o Mini",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_vision: true,
        supports_tools: true,
        supports_structured_output: true,
        is_reasoning: false,
    },
];

/// Conservative defaults for unknown models.
const UNKNOWN_DEFAULTS: ModelInfo = ModelInfo {
    id: "unknown",
    name: "Unknown Model",
    context_window: 128_000,
    max_output_tokens: 16_384,
    supports_vision: false,
    supports_tools: true,
    supports_structured_output: false,
    is_reasoning: false,
};

/// Look up a model by ID. Returns conservative defaults for unknown models.
///
/// Matches exact IDs first, then falls back to the LONGEST matching prefix for
/// dated snapshots (e.g., `gpt-5.4-2026-03-05` matches `gpt-5.4`). Longest-match
/// wins so an overlapping-prefix snapshot resolves to the most specific catalog
/// row — `gpt-5.4-mini-2026-03-05` must match `gpt-5.4-mini`, not the shorter
/// `gpt-5.4` that happens to be declared first.
pub(crate) fn lookup(model_id: &str) -> &'static ModelInfo {
    // Exact match.
    if let Some(info) = MODELS.iter().find(|m| m.id == model_id) {
        return info;
    }

    // Longest-prefix match for dated snapshots. Catalog declaration order is NOT
    // a reliable specificity order (`gpt-5.4` precedes `gpt-5.4-mini`), so we
    // pick the prefix that consumes the most of the model id rather than the
    // first one that matches.
    if let Some(info) = MODELS
        .iter()
        .filter(|m| model_id.starts_with(m.id))
        .max_by_key(|m| m.id.len())
    {
        return info;
    }

    &UNKNOWN_DEFAULTS
}

/// One selectable model, shaped to the `provider-entry` WIT record
/// (`astrid-runtime/wit` `interfaces/registry.wit`). Serialized to JSON and
/// carried in the `describe-response.providers` array. Field names match the
/// WIT record exactly so the registry deserializes it directly.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProviderEntry {
    /// MODEL id (e.g. "gpt-5.4"). Per registry.wit this is a model id, not the
    /// provider/capsule name.
    pub id: String,
    /// Human-readable summary (e.g. "OpenAI GPT-5.4 Mini").
    pub description: String,
    /// Topic the registry routes generate requests to for this provider.
    pub request_topic: String,
    /// Topic this provider streams responses on.
    pub stream_topic: String,
    /// Capability tags derived from the model's catalog flags.
    pub capabilities: Vec<String>,
    /// Maximum context window in tokens for this model.
    pub context_window: u64,
    /// Default max output tokens for this model.
    pub max_output_tokens: u64,
}

/// Capability list derived from a model's catalog flags. "text" is always
/// present; every other tag is gated on the model's own flag so a future
/// non-tools (or non-vision/structured/reasoning) model advertises correctly.
pub(crate) fn capabilities_for(info: &ModelInfo) -> Vec<String> {
    let mut caps = vec!["text".to_string()];
    if info.supports_tools {
        caps.push("tools".to_string());
    }
    if info.supports_vision {
        caps.push("vision".to_string());
    }
    if info.supports_structured_output {
        caps.push("structured_output".to_string());
    }
    if info.is_reasoning {
        caps.push("reasoning".to_string());
    }
    caps
}

/// Build the full ordered list of provider entries for the describe response.
///
/// `default_model` is the env-`model` hint (the model the registry should
/// auto-select for this capsule). It is resolved through [`lookup`] — the same
/// prefix/snapshot-aware resolution the execute path uses — so a dated-snapshot
/// default (e.g. `gpt-5.4-2026-03-05`) hoists its canonical catalog row
/// (`gpt-5.4`). The matching entry is emitted FIRST so the registry can identify
/// the default purely from response ordering — no WIT field is added (the
/// `provider-entry` record has no `default` flag and is a frozen-shaped
/// contract). If `default_model` does not resolve to a catalog id, ordering is
/// left as the catalog order (the registry treats entry[0] as the default hint
/// regardless).
///
/// `request_topic` / `stream_topic` are SHARED across every entry — all models
/// are served by the same generate/stream topics; only the model id differs.
pub(crate) fn build_provider_entries(
    default_model: &str,
    request_topic: &str,
    stream_topic: &str,
) -> Vec<ProviderEntry> {
    let mut entries: Vec<ProviderEntry> = MODELS
        .iter()
        .map(|info| ProviderEntry {
            id: info.id.to_string(),
            description: format!("OpenAI {}", info.name),
            request_topic: request_topic.to_string(),
            stream_topic: stream_topic.to_string(),
            capabilities: capabilities_for(info),
            context_window: info.context_window,
            max_output_tokens: info.max_output_tokens,
        })
        .collect();

    // Hoist the env-default model to the front so it is the identifiable
    // auto-select hint. Resolve the default through `lookup` so a dated-snapshot
    // default (e.g. gpt-5.4-2026-03-05) hoists its canonical catalog row; an
    // unknown default resolves to UNKNOWN_DEFAULTS (not a catalog id) and leaves
    // the full catalog intact in catalog order. Move-to-front is STABLE so the
    // remaining entries keep their catalog order.
    let default_id = lookup(default_model).id;
    if let Some(pos) = entries.iter().position(|e| e.id == default_id) {
        let entry = entries.remove(pos);
        entries.insert(0, entry);
    }

    entries
}

/// Build provider entries from a LIVE `/v1/models` id list, ENRICHED from the
/// catalog.
///
/// Each live id is resolved through [`lookup`] (exact, then longest-prefix) to
/// borrow its capability flags + context window + max output tokens. An id that
/// is NOT in the catalog resolves to [`UNKNOWN_DEFAULTS`] (conservative) and is
/// named after the id itself (`OpenAI <id>`) rather than the generic
/// `Unknown Model`, so the live list stays self-describing. A catalog id keeps
/// its curated display name.
///
/// The configured `default_model` is always `entry[0]`: if it is present in the
/// live list it is hoisted (stable move-to-front, the rest keep server order);
/// if it is absent it is PREPENDED as its own enriched entry, so the operator's
/// configured default is always selectable and first even when the upstream
/// catalogue does not advertise it.
///
/// Live ids are deduplicated stably (server order preserved) before any
/// hoist/prepend, and a default-prepend never produces a duplicate of an id
/// already present. `request_topic`/`stream_topic` are SHARED across every
/// entry — only the model id differs.
pub(crate) fn build_live_entries(
    live_ids: &[String],
    default_model: &str,
    request_topic: &str,
    stream_topic: &str,
) -> Vec<ProviderEntry> {
    let make_entry = |id: &str| -> ProviderEntry {
        let info = lookup(id);
        // A catalog hit keeps its curated display name; a miss (UNKNOWN_DEFAULTS)
        // is named after the live id itself, never the generic "Unknown Model".
        let description = if info.id == UNKNOWN_DEFAULTS.id {
            format!("OpenAI {id}")
        } else {
            format!("OpenAI {}", info.name)
        };
        ProviderEntry {
            id: id.to_string(),
            description,
            request_topic: request_topic.to_string(),
            stream_topic: stream_topic.to_string(),
            capabilities: capabilities_for(info),
            context_window: info.context_window,
            max_output_tokens: info.max_output_tokens,
        }
    };

    // Stable dedup of the live ids, preserving server order, dropping blanks —
    // a defensive second line behind the discovery-side extraction.
    let mut seen = std::collections::HashSet::new();
    let mut ordered: Vec<String> = live_ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .filter(|id| seen.insert(id.clone()))
        .collect();

    // Hoist (or prepend) the configured default so it is always entry[0].
    let trimmed_default = default_model.trim();
    if !trimmed_default.is_empty() {
        if let Some(pos) = ordered.iter().position(|id| id == trimmed_default) {
            let id = ordered.remove(pos);
            ordered.insert(0, id);
        } else {
            ordered.insert(0, trimmed_default.to_string());
        }
    }

    ordered.iter().map(|id| make_entry(id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST_TOPIC: &str = "llm.v1.request.generate.openai";
    const STREAM_TOPIC: &str = "llm.v1.stream.openai";

    #[test]
    fn lookup_exact_match_wins() {
        // An exact id always resolves to its own row, never a prefix.
        assert_eq!(lookup("gpt-5.4").id, "gpt-5.4");
        assert_eq!(lookup("gpt-5.4-mini").id, "gpt-5.4-mini");
        assert_eq!(lookup("gpt-5.3-codex-spark").id, "gpt-5.3-codex-spark");
    }

    #[test]
    fn lookup_prefers_longest_matching_prefix() {
        // Overlapping prefixes: `gpt-5.4` is declared BEFORE `gpt-5.4-mini`, so a
        // first-match prefix search would wrongly resolve a mini snapshot to the
        // base model. Longest-match-wins must pick the most specific row.
        assert_eq!(lookup("gpt-5.4-mini-2026-03-05").id, "gpt-5.4-mini");
        assert_eq!(lookup("gpt-5.4-nano-2026-03-05").id, "gpt-5.4-nano");

        // Three-level overlap: `gpt-5.3` ⊂ `gpt-5.3-codex` ⊂ `gpt-5.3-codex-spark`.
        // A dated `gpt-5.3-codex` snapshot must resolve to `gpt-5.3-codex`, not the
        // shorter `gpt-5.3`; a `gpt-5.3-codex-spark` snapshot to the longest row.
        assert_eq!(lookup("gpt-5.3-codex-2026-01-01").id, "gpt-5.3-codex");
        assert_eq!(
            lookup("gpt-5.3-codex-spark-2026-01-01").id,
            "gpt-5.3-codex-spark"
        );

        // The base model still resolves for its own dated snapshot.
        assert_eq!(lookup("gpt-5.4-2026-03-05").id, "gpt-5.4");
        assert_eq!(lookup("gpt-4.1-mini-2025-04-14").id, "gpt-4.1-mini");
    }

    #[test]
    fn lookup_unknown_falls_back_to_defaults() {
        assert_eq!(lookup("does-not-exist").id, UNKNOWN_DEFAULTS.id);
    }

    #[test]
    fn capabilities_respect_supports_tools_flag() {
        // A catalog model with tools advertises the "tools" capability.
        let with_tools = ModelInfo {
            supports_tools: true,
            ..UNKNOWN_DEFAULTS
        };
        assert!(capabilities_for(&with_tools).contains(&"tools".to_string()));

        // A model without tools must NOT advertise "tools" — the field is
        // respected, not hardcoded. "text" remains unconditionally present.
        let no_tools = ModelInfo {
            supports_tools: false,
            ..UNKNOWN_DEFAULTS
        };
        let caps = capabilities_for(&no_tools);
        assert!(!caps.contains(&"tools".to_string()));
        assert!(caps.contains(&"text".to_string()));
    }

    #[test]
    fn describe_emits_one_entry_per_catalog_model() {
        let entries = build_provider_entries("gpt-5.5", REQUEST_TOPIC, STREAM_TOPIC);
        assert_eq!(entries.len(), MODELS.len());
    }

    #[test]
    fn gpt_5_5_is_in_catalog_as_frontier_head_and_default() {
        // The new frontier sits at the catalog HEAD and is the default-hoisted
        // entry (it is already first, so the hoist is a no-op that still leads).
        assert_eq!(MODELS[0].id, "gpt-5.5");
        assert_eq!(lookup("gpt-5.5").id, "gpt-5.5");
        assert_eq!(lookup("gpt-5.5-codex").id, "gpt-5.5-codex");

        let entries = build_provider_entries("gpt-5.5", REQUEST_TOPIC, STREAM_TOPIC);
        assert_eq!(entries[0].id, "gpt-5.5");
        // gpt-5.5 carries the frontier capability set + lengths.
        let g = &entries[0];
        assert_eq!(g.context_window, 1_050_000);
        assert_eq!(g.max_output_tokens, 128_000);
        assert!(g.capabilities.contains(&"reasoning".to_string()));
        assert!(g.capabilities.contains(&"vision".to_string()));
    }

    #[test]
    fn live_entry_for_catalog_id_is_enriched_from_catalog() {
        // A live id that IS in the catalog borrows its caps + lengths + curated
        // display name — not the conservative unknown defaults.
        let entries = build_live_entries(
            &["gpt-5.5".to_string()],
            "gpt-5.5",
            REQUEST_TOPIC,
            STREAM_TOPIC,
        );
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.id, "gpt-5.5");
        assert_eq!(e.description, "OpenAI GPT-5.5");
        assert_eq!(e.context_window, 1_050_000);
        assert_eq!(e.max_output_tokens, 128_000);
        assert!(e.capabilities.contains(&"vision".to_string()));
        assert!(e.capabilities.contains(&"reasoning".to_string()));
        assert_eq!(e.request_topic, REQUEST_TOPIC);
        assert_eq!(e.stream_topic, STREAM_TOPIC);
    }

    #[test]
    fn live_entry_for_dated_snapshot_enriches_via_prefix() {
        // A live dated-snapshot id resolves through longest-prefix lookup to its
        // canonical catalog row's caps/lengths, while KEEPING the live id verbatim.
        let entries = build_live_entries(
            &["gpt-5.4-mini-2026-03-05".to_string()],
            "gpt-5.5",
            REQUEST_TOPIC,
            STREAM_TOPIC,
        );
        // Default (gpt-5.5) is prepended first; the snapshot is the second entry.
        let snap = entries
            .iter()
            .find(|e| e.id == "gpt-5.4-mini-2026-03-05")
            .expect("snapshot entry present");
        // gpt-5.4-mini caps/lengths.
        assert_eq!(snap.context_window, 400_000);
        assert_eq!(snap.max_output_tokens, 128_000);
        assert_eq!(snap.description, "OpenAI GPT-5.4 Mini");
    }

    #[test]
    fn live_entry_for_unknown_id_uses_conservative_defaults_and_id_name() {
        // A live id NOT in the catalog falls back to UNKNOWN_DEFAULTS for caps +
        // lengths, and is named after the id itself, never "Unknown Model".
        let entries = build_live_entries(
            &["gpt-9-experimental".to_string()],
            "gpt-9-experimental",
            REQUEST_TOPIC,
            STREAM_TOPIC,
        );
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.id, "gpt-9-experimental");
        assert_eq!(e.description, "OpenAI gpt-9-experimental");
        assert_eq!(e.context_window, UNKNOWN_DEFAULTS.context_window);
        assert_eq!(e.max_output_tokens, UNKNOWN_DEFAULTS.max_output_tokens);
        // Conservative: no vision, no structured_output, no reasoning; tools+text.
        assert!(e.capabilities.contains(&"text".to_string()));
        assert!(e.capabilities.contains(&"tools".to_string()));
        assert!(!e.capabilities.contains(&"vision".to_string()));
        assert!(!e.capabilities.contains(&"reasoning".to_string()));
        // Never the generic catalog fallback name.
        assert_ne!(e.description, format!("OpenAI {}", UNKNOWN_DEFAULTS.name));
    }

    #[test]
    fn live_default_present_is_hoisted_first_preserving_server_order() {
        // The configured default appears mid-list; it must lead, and the rest
        // keep their server (discovered) order.
        let live = vec![
            "a-model".to_string(),
            "the-default".to_string(),
            "z-model".to_string(),
        ];
        let entries = build_live_entries(&live, "the-default", REQUEST_TOPIC, STREAM_TOPIC);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["the-default", "a-model", "z-model"]);
    }

    #[test]
    fn live_default_absent_is_prepended_as_its_own_entry() {
        // The configured default is NOT in the live list: it must still be first,
        // prepended as its own enriched entry, and nothing else is dropped.
        let live = vec!["a-model".to_string(), "z-model".to_string()];
        let entries = build_live_entries(&live, "gpt-5.5", REQUEST_TOPIC, STREAM_TOPIC);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-5.5", "a-model", "z-model"]);
        // The prepended default is enriched (gpt-5.5 is a catalog id).
        assert_eq!(entries[0].context_window, 1_050_000);
        assert_eq!(entries[0].description, "OpenAI GPT-5.5");
    }

    #[test]
    fn live_default_prepend_never_duplicates() {
        // Even if the default also appears later in the live list, hoisting must
        // not leave a duplicate id (dedup runs first, then move-to-front).
        let live = vec![
            "x".to_string(),
            "gpt-5.5".to_string(),
            "y".to_string(),
            "gpt-5.5".to_string(),
        ];
        let entries = build_live_entries(&live, "gpt-5.5", REQUEST_TOPIC, STREAM_TOPIC);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-5.5", "x", "y"]);
    }

    #[test]
    fn live_entries_drop_blank_ids_and_dedup() {
        // Defensive: blanks dropped, duplicates collapsed stably even at this layer.
        let live = vec![
            "  ".to_string(),
            "a".to_string(),
            "a".to_string(),
            String::new(),
            "b".to_string(),
        ];
        let entries = build_live_entries(&live, "", REQUEST_TOPIC, STREAM_TOPIC);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn live_entries_share_request_and_stream_topics() {
        let entries = build_live_entries(
            &["gpt-5.5".to_string(), "weird-id".to_string()],
            "gpt-5.5",
            REQUEST_TOPIC,
            STREAM_TOPIC,
        );
        for e in &entries {
            assert_eq!(e.request_topic, REQUEST_TOPIC);
            assert_eq!(e.stream_topic, STREAM_TOPIC);
        }
    }

    #[test]
    fn entry_ids_are_model_ids_never_provider_name() {
        let entries = build_provider_entries("gpt-5.4", REQUEST_TOPIC, STREAM_TOPIC);

        let catalog_ids: std::collections::HashSet<&str> = MODELS.iter().map(|m| m.id).collect();
        for entry in &entries {
            assert_ne!(
                entry.id, "openai",
                "provider name must never be an entry id"
            );
            assert!(
                catalog_ids.contains(entry.id.as_str()),
                "entry id {} is not a catalog model id",
                entry.id
            );
        }

        let entry_ids: std::collections::HashSet<&str> =
            entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(entry_ids, catalog_ids, "entry ids must equal catalog ids");
    }

    #[test]
    fn all_entries_share_request_and_stream_topics() {
        let entries = build_provider_entries("gpt-5.4", REQUEST_TOPIC, STREAM_TOPIC);
        for entry in &entries {
            assert_eq!(entry.request_topic, REQUEST_TOPIC);
            assert_eq!(entry.stream_topic, STREAM_TOPIC);
        }
    }

    #[test]
    fn default_model_is_first_entry() {
        let entries = build_provider_entries("o3-mini", REQUEST_TOPIC, STREAM_TOPIC);
        assert_eq!(entries[0].id, "o3-mini");

        let entries = build_provider_entries("gpt-5.4", REQUEST_TOPIC, STREAM_TOPIC);
        assert_eq!(entries[0].id, "gpt-5.4");
    }

    #[test]
    fn dated_snapshot_default_hoists_canonical_catalog_id() {
        // A dated-snapshot default resolves via `lookup` (prefix match) to its
        // canonical catalog row, which is the id that must be hoisted first.
        let entries = build_provider_entries("gpt-5.4-2026-03-05", REQUEST_TOPIC, STREAM_TOPIC);
        assert_eq!(entries[0].id, "gpt-5.4");
        assert_eq!(entries.len(), MODELS.len());

        // A non-frontier dated snapshot hoists its own canonical row, not the
        // catalog head.
        let entries = build_provider_entries("o4-mini-2025-04-16", REQUEST_TOPIC, STREAM_TOPIC);
        assert_eq!(entries[0].id, "o4-mini");
        assert_eq!(entries.len(), MODELS.len());
    }

    #[test]
    fn move_to_front_is_stable_preserving_catalog_order() {
        // Hoisting a mid-catalog default must be a STABLE move-to-front: the
        // default leads, every other entry keeps its relative catalog order
        // (no swap-induced scrambling of the old head).
        let default_id = "o3-mini";
        let entries = build_provider_entries(default_id, REQUEST_TOPIC, STREAM_TOPIC);
        assert_eq!(entries[0].id, default_id);

        let expected_tail: Vec<&str> = MODELS
            .iter()
            .map(|m| m.id)
            .filter(|id| *id != default_id)
            .collect();
        let actual_tail: Vec<&str> = entries[1..].iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            actual_tail, expected_tail,
            "non-default entries must keep catalog order after a stable move-to-front"
        );
    }

    #[test]
    fn unknown_default_model_leaves_catalog_order() {
        let entries = build_provider_entries("does-not-exist", REQUEST_TOPIC, STREAM_TOPIC);
        let expected: Vec<&str> = MODELS.iter().map(|m| m.id).collect();
        let actual: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            actual, expected,
            "an unknown default must leave the full catalog in catalog order"
        );
        // Catalog head is the current frontier, gpt-5.5.
        assert_eq!(entries[0].id, "gpt-5.5");
    }

    #[test]
    fn capabilities_differ_per_model_where_catalog_says_so() {
        let entries = build_provider_entries("gpt-5.4", REQUEST_TOPIC, STREAM_TOPIC);
        let cap_of = |id: &str| -> Vec<String> {
            entries
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing entry {id}"))
                .capabilities
                .clone()
        };

        // o3-mini lacks vision; gpt-5.4 has it.
        assert!(!cap_of("o3-mini").contains(&"vision".to_string()));
        assert!(cap_of("gpt-5.4").contains(&"vision".to_string()));

        // gpt-5.3 is not a reasoning model; o3 is.
        assert!(!cap_of("gpt-5.3").contains(&"reasoning".to_string()));
        assert!(cap_of("o3").contains(&"reasoning".to_string()));
    }

    #[test]
    fn context_window_and_max_output_match_catalog_row() {
        let entries = build_provider_entries("gpt-5.4", REQUEST_TOPIC, STREAM_TOPIC);
        let entry_of = |id: &str| -> &ProviderEntry {
            entries
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing entry {id}"))
        };

        let gpt54 = entry_of("gpt-5.4");
        assert_eq!(gpt54.context_window, 1_050_000);
        assert_eq!(gpt54.max_output_tokens, 128_000);

        let gpt4o = entry_of("gpt-4o");
        assert_eq!(gpt4o.context_window, 128_000);
        assert_eq!(gpt4o.max_output_tokens, 16_384);
    }

    #[test]
    fn provider_entry_serializes_with_wit_field_names() {
        let entries = build_provider_entries("gpt-5.4", REQUEST_TOPIC, STREAM_TOPIC);
        let value = serde_json::to_value(&entries[0]).expect("entry serializes");
        let obj = value.as_object().expect("entry is a JSON object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "capabilities",
                "context_window",
                "description",
                "id",
                "max_output_tokens",
                "request_topic",
                "stream_topic",
            ]
        );
        assert!(
            !obj.contains_key("models"),
            "ad-hoc `models` field must not be serialized"
        );
    }
}
