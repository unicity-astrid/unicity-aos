//! Pure model-selection logic: canonical id handling, route-topic validation,
//! the ollama-safe resolver, and the reconcile/auto-select decisions. None of
//! this touches the host — `lib.rs` owns IPC/KV and wraps the decisions here
//! with persistence + event publishing. Keeping the logic host-free is what
//! lets it carry its own `#[cfg(test)]` units (capsule test binaries run on
//! the developer host, not in the wasm sandbox).

use astrid_sdk::contracts::registry::ProviderEntry;

/// Fixed prefix of a provider's request topic. The trailing segment after
/// this prefix is the provider qualifier used in canonical model ids.
const LLM_REQUEST_GENERATE_PREFIX: &str = "llm.v1.request.generate.";

/// The `models` subcommands the registry understands. Named so the dispatcher
/// can recognise a valid subcommand (and short-circuit an unknown one before
/// paying for provider discovery) without re-typing the literals.
pub(crate) const SUB_LIST: &str = "list";
pub(crate) const SUB_CURRENT: &str = "current";
pub(crate) const SUB_SET: &str = "set";
pub(crate) const SUB_UNSET: &str = "unset";

/// Whether `sub` is a `models` subcommand the registry handles. Used by the
/// dispatcher to reject an unknown/empty subcommand early — before provider
/// discovery — so a typo or `--help` query doesn't wait out the discovery
/// window.
pub(crate) fn is_known_subcommand(sub: &str) -> bool {
    matches!(sub, SUB_LIST | SUB_CURRENT | SUB_SET | SUB_UNSET)
}

/// Whether a `models` run needs the ~500ms provider-discovery fan-out to build
/// its reply, given the full `args` (subcommand + flags). Skipping discovery
/// when it can't affect the answer avoids stalling the dispatch loop:
///
/// - `list` and `current --json` enumerate / look up entries → need discovery.
/// - `set <id>` resolves the id against entries → needs discovery.
/// - `set` (missing id) replies with a usage error, independent of entries.
/// - `unset` clears the stored binding and replies a fixed string.
/// - `current` (no `--json`) reports only the stored `active_model_id` string.
///
/// The last three never consult discovered entries, so they short-circuit the
/// fan-out. (An unknown subcommand is already rejected upstream.)
pub(crate) fn subcommand_needs_discovery(args: &[String]) -> bool {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let wants_json = args.iter().any(|a| a == "--json");
    match sub {
        SUB_LIST => true,
        SUB_CURRENT => wants_json,
        // `set` needs discovery only when an id is actually present to resolve.
        SUB_SET => args.get(1).is_some(),
        SUB_UNSET => false,
        _ => false,
    }
}

/// The persisted registry state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct RegistryState {
    pub(crate) providers: Vec<ProviderEntry>,
    pub(crate) active_model_id: Option<String>,
}

/// The `<capsule>` qualifier of a canonical `"<capsule>:<model>"` id —
/// everything before the FIRST `':'`. Returns the whole string when there
/// is no colon (degenerate/old-form ids).
pub(crate) fn qualifier_of(id: &str) -> &str {
    id.split_once(':').map_or(id, |(cap, _)| cap)
}

/// The bare model of a canonical `"<capsule>:<model>"` id — everything after
/// the FIRST `':'`. ollama model names contain colons
/// (`"ollama:llama3.3:70b"` -> `"llama3.3:70b"`), so we only ever split on
/// the first separator and keep the remainder verbatim. Returns the whole
/// string when there is no colon.
pub(crate) fn bare_model_of(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, model)| model)
}

/// Why a selection input failed to bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// The input matched no entry.
    Unknown { input: String },
    /// The input matched more than one entry; carries the qualified
    /// candidate ids so the operator-facing surface can echo them.
    Ambiguous { candidates: Vec<String> },
}

impl ResolveError {
    /// Render for the CLI/IPC error surface (the ambiguous list is echoed so
    /// the operator can disambiguate with a qualified id).
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Unknown { input } => format!("unknown model: {input}"),
            Self::Ambiguous { candidates } => {
                format!("ambiguous model; candidates: {}", candidates.join(", "))
            }
        }
    }
}

