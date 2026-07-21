#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

use astrid_sdk::astrid_sys::astrid::fs::host as wit_fs;
use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::{Deserialize, Serialize};

/// URI scheme prefix for the principal's home directory.
const HOME_SCHEME: &str = "home://";

struct BuiltinSkill {
    id: &'static str,
    content: &'static str,
}

const BUILTIN_SKILLS: &[BuiltinSkill] = &[BuiltinSkill {
    id: "capacity-planning",
    content: include_str!("skills/capacity-planning/SKILL.md"),
}];

#[derive(Default)]
pub struct SkillsLoader;

// Note: the host readdir returns a list of entry name strings (no per-
// entry metadata). We attempt to read SKILL.md from each entry —
// non-directories simply fail the read and are skipped.

#[derive(Debug, PartialEq)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

#[derive(Debug, Serialize)]
struct SkillInfo {
    id: String,
    name: String,
    description: String,
}

#[derive(Debug, Default, Deserialize, astrid_sdk::schemars::JsonSchema)]
pub struct ListSkillsArgs {
    /// Directory containing the skills (e.g., ".gemini/skills").
    /// The capsule will search both the workspace and the principal's
    /// home (`home://`) directory, merging results (workspace wins on
    /// duplicate skill IDs).
    pub dir_path: String,
}

#[derive(Debug, Default, Deserialize, astrid_sdk::schemars::JsonSchema)]
pub struct ReadSkillArgs {
    /// Directory containing the skills (e.g., ".gemini/skills").
    /// The capsule checks the workspace first, then falls back to
    /// the principal's home (`home://`) directory.
    pub dir_path: String,
    /// The ID/folder name of the skill to read
    pub skill_id: String,
}

#[derive(Debug, Deserialize, astrid_sdk::schemars::JsonSchema)]
pub struct CapacityModelArgs {
    /// Shared steady-state memory in MiB with zero external agents attached.
    pub shared_mib: f64,
    /// Marginal steady-state memory in MiB per attached agent.
    pub marginal_mib_per_agent: f64,
    /// Attached-agent count at which to evaluate total memory and density.
    pub agents: Option<u64>,
    /// Operator-supplied memory budget in MiB after any desired reserve.
    pub usable_memory_mib: Option<f64>,
    /// Process-wide persistent network-stream limit.
    pub host_net_streams: Option<u64>,
    /// Persistent streams consumed independently of attached agents.
    pub fixed_net_streams: Option<u64>,
    /// Persistent streams required by each attached agent.
    pub net_streams_per_agent: Option<f64>,
    /// Operator-supplied file-descriptor budget after reserving headroom for
    /// the OS and unrelated processes.
    pub usable_file_descriptors: Option<u64>,
    /// File descriptors consumed by the shared runtime before agents attach.
    pub fixed_file_descriptors: Option<u64>,
    /// Additional file descriptors consumed by each attached agent.
    pub file_descriptors_per_agent: Option<f64>,
    /// IPC subscriptions held by a multiplexing capsule. When supplied, the
    /// tool derives its client capacity from the 256-entry poll contract.
    pub ipc_subscriptions: Option<u64>,
}

#[derive(Debug, Serialize)]
struct CapacityModel {
    formula: &'static str,
    shared_mib: f64,
    marginal_mib_per_agent: f64,
    asymptotic_density_gain: f64,
    evaluated_agents: Option<u64>,
    evaluated_total_mib: Option<f64>,
    evaluated_mib_per_agent: Option<f64>,
    evaluated_density_gain: Option<f64>,
    memory_bound_agents: Option<u64>,
    net_stream_bound_agents: Option<u64>,
    file_descriptor_bound_agents: Option<u64>,
    poll_set_bound_clients: Option<u64>,
    derived_bound_agents: Option<u64>,
    bound_sources: Vec<&'static str>,
}

