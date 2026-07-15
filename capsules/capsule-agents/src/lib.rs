#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![warn(missing_docs)]

//! Project instructions capsule for Unicity AOS.
//!
//! Hooks into the prompt builder pipeline via
//! `prompt_builder.v1.hook.before_build` and injects the contents of
//! `{cwd_dir}/AGENTS.md` into the system prompt as project-level instructions.
//!
//! This is the equivalent of Claude Code's `CLAUDE.md` — a human-authored
//! file that tells the agent how to work within this specific project.
//! Unlike `memory.md` (agent-written), this file is maintained by the user.
//!
//! The folder name is read from the `cwd_dir` env config (set by the distro).
//! A rebranded distro can set `cwd_dir = ".myagent"` without forking the capsule.

use astrid_sdk::prelude::*;

/// Default project folder name — last resort if distro did not set `cwd_dir`.
const DEFAULT_CWD_DIR: &str = ".astrid";

/// Project instructions capsule.
#[derive(Default)]
pub struct AgentsInjector;

/// Resolve the instructions file path, trying AGENTS.md then ASTRID.md.
///
/// Returns the path of the first file that exists, or `None` if neither does.
fn instructions_path() -> Option<String> {
    let dir = env::var("cwd_dir");
    let cwd_dir = dir.as_deref().unwrap_or(DEFAULT_CWD_DIR);

    let agents = format!("cwd://{cwd_dir}/AGENTS.md");
    if fs::exists(&agents).unwrap_or(false) {
        return Some(agents);
    }

    let astrid = format!("cwd://{cwd_dir}/ASTRID.md");
    if fs::exists(&astrid).unwrap_or(false) {
        return Some(astrid);
    }

    None
}

#[capsule]
impl AgentsInjector {
    /// Intercepts `prompt_builder.v1.hook.before_build` events.
    ///
    /// Reads `{cwd_dir}/AGENTS.md` (falling back to `ASTRID.md`) and injects
    /// the contents as project instructions in the system prompt.
    /// Silently skipped if neither file exists.
    #[astrid::interceptor("on_before_prompt_build")]
    pub fn on_before_prompt_build(&self, payload: serde_json::Value) -> Result<(), SysError> {
        let response_topic = payload
            .get("response_topic")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SysError::ApiError("missing response_topic in before_build payload".into())
            })?;

        let path = match instructions_path() {
            Some(p) => p,
            None => return Ok(()),
        };

        let content = match fs::read_to_string(&path) {
            Ok(c) if !c.trim().is_empty() => c,
            _ => return Ok(()),
        };

        ipc::publish_json(
            response_topic,
            &serde_json::json!({ "appendSystemContext": content }),
        )?;

        Ok(())
    }
}