/// Resolve an operator selection `input` against canonical entries.
///
/// Each entry `id` is canonical `"<capsule>:<model>"`. The colon is NOT a
/// reliable separator (ollama models embed colons), so we match structurally:
///
/// 0. **Exact canonical pass.** If any entry's canonical `id` equals `input`,
///    it binds immediately — a canonical id is unique by construction and
///    always wins, even when another entry's *bare* model happens to equal
///    the same string.
/// 1. **Bare pass (no splitting).** An entry binds when its bare model (`id`
///    after the first `':'`) equals `input`. (The exact canonical `id == input`
///    case is already covered by Pass 0.) One match binds; several ->
///    `Ambiguous` (the qualified ids).
/// 2. **Qualified pass** (only when the exact pass found nothing AND `input`
///    contains `':'`): treat `input` as `"<capsule>:<model>"` split on the
///    FIRST `':'`, and match entries whose qualifier and bare model both
///    equal those halves. One binds; zero -> `Unknown`; several -> `Ambiguous`.
/// 3. Otherwise `Unknown`.
pub(crate) fn resolve_selection<'a>(
    input: &str,
    entries: &'a [ProviderEntry],
) -> Result<&'a ProviderEntry, ResolveError> {
    // Pass 0: an exact canonical id is unique by construction and always
    // wins. Short-circuit before the combined filter below, otherwise an
    // input that *is* a canonical id can be dragged into `Ambiguous` when a
    // second entry's bare model happens to equal that same string.
    if let Some(exact) = entries.iter().find(|e| e.id == input) {
        return Ok(exact);
    }

    // Pass 1: bare-model match, no splitting. The exact canonical `id == input`
    // case is already handled by the Pass 0 short-circuit above, so this filter
    // only needs the bare-model check.
    let exact: Vec<&ProviderEntry> = entries
        .iter()
        .filter(|e| bare_model_of(&e.id) == input)
        .collect();
    match exact.as_slice() {
        [one] => return Ok(one),
        [] => {}
        many => {
            return Err(ResolveError::Ambiguous {
                candidates: many.iter().map(|e| e.id.clone()).collect(),
            });
        }
    }

    // Pass 2: qualified, only when the exact pass found nothing and the input
    // carries a separator. Split on the FIRST colon so an ollama model
    // (`"ollama:llama3.3:70b"`) keeps its embedded colons in the model half.
    if let Some((cap, model)) = input.split_once(':') {
        let qualified: Vec<&ProviderEntry> = entries
            .iter()
            .filter(|e| qualifier_of(&e.id) == cap && bare_model_of(&e.id) == model)
            .collect();
        match qualified.as_slice() {
            [one] => return Ok(one),
            [] => {}
            many => {
                return Err(ResolveError::Ambiguous {
                    candidates: many.iter().map(|e| e.id.clone()).collect(),
                });
            }
        }
    }

    Err(ResolveError::Unknown {
        input: input.to_string(),
    })
}

/// Extract a provider qualifier from a routable LLM request topic.
///
/// The provider self-reports its routing as
/// `request_topic = "llm.v1.request.generate.<provider>"`. The registry uses
/// that trailing segment as the human-facing qualifier in canonical ids, but
/// it must not force the qualifier to equal the capsule package name. Topics
/// are routing contracts, not capsule identity envelopes; provider provenance
/// belongs in the kernel-stamped message envelope.
///
/// Returns the `<provider>` qualifier, or `None` when the topic does not name
/// a concrete generate route.
pub(crate) fn request_topic_qualifier(request_topic: &str) -> Option<&str> {
    let candidate = request_topic.strip_prefix(LLM_REQUEST_GENERATE_PREFIX)?;
    if !is_valid_provider_qualifier(candidate) {
        return None;
    }
    Some(candidate)
}

