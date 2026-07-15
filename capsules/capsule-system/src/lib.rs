#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![allow(missing_docs)]

//! System management tools capsule for Unicity AOS.
//!
//! Gives the LLM typed tools to inspect and manage its own runtime.
//! All operations go through the kernel's VFS and capability system —
//! the capsule cannot bypass sandbox boundaries.
//!
//! # Tools
//!
//! - `list_capsules` — enumerate installed capsules with names and versions
//! - `inspect_capsule` — read a capsule's manifest and metadata
//! - `list_interfaces` — list available WIT interface contracts
//! - `read_interface` — read a WIT interface definition
//! - `system_status` — runtime health and interface coverage summary

use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::{Deserialize, Serialize};

/// Capsule directory under the principal home (FHS layout).
const CAPSULES_DIR: &str = "home://.local/capsules";

/// Standard WIT interface directory — per-principal, accessible via `home://wit/`.
const WIT_DIR: &str = "home://wit";

/// Skill installed to `home://skills/capsule-development/SKILL.md` on install.
const CAPSULE_DEV_SKILL: &str = include_str!("skills/capsule-development/SKILL.md");

#[derive(Default)]
pub struct SystemTools;

// ---------------------------------------------------------------------------
// Tool argument types
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct EmptyArgs {}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct InspectCapsuleArgs {
    /// Capsule name (e.g. `astrid-capsule-session`).
    pub name: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ReadInterfaceArgs {
    /// Interface filename (e.g. `session.wit`).
    pub name: String,
}

// ---------------------------------------------------------------------------
// Response types (serialized to JSON for the LLM)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CapsuleSummary {
    name: String,
    version: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    exports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    imports: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SystemStatusResponse {
    capsule_count: usize,
    exports: Vec<String>,
    imports_satisfied: Vec<String>,
    imports_unsatisfied: Vec<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a `meta.json` file content into a serde_json::Value.
fn parse_meta(content: &str) -> Option<serde_json::Value> {
    serde_json::from_str(content).ok()
}

/// Extract `namespace/interface` strings from the nested exports/imports map
/// in meta.json: `{ "astrid": { "session": "1.0.0" } }` → `["astrid/session 1.0.0"]`
fn flatten_interface_map(map: &serde_json::Value) -> Vec<String> {
    let mut result = Vec::new();
    if let Some(obj) = map.as_object() {
        for (ns, ifaces) in obj {
            if let Some(ifaces_obj) = ifaces.as_object() {
                for (name, version) in ifaces_obj {
                    let ver = version.as_str().unwrap_or("?");
                    result.push(format!("{ns}/{name} {ver}"));
                }
            }
        }
    }
    result
}

/// List entry names under a VFS path.
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
impl SystemTools {
    /// Install the capsule-development skill to `home://skills/capsule-development/SKILL.md`
    /// so the skills capsule can surface it to the LLM.
    #[astrid::install]
    pub fn on_install(&self) -> Result<(), SysError> {
        // home:// may not be available during lifecycle dispatch when installing
        // without a running daemon. Ignore the host errors (`let _ = ...`) to
        // silently skip — the skill will be written on the next full boot
        // once the principal home is mounted.
        //
        // `create_dir_all` is idempotent and creates missing parents in a single
        // host call, replacing the prior exists-then-create_dir ladder.
        let _ = astrid_sdk::fs::create_dir_all("home://skills/capsule-development");
        let _ = astrid_sdk::fs::write(
            "home://skills/capsule-development/SKILL.md",
            CAPSULE_DEV_SKILL.as_bytes(),
        );
        Ok(())
    }

    /// List all installed capsules with their names and versions. Use `inspect_capsule`
    /// for a capsule's manifest, exports, imports, and capabilities.
    /// Returns a JSON array of capsule summaries.
    #[astrid::tool("list_capsules")]
    pub fn list_capsules(&self, _args: EmptyArgs) -> Result<String, SysError> {
        let capsule_names = list_entries(CAPSULES_DIR)?;
        let mut summaries = Vec::new();

        for name in &capsule_names {
            let meta_path = format!("{CAPSULES_DIR}/{name}/meta.json");
            let meta_content = match astrid_sdk::fs::read_to_string(&meta_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let meta = match parse_meta(&meta_content) {
                Some(m) => m,
                None => continue,
            };

            let version = meta
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let exports = meta
                .get("exports")
                .map(flatten_interface_map)
                .unwrap_or_default();

            let imports = meta
                .get("imports")
                .map(flatten_interface_map)
                .unwrap_or_default();

            summaries.push(CapsuleSummary {
                name: name.clone(),
                version,
                exports,
                imports,
            });
        }

        serde_json::to_string_pretty(&summaries)
            .map_err(|e| SysError::ApiError(format!("serialize: {e}")))
    }

    /// Read a capsule's full manifest and installation metadata.
    /// Returns the Capsule.toml content and meta.json as a combined response.
    #[astrid::tool("inspect_capsule")]
    pub fn inspect_capsule(&self, args: InspectCapsuleArgs) -> Result<String, SysError> {
        let name = args.name.trim();
        if name.is_empty() {
            return Err(SysError::ApiError("Capsule name cannot be empty".into()));
        }

        // Reject path traversal
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(SysError::ApiError(
                "Invalid capsule name — path traversal rejected".into(),
            ));
        }

        let manifest_path = format!("{CAPSULES_DIR}/{name}/Capsule.toml");
        let meta_path = format!("{CAPSULES_DIR}/{name}/meta.json");

        let manifest = astrid_sdk::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|_| format!("(Capsule.toml not found for {name})"));

        let meta = astrid_sdk::fs::read_to_string(&meta_path)
            .unwrap_or_else(|_| format!("(meta.json not found for {name})"));

        Ok(format!(
            "=== Capsule.toml ===\n{manifest}\n\n=== meta.json ===\n{meta}"
        ))
    }

    /// List all WIT interface definitions available in the system.
    /// These define the typed contracts between capsules.
    #[astrid::tool("list_interfaces")]
    pub fn list_interfaces(&self, _args: EmptyArgs) -> Result<String, SysError> {
        match list_entries(WIT_DIR) {
            Ok(files) => {
                if files.is_empty() {
                    Ok("No WIT interfaces installed. Run `aos init` to set up the standard interfaces.".into())
                } else {
                    Ok(files.join("\n"))
                }
            }
            Err(_) => Ok(
                "WIT directory not found. Run `aos init` to set up the standard interfaces.".into(),
            ),
        }
    }

    /// Read a WIT interface definition file. Returns the full typed contract
    /// so you can understand the message schemas between capsules.
    #[astrid::tool("read_interface")]
    pub fn read_interface(&self, args: ReadInterfaceArgs) -> Result<String, SysError> {
        let name = args.name.trim();
        if name.is_empty() {
            return Err(SysError::ApiError("Interface name cannot be empty".into()));
        }

        // Reject path traversal
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(SysError::ApiError(
                "Invalid interface name — path traversal rejected".into(),
            ));
        }

        // Add .wit extension if not present
        let filename = if name.ends_with(".wit") {
            name.to_string()
        } else {
            format!("{name}.wit")
        };

        let path = format!("{WIT_DIR}/{filename}");
        astrid_sdk::fs::read_to_string(&path).map_err(|_| {
            SysError::ApiError(format!(
                "Interface '{filename}' not found. Use list_interfaces to see available interfaces."
            ))
        })
    }

    /// Show runtime status: capsule count, interface coverage, satisfied and
    /// unsatisfied imports. Helps you understand the health of the system.
    #[astrid::tool("system_status")]
    pub fn system_status(&self, _args: EmptyArgs) -> Result<String, SysError> {
        let capsule_names = list_entries(CAPSULES_DIR)?;

        // Collect all exports and imports across all capsules
        let mut all_exports: Vec<String> = Vec::new();
        let mut all_imports: Vec<(String, String)> = Vec::new(); // (interface, capsule_name)

        for name in &capsule_names {
            let meta_path = format!("{CAPSULES_DIR}/{name}/meta.json");
            let meta_content = match astrid_sdk::fs::read_to_string(&meta_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let meta = match parse_meta(&meta_content) {
                Some(m) => m,
                None => continue,
            };

            if let Some(exports) = meta.get("exports") {
                for iface in flatten_interface_map(exports) {
                    // Strip version for matching: "astrid/session 1.0.0" → "astrid/session"
                    let key = iface.split_whitespace().next().unwrap_or(&iface);
                    if !all_exports.contains(&key.to_string()) {
                        all_exports.push(key.to_string());
                    }
                }
            }

            if let Some(imports) = meta.get("imports") {
                for iface in flatten_interface_map(imports) {
                    let key = iface.split_whitespace().next().unwrap_or(&iface);
                    all_imports.push((key.to_string(), name.clone()));
                }
            }
        }

        let mut satisfied = Vec::new();
        let mut unsatisfied = Vec::new();

        for (iface, capsule) in &all_imports {
            if all_exports.contains(iface) {
                satisfied.push(format!("{iface} (needed by {capsule})"));
            } else {
                unsatisfied.push(format!("{iface} (needed by {capsule})"));
            }
        }

        let status = SystemStatusResponse {
            capsule_count: capsule_names.len(),
            exports: all_exports,
            imports_satisfied: satisfied,
            imports_unsatisfied: unsatisfied,
        };

        serde_json::to_string_pretty(&status)
            .map_err(|e| SysError::ApiError(format!("serialize: {e}")))
    }
}
