#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![allow(missing_docs)]

//! Capsule-authoring forge for Unicity AOS.
//!
//! Gives a fresh LLM the tools and Skills to understand Unicity AOS, build
//! capsules and harnesses safely on AOS from zero knowledge: inspect contracts,
//! scaffold a compiling skeleton, map an intent to manifest capabilities,
//! validate a `Capsule.toml`, and diagnose an installation.
//!
//! All operations go through the kernel's VFS and capability system — the
//! capsule cannot bypass sandbox boundaries.
//!
//! # Tools
//!
//! - `forge_quickstart` — the inline build-your-first-capsule guide
//! - `forge_guide` — progressively load the exhaustive author reference
//! - `meta_harness_quickstart` — how to build a governed meta-harness on AOS
//! - `scaffold_capsule` — a complete compiling tool-capsule skeleton as JSON
//! - `explain_interface` — read a WIT contract + a prose summary
//! - `suggest_capabilities` — map an intent to the exact manifest lines
//! - `validate_manifest` — lint a Capsule.toml for the common mistakes
//! - `capsule_doctor` — diagnose an installed capsule

mod checks;
mod scaffold;

use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

/// Capsule directory under the principal home (FHS layout).
const CAPSULES_DIR: &str = "home://.local/capsules";

/// Standard WIT interface directory — per-principal, accessible via `home://wit/`.
const WIT_DIR: &str = "home://wit";

/// Inline quickstart returned by `forge_quickstart` — the condensed front door.
const QUICKSTART_MD: &str = include_str!("quickstart.md");

/// Inline meta-harness bootstrap returned by `meta_harness_quickstart`.
const META_HARNESS_QUICKSTART_MD: &str = include_str!("meta_harness_quickstart.md");

/// One progressively disclosed chapter of the author manual.
struct GuideChapter {
    topic: &'static str,
    summary: &'static str,
    content: &'static str,
}

/// The authoritative agent-facing author manual. Keep the trigger Skill short;
/// Forge serves these chapters only when the current work reaches them.
const GUIDE_CHAPTERS: &[GuideChapter] = &[
    GuideChapter {
        topic: "foundations",
        summary: "AOS, Astrid Runtime, capsules, skills, harnesses, plugins, and artifact choice",
        content: include_str!("guides/foundations.md"),
    },
    GuideChapter {
        topic: "workspace",
        summary: "portable source locations, repository discovery, scratch candidates, and ownership",
        content: include_str!("guides/workspace.md"),
    },
    GuideChapter {
        topic: "capsule",
        summary: "project anatomy, Rust macros, SDK modules, lifecycle, state, and principals",
        content: include_str!("guides/capsule.md"),
    },
    GuideChapter {
        topic: "manifest",
        summary: "complete Capsule.toml surface, packaging, environment, commands, and MCP",
        content: include_str!("guides/manifest.md"),
    },
    GuideChapter {
        topic: "capabilities",
        summary: "all capability fields, least authority, VFS paths, and host-call gates",
        content: include_str!("guides/capabilities.md"),
    },
    GuideChapter {
        topic: "ipc",
        summary: "tool flow, topics, ACL matching, handlers, fan-out, layering, and priority",
        content: include_str!("guides/ipc.md"),
    },
    GuideChapter {
        topic: "wit",
        summary: "typed contracts, imports, exports, composition, and interface inspection",
        content: include_str!("guides/wit.md"),
    },
    GuideChapter {
        topic: "skills",
        summary: "natural Skill design, capsule distribution, host plugins, precedence, and references",
        content: include_str!("guides/skills.md"),
    },
    GuideChapter {
        topic: "authority",
        summary: "initiative, presentation, source changes, install, grants, consent, and activation",
        content: include_str!("guides/authority.md"),
    },
    GuideChapter {
        topic: "build",
        summary: "scaffold, build, install, grant, test, diagnose, upgrade, and release loops",
        content: include_str!("guides/build.md"),
    },
    GuideChapter {
        topic: "security",
        summary: "untrusted inputs, failure semantics, limits, reliability, evaluation, and review",
        content: include_str!("guides/security.md"),
    },
    GuideChapter {
        topic: "meta-harness",
        summary: "proactive world extension and building a meta-harness from first principles",
        content: include_str!("guides/meta-harness.md"),
    },
];