fn is_valid_provider_qualifier(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// What a reconcile decided to do with a stored `active_model_id`. Returned by
/// the pure [`reconcile_active_model_in_place`] so the host wrapper knows
/// whether to persist + which event (if any) to publish, and so the decision
/// is unit-testable without touching the host.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReconcileOutcome {
    /// Stored id still resolves (or was already `None`); no change.
    Unchanged,
    /// Stored bare provider-id form was remapped to a capsule default.
    Remapped { from: String, to: ProviderEntry },
    /// Stored id is genuinely gone; cleared to `None`.
    Cleared { from: String },
}

/// Pure core of `reconcile_active_model`: mutates `state.active_model_id` in
/// memory and reports what it did. No host calls — testable against an
/// in-memory `RegistryState`.
///
/// - **Remap** when the stored id is the OLD bare provider-id form (e.g.
///   `"openai-compat"`) and some discovered entry's `<capsule>` qualifier
///   equals it — bind that capsule's DEFAULT model, which by the cross-capsule
///   ordering convention is the FIRST entry in that capsule's group. This
///   preserves the principal's binding across a single->multi upgrade instead
///   of dropping it.
/// - **Clear** only when no entry matches and the stored id is not a
///   recognizable capsule qualifier (the model is genuinely gone).
pub(crate) fn reconcile_active_model_in_place(state: &mut RegistryState) -> ReconcileOutcome {
    let Some(id) = state.active_model_id.clone() else {
        return ReconcileOutcome::Unchanged;
    };

    // No providers means discovery has not run yet OR transiently failed — we
    // CANNOT judge staleness without entries to compare against. Clearing here
    // would permanently wipe a valid selection on a single empty discovery
    // window. Leave the stored binding untouched; the next run with a populated
    // provider set reconciles it.
    if state.providers.is_empty() {
        return ReconcileOutcome::Unchanged;
    }

    // Already resolves by exact canonical match — nothing to do.
    if state.providers.iter().any(|p| p.id == id) {
        return ReconcileOutcome::Unchanged;
    }

    // Remap: the stored id is the old bare provider-id form and matches some
    // capsule's qualifier. Bind that capsule's default = its first entry in
    // emit order (entries are stored per-capsule, entry[0]-first).
    if let Some(default) = state
        .providers
        .iter()
        .find(|p| qualifier_of(&p.id) == id)
        .cloned()
    {
        state.active_model_id = Some(default.id.clone());
        return ReconcileOutcome::Remapped {
            from: id,
            to: default,
        };
    }

    // Genuinely gone — clear.
    state.active_model_id = None;
    ReconcileOutcome::Cleared { from: id }
}

/// Pure core of `auto_select_defaults`: mutates `state.active_model_id` in
/// memory and returns the selected entry (or `None` if nothing changed). No
/// host calls — testable against an in-memory `RegistryState`.
///
/// Entries are stored in per-capsule, entry[0]-first order, so `providers[0]`
/// is the first-discovered capsule's default-hint model. Replaces the old
/// `len == 1` gate, which never fired once any provider emitted multiple
/// entries. Discovery order is not authority-ranked, so any deterministic
/// choice is acceptable for an *initial* auto-select; the operator overrides
/// via `models set`. The contract that matters: a fresh principal lands on a
/// resolvable, default-hint model rather than `None`.
pub(crate) fn auto_select_defaults_in_place(state: &mut RegistryState) -> Option<ProviderEntry> {
    if state.active_model_id.is_some() {
        return None;
    }
    let provider = state.providers.first().cloned()?;
    state.active_model_id = Some(provider.id.clone());
    Some(provider)
}

/// Serialize one entry for the machine-readable `--json` output. Mirrors the
/// canonical `ProviderEntry` shape with the now-canonical `id`.
fn entry_to_json(e: &ProviderEntry) -> serde_json::Value {
    serde_json::json!({
        "id": e.id,
        "description": e.description,
        "request_topic": e.request_topic,
        "stream_topic": e.stream_topic,
        "capabilities": e.capabilities,
        "context_window": e.context_window,
        "max_output_tokens": e.max_output_tokens,
    })
}

