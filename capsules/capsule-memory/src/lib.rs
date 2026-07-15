#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

//! Cross-session memory capsule for Unicity AOS.
//!
//! Hooks into the prompt builder pipeline via
//! `prompt_builder.v1.hook.before_build` and injects memory from two sources:
//!
//! - **Personal** (`home://memory.md`) — user preferences, communication style,
//!   long-term facts about the user. Always injected regardless of project.
//! - **Project** (`cwd://{cwd_dir}/memory.md`) — project-specific context,
//!   current work, conventions. Only present when working inside a project.
//!
//! Both are optional — missing or empty files are silently skipped.
//! Personal memory is injected first; project memory appends after.
//!
//! The project folder name is read from the `cwd_dir` env config (set by the
//! distro). A rebranded distro sets `cwd_dir = ".myagent"` without forking.

use astrid_sdk::prelude::*;

/// Maximum bytes per memory section.
///
/// Applied independently to personal and project memory to prevent either
/// from crowding out the other or consuming unbounded context window.
const MAX_MEMORY_BYTES: usize = 32_768;

/// Default project folder name — last resort if distro did not set `cwd_dir`.
const DEFAULT_CWD_DIR: &str = ".astrid";

/// Cross-session memory injector capsule.
#[derive(Default)]
pub struct MemoryInjector;

/// Path to the project-local memory file.
fn project_memory_path() -> String {
    let dir = env::var("cwd_dir");
    format!(
        "cwd://{}/memory.md",
        dir.as_deref().unwrap_or(DEFAULT_CWD_DIR)
    )
}

/// Truncate content to `MAX_MEMORY_BYTES` on a char boundary.
fn truncate(content: &str) -> (&str, bool) {
    if content.len() > MAX_MEMORY_BYTES {
        (
            &content[..content.floor_char_boundary(MAX_MEMORY_BYTES)],
            true,
        )
    } else {
        (content, false)
    }
}

/// Read a memory file, returning `None` if missing or empty.
fn read_memory(path: &str) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(c) if !c.trim().is_empty() => Some(c),
        _ => None,
    }
}

#[capsule]
impl MemoryInjector {
    /// Intercepts `prompt_builder.v1.hook.before_build` events.
    ///
    /// Reads personal memory (`home://memory.md`) and project memory
    /// (`cwd://{cwd_dir}/memory.md`), injects both into the system prompt.
    /// Either or both may be absent — silently skipped if so.
    #[astrid::interceptor("on_before_prompt_build")]
    pub fn on_before_prompt_build(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let response_topic = payload
            .get("response_topic")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SysError::ApiError(format!(
                    "missing response_topic in before_build payload. keys: {:?}",
                    payload.as_object().map(|o| o.keys().collect::<Vec<_>>())
                ))
            })?;

        let personal = read_memory("home://memory.md");
        let project = read_memory(&project_memory_path());

        let sections = [
            (personal.as_deref(), "personal"),
            (project.as_deref(), "project"),
        ]
        .iter()
        .filter_map(|(content, label)| content.map(|c| (c, *label)))
        .map(|(content, label)| {
            let (text, truncated) = truncate(content);
            let mut section = format!("# Memory ({label})\n\n{text}");
            if truncated {
                section.push_str("\n\n[Memory truncated]");
            }
            section
        })
        .collect::<Vec<_>>()
        .join("\n\n");

        if sections.is_empty() {
            return Ok(());
        }

        ipc::publish_json(
            response_topic,
            &serde_json::json!({ "appendSystemContext": sections }),
        )?;

        Ok(())
    }
}