/// Skill discovery and loading tools.
///
/// Skills are reusable prompt templates stored as SKILL.md files with YAML
/// frontmatter (name, description). They live in workspace or home
/// directories and are merged with workspace taking priority.
#[capsule]
impl SkillsLoader {
    /// List all available skills in a directory. Scans both the workspace and
    /// home (`~/.astrid/home/{principal}/`) directories and system built-ins,
    /// merging results.
    /// Returns a JSON array of `{id, name, description}` objects. Workspace
    /// skills take priority over home skills with the same ID.
    #[astrid::tool("list_skills")]
    pub fn list_skills(&self, args: ListSkillsArgs) -> Result<String, SysError> {
        let bare_dir = bare_path(validate_dir_path(&args.dir_path)?);

        let mut skills = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        // Scan workspace first (takes priority on duplicate IDs)
        collect_skills_from(bare_dir, &mut skills, &mut seen_ids);

        // Scan home directory (new skills only, no overrides)
        let home_dir = format!("{HOME_SCHEME}{bare_dir}");
        collect_skills_from(&home_dir, &mut skills, &mut seen_ids);

        // Built-ins are the final fallback. A workspace or principal-home skill
        // with the same ID intentionally overrides the shipped version.
        collect_builtin_skills(&mut skills, &mut seen_ids);

        skills.sort_by(|left, right| left.id.cmp(&right.id));

        let json = serde_json::to_string(&skills)?;
        Ok(json)
    }

    /// Read the full content of a specific skill by its ID. Returns the raw
    /// SKILL.md content including frontmatter. Checks the workspace directory
    /// first, then falls back to the home directory and system built-ins.
    #[astrid::tool("read_skill")]
    pub fn read_skill(&self, args: ReadSkillArgs) -> Result<String, SysError> {
        let bare_dir = bare_path(validate_dir_path(&args.dir_path)?);
        let skill_path = resolve_skill_path(bare_dir, &args.skill_id)?;

        // Try workspace first — only fall back to home if the file is absent.
        // Permission errors or other I/O failures are surfaced immediately.
        match read_file_string(&skill_path) {
            Ok(content) => return Ok(content),
            Err(e) => {
                if !matches!(e, wit_fs::ErrorCode::NotFound) {
                    return Err(SysError::ApiError(format!(
                        "Failed to read skill '{}' from workspace: {:?}",
                        args.skill_id, e
                    )));
                }
            }
        }

        // Workspace file absent — fall back to home
        let home_skill_path =
            resolve_skill_path(&format!("{HOME_SCHEME}{bare_dir}"), &args.skill_id)?;
        match read_file_string(&home_skill_path) {
            Ok(content) => return Ok(content),
            Err(wit_fs::ErrorCode::NotFound) => {}
            Err(error) => {
                return Err(SysError::ApiError(format!(
                    "Failed to read skill '{}' from principal home: {error:?}",
                    args.skill_id
                )));
            }
        }

        if let Some(content) = builtin_skill(&args.skill_id) {
            return Ok(content.to_string());
        }

        Err(SysError::ApiError(format!(
            "Skill '{}' could not be read",
            args.skill_id
        )))
    }

    /// Evaluate the shared-plus-marginal capacity model and any resource
    /// envelopes the caller measured. No safety factor is invented: pass an
    /// already-reserved usable memory budget when a memory bound is wanted.
    #[astrid::tool("model_capacity")]
    pub fn model_capacity(&self, args: CapacityModelArgs) -> Result<String, SysError> {
        serde_json::to_string(&calculate_capacity(&args)?).map_err(Into::into)
    }
}

fn builtin_skill(skill_id: &str) -> Option<&'static str> {
    BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.id == skill_id)
        .map(|skill| skill.content)
}

fn collect_builtin_skills(
    skills: &mut Vec<SkillInfo>,
    seen_ids: &mut std::collections::HashSet<String>,
) {
    for builtin in BUILTIN_SKILLS {
        if seen_ids.contains(builtin.id) {
            continue;
        }
        let Some(frontmatter) = parse_frontmatter(builtin.content) else {
            log::warn(format!(
                "built-in skill '{}' has invalid frontmatter",
                builtin.id
            ));
            continue;
        };
        seen_ids.insert(builtin.id.to_string());
        skills.push(SkillInfo {
            id: builtin.id.to_string(),
            name: frontmatter.name,
            description: frontmatter.description,
        });
    }
}

fn finite_positive(value: f64, field: &str) -> Result<f64, SysError> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(SysError::ApiError(format!(
            "{field} must be finite and greater than zero"
        )))
    }
}