/// Render a human-readable `list` table, marking the active entry with `*`.
fn render_list_table(entries: &[ProviderEntry], active: Option<&str>) -> String {
    if entries.is_empty() {
        return "No LLM models available".to_string();
    }
    let mut out = String::new();
    for e in entries {
        let marker = if active == Some(e.id.as_str()) {
            "* "
        } else {
            "  "
        };
        out.push_str(&format!("{marker}{}  {}\n", e.id, e.description));
    }
    // Trim the trailing newline so the CLI doesn't print a blank line.
    out.pop();
    out
}

/// Build the CLI `{ exit_code, output, error? }` result body for a `models`
/// run, with NO IPC or KV access — the dispatch wrapper supplies `entries`
/// (from discovery) and `active`, then publishes whatever this returns. Kept
/// pure so the body shape is unit-testable.
///
/// `args[0]` selects the subcommand: `list [--json]`, `current [--json]`,
/// `set <id>`, `unset`. `set`/`unset` mutate state in the wrapper; here they
/// only validate input and SHAPE the reply — `set` resolves against `entries`
/// so the success/failure body (and the canonical id it confirms) is testable.
pub(crate) fn models_result(
    args: &[String],
    entries: &[ProviderEntry],
    active: Option<&str>,
) -> serde_json::Value {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let wants_json = args.iter().any(|a| a == "--json");

    match sub {
        SUB_LIST => {
            if wants_json {
                let arr: Vec<serde_json::Value> = entries.iter().map(entry_to_json).collect();
                ok_output(serde_json::Value::Array(arr).to_string())
            } else {
                ok_output(render_list_table(entries, active))
            }
        }
        SUB_CURRENT => {
            if wants_json {
                let entry = active.and_then(|id| entries.iter().find(|e| e.id == id));
                let body = serde_json::json!({ "active": entry.map(entry_to_json) });
                ok_output(body.to_string())
            } else {
                ok_output(active.unwrap_or("none").to_string())
            }
        }
        SUB_SET => {
            let Some(input) = args.get(1) else {
                return err_output("usage: models set <id>".to_string());
            };
            match resolve_selection(input, entries) {
                Ok(entry) => ok_output(format!("active model set to {}", entry.id)),
                Err(e) => err_output(e.message()),
            }
        }
        SUB_UNSET => ok_output("active model cleared".to_string()),
        "" => err_output("usage: models <list|current|set|unset> [--json]".to_string()),
        other => err_output(format!(
            "unknown subcommand '{other}'; usage: models <list|current|set|unset> [--json]"
        )),
    }
}

/// A success result body (`exit_code 0` + `output`).
fn ok_output(output: String) -> serde_json::Value {
    serde_json::json!({ "exit_code": 0, "output": output })
}