/// Guidance appended to every `capsule_doctor` result. The describe fan-out
/// race (incomplete on first prompt after boot) is fixed in the current kernel,
/// so a tool that's in the manifest but invisible to the model points at a real
/// manifest problem, not a kernel race.
const DOCTOR_GUIDANCE: &str = "If a tool is declared in the manifest but the model \
isn't seeing it, it is almost certainly a manifest problem — check that each tool \
has its own `tool.v1.execute.<tool>` subscribe row with a `tool_execute_<tool>` \
handler, and that `tool.v1.execute.*.result` + `tool.v1.response.describe.*` are \
in [publish]. (The old describe fan-out race on first prompt after boot is fixed \
in the current kernel.)";

#[derive(Default)]
pub struct ForgeTools;

// ---------------------------------------------------------------------------
// Tool argument types
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct EmptyArgs {}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ForgeGuideArgs {
    /// Manual topic to read. Omit it, or use `index`, to list every topic.
    /// Topics: foundations, workspace, capsule, manifest, capabilities, ipc,
    /// wit, skills, authority, build, security, meta-harness.
    #[serde(default)]
    pub topic: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ScaffoldArgs {
    /// Capsule name, e.g. `aos-weather`. Becomes the crate name.
    pub name: String,
    /// Capsule kind. Only `"tool"` is supported in v1 (the default).
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ExplainArgs {
    /// WIT interface filename, e.g. `tool` or `llm.wit`.
    pub name: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct IntentArgs {
    /// Plain-English description of what the capsule should be able to do.
    pub intent: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ManifestArgs {
    /// The full contents of a `Capsule.toml` to lint.
    pub toml: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct DoctorArgs {
    /// Installed capsule name, e.g. `aos-system`.
    pub name: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Reject empty names and path-traversal characters in untrusted name args.
fn validate_name(name: &str) -> Result<(), SysError> {
    if name.is_empty() {
        return Err(SysError::ApiError("Name cannot be empty".into()));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(SysError::ApiError(
            "Invalid name — path traversal rejected".into(),
        ));
    }
    Ok(())
}

/// Serialize a JSON value to a pretty string, mapping the (impossible) error.
fn to_json(value: &Value) -> Result<String, SysError> {
    serde_json::to_string_pretty(value).map_err(|e| SysError::ApiError(format!("serialize: {e}")))
}

/// List sorted entry names under a VFS path.
fn list_entries(path: &str) -> Result<Vec<String>, SysError> {
    let entries = astrid_sdk::fs::read_dir(path)?;
    let mut names: Vec<String> = entries.map(|e| e.file_name().to_string()).collect();
    names.sort();
    Ok(names)
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[capsule]
impl ForgeTools {
    /// Get the build-your-first-capsule guide: the minimal file set, the
    /// build→install→verify loop, and the top footguns. Start here.
    #[astrid::tool("forge_quickstart")]
    pub fn forge_quickstart(&self, _args: EmptyArgs) -> Result<String, SysError> {
        Ok(QUICKSTART_MD.to_string())
    }

    /// Read one chapter of the exhaustive AOS author manual. Omit `topic` or
    /// pass `index` to list the chapters, then load only what the current work
    /// needs. This is the detailed reference behind the capsule-forge Skill.
    #[astrid::tool("forge_guide")]
    pub fn forge_guide(&self, args: ForgeGuideArgs) -> Result<String, SysError> {
        let topic = args.topic.as_deref().unwrap_or("index").trim();
        let normalized = topic.to_ascii_lowercase().replace(['_', ' '], "-");
        if normalized.is_empty() || matches!(normalized.as_str(), "index" | "list" | "help") {
            return Ok(guide_index());
        }

        if let Some(chapter) = GUIDE_CHAPTERS
            .iter()
            .find(|chapter| chapter.topic == normalized)
        {
            return Ok(chapter.content.to_string());
        }

        Ok(format!(
            "Unknown Forge guide topic '{topic}'.\n\n{}",
            guide_index()
        ))
    }

    /// Explain how AOS supervises platform workers, handles capability gaps,
    /// and uses Forge without allowing generated code to self-promote.
    #[astrid::tool("meta_harness_quickstart")]
    pub fn meta_harness_quickstart(&self, _args: EmptyArgs) -> Result<String, SysError> {
        Ok(META_HARNESS_QUICKSTART_MD.to_string())
    }

    /// Scaffold a complete, compiling tool-capsule skeleton. Returns a JSON
    /// object mapping each relative file path to its content — write them all,
    /// then `aos capsule build`. Only the `tool` kind is supported in v1.
    #[astrid::tool("scaffold_capsule")]
    pub fn scaffold_capsule(&self, args: ScaffoldArgs) -> Result<String, SysError> {
        let name = args.name.trim();
        validate_name(name)?;

        let kind = args.kind.as_deref().unwrap_or("tool");
        if kind != "tool" {
            return to_json(&json!({
                "note": format!("kind \"{kind}\" is not supported in v1; only \"tool\" is available."),
                "supported_kinds": ["tool"],
            }));
        }

        to_json(&scaffold::tool_skeleton(name))
    }

    /// Read a WIT interface contract from `home://wit/` and return the raw WIT
    /// plus a short prose summary (package, interfaces, records). If the file
    /// isn't found, lists the available interfaces.
    #[astrid::tool("explain_interface")]
    pub fn explain_interface(&self, args: ExplainArgs) -> Result<String, SysError> {
        let name = args.name.trim();
        validate_name(name)?;

        let filename = if name.ends_with(".wit") {
            name.to_string()
        } else {
            format!("{name}.wit")
        };
        let path = format!("{WIT_DIR}/{filename}");

        match astrid_sdk::fs::read_to_string(&path) {
            Ok(wit) => Ok(format_wit(&filename, &wit)),
            Err(_) => {
                let available = list_entries(WIT_DIR).unwrap_or_default();
                if let Some((bundle, wit)) =
                    find_declared_interface(interface_lookup_name(name), &available)
                {
                    return Ok(format!(
                        "Interface '{name}' is declared inside bundled file '{bundle}'.\n\n{}",
                        format_wit(&bundle, &wit)
                    ));
                }
                Ok(format!(
                    "Interface '{filename}' not found.\nAvailable interfaces:\n{}",
                    if available.is_empty() {
                        "(none — the WIT store fills from installed capsules; inspect the project's pinned WIT source or install the required provider)".to_string()
                    } else {
                        available.join("\n")
                    }
                ))
            }
        }
    }

    /// Map a plain-English intent to the exact Capsule.toml capability lines.
    /// Covers files, HTTP, TCP, KV, processes, sockets, identity, and calling
    /// or being an LLM provider (grounded in the real bus topics).
    #[astrid::tool("suggest_capabilities")]
    pub fn suggest_capabilities(&self, args: IntentArgs) -> Result<String, SysError> {
        let intent = args.intent.to_lowercase();
        let mut snippets = suggest_from_intent(&intent);
        if snippets.is_empty() {
            snippets.push(json!({
                "match": "(no keywords recognised)",
                "note": "Describe a concrete action: read/write files, http, tcp, kv, spawn a process, bind a socket, identity, or call/serve an LLM.",
                "manifest": "",
            }));
        }
        to_json(&json!({ "intent": args.intent, "suggestions": snippets }))
    }

    /// Lint a Capsule.toml string for the common mistakes a new author hits.
    /// Returns a JSON list of findings: { level, message, fix }.
    #[astrid::tool("validate_manifest")]
    pub fn validate_manifest(&self, args: ManifestArgs) -> Result<String, SysError> {
        let findings = checks::validate_manifest(&args.toml);
        serde_json::to_string_pretty(&findings)
            .map_err(|e| SysError::ApiError(format!("serialize: {e}")))
    }

    /// Diagnose an installed capsule: missing describe handler, missing result
    /// publish, tool subscribes with no handler, and unsatisfied imports.
    #[astrid::tool("capsule_doctor")]
    pub fn capsule_doctor(&self, args: DoctorArgs) -> Result<String, SysError> {
        let name = args.name.trim();
        validate_name(name)?;

        let manifest_path = format!("{CAPSULES_DIR}/{name}/Capsule.toml");
        let manifest = astrid_sdk::fs::read_to_string(&manifest_path).map_err(|_| {
            SysError::ApiError(format!(
                "Capsule '{name}' not found at {manifest_path}. Run the `list_capsules` tool (system capsule) to see installed names."
            ))
        })?;

        let mut findings = checks::validate_manifest(&manifest);
        diagnose_imports(name, &manifest, &mut findings);

        to_json(&json!({
            "capsule": name,
            "findings": findings,
            "guidance": DOCTOR_GUIDANCE,
        }))
    }
}

fn format_wit(filename: &str, wit: &str) -> String {
    let summary = summarize_wit(wit);
    format!("=== {filename} ===\n{wit}\n\n=== summary ===\n{summary}")
}

/// Find an interface by declaration when the installed WIT mirror uses a
/// bundle filename rather than one file per interface.
fn find_declared_interface(name: &str, available: &[String]) -> Option<(String, String)> {
    for filename in available.iter().filter(|file| file.ends_with(".wit")) {
        let path = format!("{WIT_DIR}/{filename}");
        let Ok(wit) = astrid_sdk::fs::read_to_string(&path) else {
            continue;
        };
        if wit_declares(&wit, name) {
            return Some((filename.clone(), wit));
        }
    }
    None
}

fn interface_lookup_name(name: &str) -> &str {
    name.strip_suffix(".wit").unwrap_or(name)
}

fn wit_declares(wit: &str, name: &str) -> bool {
    wit.lines().any(|line| {
        let line = line.trim();
        ["interface ", "world "]
            .iter()
            .any(|keyword| wit_decl_name(line, keyword).as_deref() == Some(name))
    })
}

fn guide_index() -> String {
    let mut index = String::from(
        "# Unicity AOS Author Manual\n\nCall `forge_guide` again with one `topic`. Load only the chapters relevant to the current decision.\n\n",
    );
    for chapter in GUIDE_CHAPTERS {
        index.push_str(&format!("- `{}` — {}\n", chapter.topic, chapter.summary));
    }
    index
}

// ---------------------------------------------------------------------------
// explain_interface: lightweight WIT summary
// ---------------------------------------------------------------------------

/// Parse a WIT file simply: the `package` line, `interface` names, and `record`
/// names. Not a real WIT parser — just enough to orient the reader.
fn summarize_wit(wit: &str) -> String {
    let mut package = None;
    let mut interfaces = Vec::new();
    let mut records = Vec::new();

    for line in wit.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("package ") {
            package = Some(rest.trim_end_matches(';').trim().to_string());
        } else if let Some(name) = wit_decl_name(line, "interface ") {
            interfaces.push(name);
        } else if let Some(name) = wit_decl_name(line, "record ") {
            records.push(name);
        }
    }

    let mut summary = String::new();
    summary.push_str(&format!(
        "package: {}\n",
        package.as_deref().unwrap_or("(none)")
    ));
    summary.push_str(&format!(
        "interfaces ({}): {}\n",
        interfaces.len(),
        if interfaces.is_empty() {
            "(none)".into()
        } else {
            interfaces.join(", ")
        }
    ));
    summary.push_str(&format!(
        "records ({}): {}",
        records.len(),
        if records.is_empty() {
            "(none)".into()
        } else {
            records.join(", ")
        }
    ));
    summary
}

/// Extract the declared name from a line like `interface foo {` or `record bar {`.
fn wit_decl_name(line: &str, keyword: &str) -> Option<String> {
    let rest = line.strip_prefix(keyword)?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

// ---------------------------------------------------------------------------
// suggest_capabilities: keyword -> manifest snippet
// ---------------------------------------------------------------------------

/// Map recognised keywords in the intent to manifest snippets. Each entry is
/// `{ match, manifest, note }`.
fn suggest_from_intent(intent: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let any = |needles: &[&str]| mentions_positive(intent, needles);

    if any(&["read file", "read files", "read a file", "load file"]) {
        out.push(cap(
            "read files",
            "[capabilities]\nfs_read = [\"home://\"]",
            "Read under home://. Narrow the prefix to the smallest path you need.",
        ));
    }
    if any(&["write file", "write files", "save file", "write to disk"]) {
        out.push(cap(
            "write files",
            "[capabilities]\nfs_write = [\"home://data/\"]",
            "Write under the narrowest prefix that contains the capsule's mutable data.",
        ));
    }
    if any(&[
        "http",
        "rest api",
        "web request",
        "fetch url",
        "call an api",
    ]) {
        out.push(cap(
            "outbound http",
            "[capabilities]\nnet = [\"api.example.com\"]",
            "Outbound HTTP via astrid_sdk::http. List each concrete hostname; use [\"*\"] only if the host set is open.",
        ));
    }
    if any(&["tcp", "socket connect", "outbound connection", "connect to"]) {
        out.push(cap(
            "outbound tcp",
            "[capabilities]\nnet_connect = [\"host:port\"]",
            "Raw outbound TCP. List the concrete host:port targets.",
        ));
    }
    if any(&[
        "key-value",
        "key value",
        "kv store",
        "persist state",
        "store state",
    ]) {
        out.push(cap("kv store", "# No capability entry required for ordinary capsule KV.",
            "The kv manifest field is reserved and not the active gate. astrid_sdk::kv is already scoped per capsule and principal; stateful tools also auto-persist self."));
    }
    if any(&["spawn", "run a process", "subprocess", "shell out", "exec"]) {
        out.push(cap("spawn a process", "[capabilities]\nhost_process = [\"git\", \"cargo\"]\n# add allow_persistent = true for reattachable persistent processes",
            "Name each allowed binary. allow_persistent adds host-owned reattachable processes."));
    }
    if any(&[
        "bind a socket",
        "listen",
        "unix socket",
        "server socket",
        "accept connections",
    ]) {
        out.push(cap("bind a socket", "[capabilities]\nnet_bind = [\"...\"]",
            "Bind a listening socket (e.g. a Unix-socket uplink). Rare; most capsules are bus-only."));
    }
    if any(&["identity", "sign", "signature", "keypair", "ed25519"]) {
        out.push(cap(
            "identity ops",
            "[capabilities]\nidentity = [\"resolve\"]",
            "Identity operations via astrid_sdk::identity. A list of resolve < link < admin (each implies the lesser).",
        ));
    }
    if any(&[
        "persistent process",
        "process survives",
        "outlive the capsule",
        "reattachable process",
    ]) {
        out.push(cap(
            "persistent host process",
            "[capabilities]\nhost_process = [\"program\"]\nallow_persistent = true",
            "Persistent process authority is an explicit boolean sub-grant on top of the executable allowlist.",
        ));
    }
    if any(&[
        "uplink",
        "platform bridge",
        "external message bridge",
        "publish as another principal",
    ]) {
        out.push(cap(
            "uplink",
            "[capabilities]\nuplink = true",
            "Uplink authority enables attributed publishing and long-lived execution semantics. Use it only for a real protocol edge.",
        ));
    }
    if any(&[
        "inject system prompt",
        "modify system prompt",
        "prompt injection hook",
    ]) {
        out.push(cap(
            "system prompt injection",
            "[capabilities]\nallow_prompt_injection = true",
            "This permits hook output to affect system-prompt construction. Ordinary tool responses do not need it.",
        ));
    }
    suggest_llm(intent, &mut out);
    out
}

/// Match an intent keyword unless the occurrence is in a plainly negated
/// clause. Suggestions are candidates, but common phrases such as "no identity
/// operations" must not manufacture an authority request.
fn mentions_positive(intent: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| {
        intent
            .match_indices(needle)
            .any(|(index, _)| !occurrence_is_negated(intent, index))
    })
}

fn occurrence_is_negated(intent: &str, index: usize) -> bool {
    let before = &intent[..index];
    let clause_start = [
        before.rfind('.').map_or(0, |value| value + 1),
        before.rfind(',').map_or(0, |value| value + 1),
        before.rfind(';').map_or(0, |value| value + 1),
        before.rfind('\n').map_or(0, |value| value + 1),
        before.rfind(" but ").map_or(0, |value| value + 5),
        before.rfind(" however ").map_or(0, |value| value + 9),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let clause = before[clause_start..].trim();
    [
        "no",
        "not",
        "never",
        "without",
        "avoid",
        "avoids",
        "avoiding",
        "exclude",
        "excluding",
        "don't",
        "doesn't",
        "do not",
        "does not",
        "will not",
        "won't",
    ]
    .iter()
    .any(|negation| {
        clause == *negation
            || clause
                .strip_prefix(negation)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            || clause
                .strip_suffix(negation)
                .is_some_and(|rest| rest.ends_with(char::is_whitespace))
    })
}

/// LLM provider / consumer suggestion, grounded in the real openai-compat topics.
fn suggest_llm(intent: &str, out: &mut Vec<Value>) {
    let provider = mentions_positive(
        intent,
        &[
            "be a provider",
            "llm provider",
            "serve an llm",
            "expose a model",
            "provide a model",
        ],
    );
    let consumer = mentions_positive(
        intent,
        &[
            "call an llm",
            "call the llm",
            "use an llm",
            "ask the model",
            "generate text",
        ],
    );

    if provider {
        out.push(cap(
            "be an LLM provider",
            "[exports]\n\"astrid:llm\" = \"1.0.0\"\n\n[capabilities]\nnet = [\"*\"]\n\n[publish]\n\"llm.v1.stream.<provider>\" = { wit = \"@unicity-astrid/wit/llm/stream-event\" }\n\"llm.v1.response.describe\" = { wit = \"@unicity-astrid/wit/llm/describe-response\" }\n\n[subscribe]\n\"llm.v1.request.describe\" = { wit = \"@unicity-astrid/wit/llm/describe-request\", handler = \"llm_describe\" }\n\"llm.v1.request.generate.<provider>\" = { wit = \"@unicity-astrid/wit/llm/generate-request\", handler = \"handle_llm_request\" }",
            "Export astrid:llm, subscribe generate.<provider> + request.describe, publish a stream + describe-response. The registry fans out to the unsuffixed describe topic. Mirrors aos-openai-compat.",
        ));
    }
    if consumer {
        out.push(cap(
            "call an LLM (consumer)",
            "[imports]\n\"astrid:llm\" = \"^1.0\"\n\n[publish]\n\"llm.v1.request.generate.*\" = { wit = \"@unicity-astrid/wit/llm/generate-request\" }\n\n[subscribe]\n\"llm.v1.stream.*\" = { wit = \"@unicity-astrid/wit/llm/stream-event\", handler = \"handle_llm_stream\" }",
            "Import astrid:llm, publish a generate request to the active provider's topic, subscribe its stream. Mirrors how aos-react drives a provider.",
        ));
    }
}

fn cap(matched: &str, manifest: &str, note: &str) -> Value {
    json!({ "match": matched, "manifest": manifest, "note": note })
}

// ---------------------------------------------------------------------------
// capsule_doctor: unsatisfied imports across installed capsules
// ---------------------------------------------------------------------------

/// Cross-check the target's `[imports]` against every installed capsule's
/// exported interfaces (from each `meta.json`). Adds an error finding per
/// import that nothing exports.
fn diagnose_imports(target: &str, manifest: &str, findings: &mut Vec<checks::Finding>) {
    let imports = manifest_imports(manifest);
    if imports.is_empty() {
        return;
    }
    let exports = all_installed_exports(target);
    for import in imports {
        let satisfied = exports.iter().any(|e| e == &import);
        if !satisfied {
            findings.push(checks::Finding {
                level: "error",
                message: format!("Import `{import}` is not exported by any installed capsule."),
                fix: format!("Install a capsule that exports `{import}` before this one boots."),
            });
        }
    }
}

/// Extract the import interface keys from a manifest `[imports]` table, e.g.
/// `"astrid:llm" = "^1.0"` → `astrid:llm`.
fn manifest_imports(manifest: &str) -> Vec<String> {
    manifest
        .parse::<toml::Value>()
        .ok()
        .as_ref()
        .and_then(|t| t.get("imports"))
        .and_then(toml::Value::as_table)
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

/// Gather every exported interface name across all installed capsules except
/// the target itself, reading each `meta.json` `exports` map.
fn all_installed_exports(target: &str) -> Vec<String> {
    let mut exports = Vec::new();
    let Ok(names) = list_entries(CAPSULES_DIR) else {
        return exports;
    };
    for name in names {
        if name == target {
            continue;
        }
        let meta_path = format!("{CAPSULES_DIR}/{name}/meta.json");
        let Ok(content) = astrid_sdk::fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        if let Some(map) = meta.get("exports").and_then(Value::as_object) {
            collect_export_keys(map, &mut exports);
        }
    }
    exports
}

/// Flatten a meta.json exports map `{ "astrid": { "llm": "1.0.0" } }` into
/// `["astrid:llm"]` to match the `[imports]` key shape.
fn collect_export_keys(map: &serde_json::Map<String, Value>, out: &mut Vec<String>) {
    for (ns, ifaces) in map {
        if let Some(obj) = ifaces.as_object() {
            for name in obj.keys() {
                out.push(format!("{ns}:{name}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ForgeGuideArgs, ForgeTools, GUIDE_CHAPTERS, META_HARNESS_QUICKSTART_MD, guide_index,
        interface_lookup_name, suggest_from_intent, wit_declares,
    };

    const CAPSULE_FORGE_SKILL: &str = include_str!("skills/capsule-forge/SKILL.md");
    const META_HARNESS_SKILL: &str = include_str!("skills/meta-harness/SKILL.md");

    #[test]
    fn interface_lookup_accepts_bare_and_wit_names() {
        assert_eq!(interface_lookup_name("tool"), "tool");
        assert_eq!(interface_lookup_name("tool.wit"), "tool");
    }

    #[test]
    fn guide_index_aliases_are_case_insensitive() {
        let forge = ForgeTools;
        for topic in ["Index", "LIST", "Help"] {
            let guide = forge
                .forge_guide(ForgeGuideArgs {
                    topic: Some(topic.to_string()),
                })
                .unwrap();
            assert!(guide.contains("# Unicity AOS Author Manual"));
        }
    }

    #[test]
    fn author_manual_is_progressive_and_complete() {
        assert!(CAPSULE_FORGE_SKILL.lines().count() < 500);
        assert!(CAPSULE_FORGE_SKILL.contains("Call `forge_guide`"));
        assert!(CAPSULE_FORGE_SKILL.contains("`references/<topic>.md`"));
        assert!(!CAPSULE_FORGE_SKILL.contains("this page is the whole map"));

        let index = guide_index();
        for chapter in GUIDE_CHAPTERS {
            assert!(index.contains(&format!("`{}`", chapter.topic)));
            assert!(
                chapter.content.lines().count() >= 35,
                "{} is too thin",
                chapter.topic
            );
        }
    }

    #[test]
    fn capability_manual_names_the_complete_current_surface() {
        let chapter = GUIDE_CHAPTERS
            .iter()
            .find(|chapter| chapter.topic == "capabilities")
            .unwrap()
            .content;
        for field in [
            "`uplink`",
            "`net`",
            "`kv`",
            "`fs_read`",
            "`fs_write`",
            "`host_process`",
            "`allow_persistent`",
            "`net_bind`",
            "`net_connect`",
            "`identity`",
            "`allow_prompt_injection`",
        ] {
            assert!(chapter.contains(field), "capability guide lost {field}");
        }
    }

    #[test]
    fn authority_manual_separates_construction_from_activation() {
        let chapter = GUIDE_CHAPTERS
            .iter()
            .find(|chapter| chapter.topic == "authority")
            .unwrap()
            .content;
        for required in [
            "create or edit a candidate",
            "compile the candidate",
            "install or replace the capsule",
            "principal use the capsule",
            "runtime effect",
            "Generated code cannot self-promote",
        ] {
            assert!(
                chapter.contains(required),
                "authority guide lost {required}"
            );
        }
    }

    #[test]
    fn ipc_manual_explains_priority_mode_switch_and_fail_open_errors() {
        let chapter = GUIDE_CHAPTERS
            .iter()
            .find(|chapter| chapter.topic == "ipc")
            .unwrap()
            .content;
        for required in [
            "concurrent fan-out",
            "ordered middleware chain",
            "ordinary handler error is logged and the chain continues",
            "must return `Deny`, not `Err`",
        ] {
            assert!(chapter.contains(required), "IPC guide lost {required}");
        }
    }

    #[test]
    fn capability_suggestions_ignore_plain_negation() {
        let suggestions = suggest_from_intent(
            "read files from the principal home, with no identity operations and without http.",
        );
        let rendered = serde_json::to_string(&suggestions).unwrap();
        assert!(rendered.contains("fs_read"));
        assert!(!rendered.contains("identity ="));
        assert!(!rendered.contains("net ="));
    }

    #[test]
    fn bundled_wit_can_be_found_by_declared_interface() {
        let bundle = "package astrid:contracts;\ninterface tool { run: func(); }\nworld capsule {}";
        assert!(wit_declares(bundle, "tool"));
        assert!(wit_declares(bundle, "capsule"));
        assert!(!wit_declares(bundle, "missing"));
    }

    #[test]
    fn meta_harness_bootstrap_teaches_proactive_world_extension() {
        for required in [
            "Unicity AOS is the operating system for agents",
            "your user-space world",
            "Reach for it proactively",
            "current objective and instructions as the anchor",
            "Decide whether the extension is needed inline",
            "Not every agent has subagents",
            "capabilities remain the operational boundary",
            "list_skills",
            "read_skill",
        ] {
            assert!(
                META_HARNESS_QUICKSTART_MD.contains(required),
                "meta-harness quickstart lost required boundary: {required}"
            );
        }
    }

    #[test]
    fn meta_harness_skill_teaches_reflexive_agent_judgment() {
        for required in [
            "name: meta-harness",
            "Treat the AOS user-space environment",
            "Exercise initiative",
            "Reach for the ability proactively",
            "The user's instruction sets the degree of freedom",
            "Worker or subagent",
            "optional pattern, not a prerequisite",
            "Improve harness code from experience",
            "Capsule-owned",
            "instructions; capsule grants and AOS policy still supply authority",
            "Definition of done",
        ] {
            assert!(
                META_HARNESS_SKILL.contains(required),
                "meta-harness skill lost required instruction: {required}"
            );
        }
    }
}