fn floor_to_u64(value: f64) -> u64 {
    if value <= 0.0 {
        0
    } else if value >= u64::MAX as f64 {
        u64::MAX
    } else {
        value.floor() as u64
    }
}

fn calculate_capacity(args: &CapacityModelArgs) -> Result<CapacityModel, SysError> {
    let shared = finite_positive(args.shared_mib, "shared_mib")?;
    let marginal = finite_positive(args.marginal_mib_per_agent, "marginal_mib_per_agent")?;
    if args.agents == Some(0) {
        return Err(SysError::ApiError(
            "agents must be greater than zero when supplied".to_string(),
        ));
    }

    let evaluated = args.agents.map(|agents| {
        let count = agents as f64;
        let total = shared + count * marginal;
        (total, total / count, count * (shared + marginal) / total)
    });

    let memory_bound = match args.usable_memory_mib {
        Some(budget) if budget.is_finite() && budget >= 0.0 => {
            Some(floor_to_u64((budget - shared).max(0.0) / marginal))
        }
        Some(_) => {
            return Err(SysError::ApiError(
                "usable_memory_mib must be finite and non-negative".to_string(),
            ));
        }
        None => None,
    };

    let net_stream_bound = match (args.host_net_streams, args.net_streams_per_agent) {
        (Some(limit), Some(per_agent)) => {
            let per_agent = finite_positive(per_agent, "net_streams_per_agent")?;
            let available = limit.saturating_sub(args.fixed_net_streams.unwrap_or(0));
            Some(floor_to_u64(available as f64 / per_agent))
        }
        (None, Some(per_agent)) => {
            finite_positive(per_agent, "net_streams_per_agent")?;
            None
        }
        _ => None,
    };

    let file_descriptor_bound = match (
        args.usable_file_descriptors,
        args.file_descriptors_per_agent,
    ) {
        (Some(limit), Some(per_agent)) => {
            let per_agent = finite_positive(per_agent, "file_descriptors_per_agent")?;
            let available = limit.saturating_sub(args.fixed_file_descriptors.unwrap_or(0));
            Some(floor_to_u64(available as f64 / per_agent))
        }
        (None, Some(per_agent)) => {
            finite_positive(per_agent, "file_descriptors_per_agent")?;
            None
        }
        _ => None,
    };

    const POLLABLES_PER_CALL: u64 = 256;
    let poll_set_bound = args.ipc_subscriptions.map(|subscriptions| {
        POLLABLES_PER_CALL
            .saturating_sub(subscriptions)
            .saturating_sub(1)
    });

    let candidates = [
        ("memory", memory_bound),
        ("net_streams", net_stream_bound),
        ("file_descriptors", file_descriptor_bound),
        ("poll_set", poll_set_bound),
    ];
    let derived_bound = candidates.iter().filter_map(|(_, value)| *value).min();
    let bound_sources = derived_bound.map_or_else(Vec::new, |minimum| {
        candidates
            .iter()
            .filter_map(|(name, value)| (*value == Some(minimum)).then_some(*name))
            .collect()
    });

    Ok(CapacityModel {
        formula: "M(N) = shared_mib + N * marginal_mib_per_agent",
        shared_mib: shared,
        marginal_mib_per_agent: marginal,
        asymptotic_density_gain: (shared + marginal) / marginal,
        evaluated_agents: args.agents,
        evaluated_total_mib: evaluated.map(|value| value.0),
        evaluated_mib_per_agent: evaluated.map(|value| value.1),
        evaluated_density_gain: evaluated.map(|value| value.2),
        memory_bound_agents: memory_bound,
        net_stream_bound_agents: net_stream_bound,
        file_descriptor_bound_agents: file_descriptor_bound,
        poll_set_bound_clients: poll_set_bound,
        derived_bound_agents: derived_bound,
        bound_sources,
    })
}