/// A failure result body (`exit_code 1` + `error`).
fn err_output(error: String) -> serde_json::Value {
    serde_json::json!({ "exit_code": 1, "error": error })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `ProviderEntry` with a canonical id and a matching
    /// `request_topic` so resolver tests read like the real discovered shape.
    fn entry(id: &str) -> ProviderEntry {
        let capsule = qualifier_of(id);
        ProviderEntry {
            id: id.to_string(),
            description: format!("test entry {id}"),
            request_topic: format!("{LLM_REQUEST_GENERATE_PREFIX}{capsule}"),
            stream_topic: format!("llm.v1.stream.{capsule}"),
            capabilities: vec!["text".to_string()],
            context_window: Some(128_000),
            max_output_tokens: Some(8_192),
        }
    }

    #[test]
    fn resolve_exact_canonical_wins() {
        let entries = vec![entry("openai-compat:gpt-5.4"), entry("openai:o3")];
        let got = resolve_selection("openai-compat:gpt-5.4", &entries).expect("binds");
        assert_eq!(got.id, "openai-compat:gpt-5.4");
    }

    #[test]
    fn resolve_exact_canonical_beats_colliding_bare() {
        // The first entry's canonical id IS the input. The second entry's bare
        // model (`id` after the first colon) ALSO equals the input. The exact
        // canonical match must win and bind the first entry uniquely — without
        // the Pass 0 short-circuit the combined filter would collect both and
        // return `Ambiguous`.
        let entries = vec![
            entry("openai-compat:gpt-5.4"),
            entry("other:openai-compat:gpt-5.4"),
        ];
        let got = resolve_selection("openai-compat:gpt-5.4", &entries).expect("binds");
        assert_eq!(got.id, "openai-compat:gpt-5.4");
    }

    #[test]
    fn resolve_bare_unique_binds() {
        let entries = vec![entry("openai-compat:gpt-5.4"), entry("openai:o3")];
        let got = resolve_selection("gpt-5.4", &entries).expect("binds");
        // The resolved entry's canonical id is the qualified form.
        assert_eq!(got.id, "openai-compat:gpt-5.4");
    }

    #[test]
    fn resolve_ollama_bare_colon_name() {
        // Entry canonical id embeds a colon in the model half.
        let entries = vec![entry("ollama:llama3.3:70b"), entry("openai:o3")];
        let got = resolve_selection("llama3.3:70b", &entries).expect("binds via bare pass");
        // Bound the ollama entry, NOT misread as capsule "llama3.3" / model "70b".
        assert_eq!(got.id, "ollama:llama3.3:70b");
    }

    #[test]
    fn resolve_qualified_only_on_miss() {
        let entries = vec![entry("ollama:llama3.3:70b"), entry("openai:o3")];
        // Fully qualified input: first-colon split yields capsule "ollama",
        // model "llama3.3:70b" (embedded colon preserved).
        let got = resolve_selection("ollama:llama3.3:70b", &entries).expect("binds via qualified");
        assert_eq!(got.id, "ollama:llama3.3:70b");
    }

    #[test]
    fn resolve_ambiguous_bare_errors() {
        let entries = vec![entry("openai:gpt-5.4"), entry("openai-compat:gpt-5.4")];
        let err = resolve_selection("gpt-5.4", &entries).expect_err("ambiguous");
        match err {
            ResolveError::Ambiguous { mut candidates } => {
                candidates.sort();
                assert_eq!(candidates, vec!["openai-compat:gpt-5.4", "openai:gpt-5.4"]);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn request_topic_qualifier_accepts_provider_alias() {
        assert_eq!(
            request_topic_qualifier("llm.v1.request.generate.openai-compat"),
            Some("openai-compat")
        );
        assert_eq!(
            request_topic_qualifier("llm.v1.request.generate.aos-openai-compat"),
            Some("aos-openai-compat")
        );
        assert_eq!(request_topic_qualifier("llm.v1.request.generate."), None);
        assert_eq!(request_topic_qualifier("llm.v1.request.generate.*"), None);
        assert_eq!(
            request_topic_qualifier("llm.v1.request.generate.openai.compat"),
            None
        );
        assert_eq!(
            request_topic_qualifier("llm.v1.request.generate.openai compat"),
            None
        );
        assert_eq!(request_topic_qualifier("some.other.topic"), None);
    }

    #[test]
    fn self_heal_remaps_bare_provider_id_after_upgrade() {
        // Old single-entry selection, now upgraded to per-model entries.
        let mut state = RegistryState {
            providers: vec![
                entry("openai-compat:gpt-4o"),
                entry("openai-compat:gpt-5.4"),
            ],
            active_model_id: Some("openai-compat".to_string()),
        };
        let outcome = reconcile_active_model_in_place(&mut state);
        // Remapped to the entry[0] default, NOT cleared and NOT the second entry.
        assert_eq!(
            outcome,
            ReconcileOutcome::Remapped {
                from: "openai-compat".to_string(),
                to: entry("openai-compat:gpt-4o"),
            }
        );
        assert_eq!(
            state.active_model_id.as_deref(),
            Some("openai-compat:gpt-4o")
        );
    }

    #[test]
    fn reconcile_keeps_selection_when_providers_empty() {
        // Discovery has not run yet OR transiently failed — providers is empty.
        // Reconcile MUST NOT clear a valid stored selection: it cannot judge
        // staleness with nothing to compare against, and clearing here would
        // permanently wipe the binding on a single empty discovery window.
        let mut state = RegistryState {
            providers: vec![],
            active_model_id: Some("openai-compat:gpt-4o".to_string()),
        };
        let outcome = reconcile_active_model_in_place(&mut state);
        assert_eq!(outcome, ReconcileOutcome::Unchanged);
        assert_eq!(
            state.active_model_id.as_deref(),
            Some("openai-compat:gpt-4o"),
            "selection must survive an empty-provider reconcile"
        );
    }

    #[test]
    fn self_heal_clears_genuinely_gone() {
        let mut state = RegistryState {
            providers: vec![entry("openai-compat:gpt-4o")],
            active_model_id: Some("deleted-cap:foo".to_string()),
        };
        let outcome = reconcile_active_model_in_place(&mut state);
        assert_eq!(
            outcome,
            ReconcileOutcome::Cleared {
                from: "deleted-cap:foo".to_string(),
            }
        );
        assert_eq!(state.active_model_id, None);
    }

    #[test]
    fn auto_select_picks_first_capsule_default_when_multi_model() {
        // providers.len() == 3: the old `len == 1` gate would never fire here.
        let mut state = RegistryState {
            providers: vec![
                entry("openai-compat:gpt-4o"),
                entry("openai-compat:gpt-5.4"),
                entry("openai:o3"),
            ],
            active_model_id: None,
        };
        let selected = auto_select_defaults_in_place(&mut state).expect("auto-selected");
        assert_eq!(selected.id, "openai-compat:gpt-4o");
        assert_eq!(
            state.active_model_id.as_deref(),
            Some("openai-compat:gpt-4o")
        );
    }

    #[test]
    fn auto_select_noop_when_already_selected() {
        let mut state = RegistryState {
            providers: vec![entry("openai-compat:gpt-4o"), entry("openai:o3")],
            active_model_id: Some("openai:o3".to_string()),
        };
        let selected = auto_select_defaults_in_place(&mut state);
        assert!(selected.is_none());
        assert_eq!(state.active_model_id.as_deref(), Some("openai:o3"));
    }

    #[test]
    fn cli_run_protocol_roundtrip() {
        let entries = vec![entry("openai-compat:gpt-4o"), entry("openai:o3")];

        // `list --json` -> exit 0, output parses as a JSON array of entries.
        let list = models_result(
            &["list".to_string(), "--json".to_string()],
            &entries,
            Some("openai-compat:gpt-4o"),
        );
        assert_eq!(list["exit_code"], 0);
        let output = list["output"].as_str().expect("output string");
        let arr: Vec<serde_json::Value> = serde_json::from_str(output).expect("json array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "openai-compat:gpt-4o");

        // `set <unknown>` -> exit 1 with an error message.
        let set = models_result(
            &["set".to_string(), "no-such-model".to_string()],
            &entries,
            None,
        );
        assert_eq!(set["exit_code"], 1);
        assert!(
            set["error"]
                .as_str()
                .expect("error string")
                .contains("unknown model")
        );
    }

    #[test]
    fn discovery_skipped_for_entry_independent_subcommands() {
        let a = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Need discovery: enumerate / look up / resolve against entries.
        assert!(subcommand_needs_discovery(&a(&["list"])));
        assert!(subcommand_needs_discovery(&a(&["list", "--json"])));
        assert!(subcommand_needs_discovery(&a(&["current", "--json"])));
        assert!(subcommand_needs_discovery(&a(&["set", "gpt-5.4"])));

        // Skip discovery: reply is independent of discovered entries.
        assert!(!subcommand_needs_discovery(&a(&["unset"])));
        assert!(!subcommand_needs_discovery(&a(&["set"]))); // missing id -> usage error
        assert!(!subcommand_needs_discovery(&a(&["current"]))); // stored id string only
    }

    #[test]
    fn persisted_form_is_canonical_never_bare() {
        let entries = vec![entry("openai-compat:gpt-5.4"), entry("openai:o3")];
        // Resolving a bare unique input yields the canonical id for persistence.
        let resolved = resolve_selection("gpt-5.4", &entries).expect("binds");
        assert_eq!(resolved.id, "openai-compat:gpt-5.4");
        assert_ne!(resolved.id, "gpt-5.4");
    }
}
