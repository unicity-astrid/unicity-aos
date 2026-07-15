#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![allow(missing_docs)]

//! Capsule-authoring forge for Unicity AOS.
//!
//! Gives a fresh LLM the tools and a Skill to build capsules from zero
//! knowledge: scaffold a compiling skeleton, read WIT contracts, map an intent
//! to the exact manifest capabilities, validate a `Capsule.toml`, and diagnose a
//! capsule that loaded but whose tools don't appear.
//!
//! All operations go through the kernel's VFS and capability system — the
//! capsule cannot bypass sandbox boundaries.
//!
//! # Tools
//!
//! - `forge_quickstart` — the inline build-your-first-capsule guide
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

/// Skill installed to `home://skills/capsule-forge/SKILL.md` on install.
/// Named `capsule-forge` (not `capsule-development`) to avoid colliding with
/// the skill the system capsule already ships.
const CAPSULE_FORGE_SKILL: &str = include_str!("skills/capsule-forge/SKILL.md");

/// Inline quickstart returned by `forge_quickstart` — the condensed front door.
const QUICKSTART_MD: &str = include_str!("quickstart.md");

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
pub struct ScaffoldArgs {
    /// Capsule name, e.g. `astrid-capsule-weather`. Becomes the crate name.
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
    /// Installed capsule name, e.g. `astrid-capsule-system`.
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
    /// Write the capsule-forge Skill to `home://skills/capsule-forge/SKILL.md`
    /// so the skills capsule can surface it to the LLM.
    #[astrid::install]
    pub fn on_install(&self) -> Result<(), SysError> {
        // home:// may be unavailable during lifecycle dispatch when installing
        // without a running daemon; ignore host errors so the skill is written
        // on the next full boot once the principal home is mounted.
        let _ = astrid_sdk::fs::create_dir_all("home://skills/capsule-forge");
        let _ = astrid_sdk::fs::write(
            "home://skills/capsule-forge/SKILL.md",
            CAPSULE_FORGE_SKILL.as_bytes(),
        );
        Ok(())
    }

    /// Get the build-your-first-capsule guide: the minimal file set, the
    /// build→install→verify loop, and the top footguns. Start here.
    #[astrid::tool("forge_quickstart")]
    pub fn forge_quickstart(&self, _args: EmptyArgs) -> Result<String, SysError> {
        Ok(QUICKSTART_MD.to_string())
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
            Ok(wit) => {
                let summary = summarize_wit(&wit);
                Ok(format!(
                    "=== {filename} ===\n{wit}\n\n=== summary ===\n{summary}"
                ))
            }
            Err(_) => {
                let available = list_entries(WIT_DIR).unwrap_or_default();
                Ok(format!(
                    "Interface '{filename}' not found.\nAvailable interfaces:\n{}",
                    if available.is_empty() {
                        "(none — the WIT store fills as capsules are installed; run `aos init` to install Unicity CE)".to_string()
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
    let any = |needles: &[&str]| needles.iter().any(|n| intent.contains(n));

    if any(&["read file", "read files", "read a file", "load file"]) {
        out.push(cap(
            "read files",
            "[capabilities]\nfs_read = [\"home://\"]",
            "Read under home://. Narrow the prefix to the smallest path you need.",
        ));
    }
    if any(&["write file", "write files", "save file", "write to disk"]) {
        out.push(cap("write files", "[capabilities]\nfs_write = [\"home://data/\"]",
            "Write under a narrow prefix. fs_write to home://skills/ is how a capsule ships a Skill."));
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
        out.push(cap("kv store", "[capabilities]\nkv = [\"mystore\"]",
            "Reserved (not yet gate-enforced): per-capsule KV via astrid_sdk::kv already works without declaring it (auto-scoped per capsule + principal). Stateful tools also auto-persist self."));
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
    suggest_llm(intent, &mut out);
    out
}

/// LLM provider / consumer suggestion, grounded in the real openai-compat topics.
fn suggest_llm(intent: &str, out: &mut Vec<Value>) {
    let provider = [
        "be a provider",
        "llm provider",
        "serve an llm",
        "expose a model",
        "provide a model",
    ]
    .iter()
    .any(|n| intent.contains(n));
    let consumer = [
        "call an llm",
        "call the llm",
        "use an llm",
        "ask the model",
        "generate text",
    ]
    .iter()
    .any(|n| intent.contains(n));

    if provider {
        out.push(cap(
            "be an LLM provider",
            "[exports]\n\"astrid:llm\" = \"1.0.0\"\n\n[capabilities]\nnet = [\"*\"]\n\n[publish]\n\"llm.v1.stream.<provider>\" = { wit = \"@unicity-astrid/wit/llm/stream-event\" }\n\"llm.v1.response.describe\" = { wit = \"@unicity-astrid/wit/llm/describe-response\" }\n\n[subscribe]\n\"llm.v1.request.describe\" = { wit = \"@unicity-astrid/wit/llm/describe-request\", handler = \"llm_describe\" }\n\"llm.v1.request.generate.<provider>\" = { wit = \"@unicity-astrid/wit/llm/generate-request\", handler = \"handle_llm_request\" }",
            "Export astrid:llm, subscribe generate.<provider> + request.describe, publish a stream + describe-response. The registry fans out to the unsuffixed describe topic. Mirrors astrid-capsule-openai-compat.",
        ));
    }
    if consumer {
        out.push(cap(
            "call an LLM (consumer)",
            "[imports]\n\"astrid:llm\" = \"^1.0\"\n\n[publish]\n\"llm.v1.request.generate.*\" = { wit = \"@unicity-astrid/wit/llm/generate-request\" }\n\n[subscribe]\n\"llm.v1.stream.*\" = { wit = \"@unicity-astrid/wit/llm/stream-event\", handler = \"handle_llm_stream\" }",
            "Import astrid:llm, publish a generate request to the active provider's topic, subscribe its stream. Mirrors how astrid-capsule-react drives a provider.",
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