/// Read a file as UTF-8 via the typed WIT fs host fn.
///
/// Returns `wit_fs::ErrorCode` directly so callers can pattern-match on
/// `NotFound` for control flow. UTF-8 decode failures are normalised to
/// `ErrorCode::Unknown(...)`. Bypassing `astrid_sdk::fs::read_to_string`
/// preserves the typed error variant the SDK collapses into a `Debug`-
/// formatted string at the wrapper boundary.
fn read_file_string(path: &str) -> Result<String, wit_fs::ErrorCode> {
    let bytes = wit_fs::read_file(path)?;
    String::from_utf8(bytes).map_err(|e| wit_fs::ErrorCode::Unknown(e.to_string()))
}

/// Returns true if `name` is a safe single path component (no traversal).
///
/// Rejects `.`, `..`, `.git`, `.env`, and all other hidden/dot-prefixed names.
/// The `starts_with('.')` check subsumes both traversal markers and dot-files.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// Strip the `home://` scheme prefix, returning the bare relative path.
///
/// Caller must ensure `path` has been validated (e.g. via `validate_dir_path`)
/// before calling — `bare_path("home://")` returns `""`.
fn bare_path(path: &str) -> &str {
    path.strip_prefix(HOME_SCHEME).unwrap_or(path)
}

/// Validates `dir_path` and returns a cleaned version with trailing slashes removed.
/// Allows the `home://` scheme prefix.
fn validate_dir_path(dir_path: &str) -> Result<&str, SysError> {
    // Strip scheme prefix for validation, then re-include it in the result
    let path_to_check = dir_path.strip_prefix(HOME_SCHEME).unwrap_or(dir_path);
    if path_to_check.is_empty() {
        return Err(SysError::ApiError(
            "Invalid dir_path: path must not be empty".into(),
        ));
    }
    if path_to_check.contains("://") {
        return Err(SysError::ApiError(
            "Invalid dir_path: unknown scheme".into(),
        ));
    }
    if path_to_check.contains("..") || path_to_check.contains('\0') {
        return Err(SysError::ApiError(
            "Invalid dir_path: path traversal detected".into(),
        ));
    }
    Ok(dir_path.trim_end_matches('/'))
}

fn resolve_skill_path(dir_path: &str, skill_id: &str) -> Result<String, SysError> {
    let clean_dir = validate_dir_path(dir_path)?;

    if !is_safe_name(skill_id) {
        return Err(SysError::ApiError(
            "Invalid skill_id: path traversal detected".into(),
        ));
    }

    Ok(format!("{}/{}/SKILL.md", clean_dir, skill_id))
}

/// Scan a single directory for skills and append results. Skips silently
/// if the directory doesn't exist (e.g. home skills dir not created yet).
fn collect_skills_from(
    dir: &str,
    skills: &mut Vec<SkillInfo>,
    seen_ids: &mut std::collections::HashSet<String>,
) {
    let names = match wit_fs::fs_readdir(dir) {
        Ok(names) => names,
        Err(wit_fs::ErrorCode::NotFound) => return,
        Err(e) => {
            log::warn(format!("readdir failed for {dir}: {e:?}"));
            return;
        }
    };

    for name in names {
        if !is_safe_name(&name) || seen_ids.contains(&name) {
            continue;
        }
        let skill_path = format!("{}/{}/SKILL.md", dir, name);
        if let Ok(content) = read_file_string(&skill_path) {
            // Reserve the ID when SKILL.md exists — even if frontmatter is
            // invalid — so a broken workspace skill blocks the home version
            // (workspace wins). Directories without SKILL.md are not skills.
            seen_ids.insert(name.clone());
            if let Some(fm) = parse_frontmatter(&content) {
                skills.push(SkillInfo {
                    id: name,
                    name: fm.name,
                    description: fm.description,
                });
            } else {
                log::warn(format!("skipping {dir}/{name}: invalid frontmatter"));
            }
        }
    }
}

/// Parse YAML frontmatter from a SKILL.md file.
///
/// Extracts `name` and `description` fields from the `---` delimited header.
/// Uses manual `key: value` parsing to avoid pulling in a YAML library for
/// two trivial fields — and to guarantee WASM compatibility.
///
/// **Limitations:** This is not a general YAML parser. It only supports simple
/// `key: value` pairs on single lines. Quoted strings (`"value"`), multi-line
/// values (`|`, `>`), nested mappings, and sequences are not handled. Values
/// containing colons are supported (via `split_once`), but surrounding quotes
/// are preserved verbatim. This is sufficient for the `name`/`description`
/// fields in SKILL.md files.
fn parse_frontmatter(content: &str) -> Option<SkillFrontmatter> {
    // Skip the opening delimiter and any trailing whitespace on that line
    let rest = content.strip_prefix("---")?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;

    // Find the closing delimiter — `---` must be on its own line
    let end_idx = rest
        .match_indices("\n---")
        .find(|&(idx, _)| {
            let after = idx + 4; // "\n---".len()
            matches!(rest.as_bytes().get(after), None | Some(b'\n') | Some(b'\r'))
        })
        .map(|(idx, _)| idx)?;
    let block = &rest[..end_idx];

    let mut name = None;
    let mut description = None;

    for line in block.lines() {
        let line = line.trim();
        if let Some((key, val)) = line.split_once(':') {
            let key = key.trim();
            let val = val.trim();
            match key {
                "name" => name = Some(val.to_string()),
                "description" => description = Some(val.to_string()),
                _ => {}
            }
        }
    }

    Some(SkillFrontmatter {
        name: name?,
        description: description?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_capacity_skill_has_valid_frontmatter() {
        let content = builtin_skill("capacity-planning").expect("built-in skill");
        let frontmatter = parse_frontmatter(content).expect("valid frontmatter");
        assert_eq!(frontmatter.name, "capacity-planning");
        assert!(frontmatter.description.contains("attached agents"));
    }

    #[test]
    fn capacity_model_matches_measured_shared_runtime_example() {
        let model = calculate_capacity(&CapacityModelArgs {
            shared_mib: 223.0,
            marginal_mib_per_agent: 5.734,
            agents: Some(8),
            usable_memory_mib: Some(1024.0),
            host_net_streams: Some(512),
            fixed_net_streams: Some(8),
            net_streams_per_agent: Some(1.0),
            usable_file_descriptors: Some(4096),
            fixed_file_descriptors: Some(128),
            file_descriptors_per_agent: Some(3.0),
            ipc_subscriptions: Some(16),
        })
        .expect("valid model");

        assert!((model.evaluated_total_mib.expect("total") - 268.872).abs() < 0.001);
        assert!((model.evaluated_density_gain.expect("gain") - 6.806).abs() < 0.01);
        assert!((model.asymptotic_density_gain - 39.894).abs() < 0.01);
        assert_eq!(model.memory_bound_agents, Some(139));
        assert_eq!(model.net_stream_bound_agents, Some(504));
        assert_eq!(model.file_descriptor_bound_agents, Some(1322));
        assert_eq!(model.poll_set_bound_clients, Some(239));
        assert_eq!(model.derived_bound_agents, Some(139));
        assert_eq!(model.bound_sources, vec!["memory"]);
    }

    #[test]
    fn capacity_model_rejects_arbitrary_or_invalid_inputs() {
        let error = calculate_capacity(&CapacityModelArgs {
            shared_mib: 0.0,
            marginal_mib_per_agent: 1.0,
            agents: None,
            usable_memory_mib: None,
            host_net_streams: None,
            fixed_net_streams: None,
            net_streams_per_agent: None,
            usable_file_descriptors: None,
            fixed_file_descriptors: None,
            file_descriptors_per_agent: None,
            ipc_subscriptions: None,
        })
        .expect_err("zero shared cost is not a measured model");
        assert!(error.to_string().contains("shared_mib"));
    }

    #[test]
    fn test_parse_valid_frontmatter() {
        let content =
            "---\nname: my-skill\ndescription: Does a thing\n---\n# My Skill\nSome content";
        let parsed = parse_frontmatter(content).unwrap();
        assert_eq!(parsed.name, "my-skill");
        assert_eq!(parsed.description, "Does a thing");
    }

    #[test]
    fn test_parse_stops_at_first_closing_delimiter() {
        let content =
            "---\nname: test\ndescription: testing\n---\n# Test\n---\nSome text\n---\nMore text";
        let parsed = parse_frontmatter(content).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.description, "testing");
    }

    #[test]
    fn test_parse_no_frontmatter() {
        let content = "# Title\nJust some text";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn test_parse_unclosed_frontmatter() {
        let content = "---\nname: test\ndescription: missing end rule\n# Oops";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn test_parse_frontmatter_crlf() {
        let content =
            "---\r\nname: crlf-skill\r\ndescription: Windows line endings\r\n---\r\n# Content";
        let parsed = parse_frontmatter(content).unwrap();
        assert_eq!(parsed.name, "crlf-skill");
        assert_eq!(parsed.description, "Windows line endings");
    }

    #[test]
    fn test_parse_frontmatter_missing_field() {
        let content = "---\nname: only-name\n---\n# Content";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn test_parse_frontmatter_delimiter_must_be_own_line() {
        // "---notadash" should not be treated as a closing delimiter
        let content = "---\nname: test\n---notadash\ndescription: real desc\n---\n# Body";
        let parsed = parse_frontmatter(content).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.description, "real desc");
    }

    #[test]
    fn test_is_safe_name() {
        assert!(is_safe_name("valid-skill"));
        assert!(is_safe_name("skill_v2"));
        assert!(!is_safe_name(""));
        assert!(!is_safe_name("../escape"));
        assert!(!is_safe_name("some/path"));
        assert!(!is_safe_name("back\\slash"));
        assert!(!is_safe_name(".."));
        assert!(!is_safe_name("."));
        assert!(!is_safe_name(".git"));
        assert!(!is_safe_name(".env"));
        assert!(!is_safe_name(".hidden-skill"));
        assert!(!is_safe_name("skill\0null"));
        assert!(!is_safe_name("skill\0"));
    }

    #[test]
    fn test_path_traversal_null_bytes() {
        assert!(resolve_skill_path("skills\0evil", "ok").is_err());
        assert!(resolve_skill_path("skills", "ok\0evil").is_err());
    }

    #[test]
    fn test_parse_frontmatter_description_with_colon() {
        let content = "---\nname: deploy\ndescription: Runs the deploy: prod pipeline\n---\n# Body";
        let parsed = parse_frontmatter(content).unwrap();
        assert_eq!(parsed.description, "Runs the deploy: prod pipeline");
    }

    #[test]
    fn test_path_traversal_check() {
        assert!(resolve_skill_path(".gemini/skills", "../secret/file").is_err());
        assert!(resolve_skill_path(".gemini/skills", "some/folder").is_err());
        assert!(resolve_skill_path(".gemini/skills", "..\\windows\\hack").is_err());
        assert!(resolve_skill_path("../escaped", "skill").is_err());

        let valid = resolve_skill_path(".gemini/skills/", "valid-skill-id").unwrap();
        assert_eq!(valid, ".gemini/skills/valid-skill-id/SKILL.md");

        let valid2 = resolve_skill_path("skills", "skill_version_2").unwrap();
        assert_eq!(valid2, "skills/skill_version_2/SKILL.md");
    }

    #[test]
    fn test_bare_path_strips_home_prefix() {
        assert_eq!(bare_path("home://skills"), "skills");
        assert_eq!(bare_path("skills"), "skills");
        assert_eq!(bare_path("home://"), "");
        assert_eq!(bare_path(".gemini/skills"), ".gemini/skills");
    }

    #[test]
    fn test_validate_dir_path_with_home_prefix() {
        assert_eq!(validate_dir_path("home://skills").unwrap(), "home://skills");
        assert!(validate_dir_path("home://../escape").is_err());
        assert!(validate_dir_path("home://skills\0evil").is_err());
    }

    #[test]
    fn test_validate_dir_path_rejects_empty() {
        assert!(validate_dir_path("").is_err());
        assert!(validate_dir_path("home://").is_err());
    }

    #[test]
    fn test_validate_dir_path_rejects_unknown_scheme() {
        assert!(validate_dir_path("cwd://skills").is_err());
        assert!(validate_dir_path("http://evil.com").is_err());
        assert!(validate_dir_path("file:///etc/passwd").is_err());
    }

    #[test]
    fn test_resolve_skill_path_with_home_prefix() {
        let path = resolve_skill_path("home://skills", "my-skill").unwrap();
        assert_eq!(path, "home://skills/my-skill/SKILL.md");
    }
}
