#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

use astrid_sdk::astrid_sys::astrid::fs::host as wit_fs;
use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use serde::{Deserialize, Serialize};

/// URI scheme prefix for the principal's home directory.
const HOME_SCHEME: &str = "home://";

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

/// Skill discovery and loading tools.
///
/// Skills are reusable prompt templates stored as SKILL.md files with YAML
/// frontmatter (name, description). They live in workspace or home
/// directories and are merged with workspace taking priority.
#[capsule]
impl SkillsLoader {
    /// List all available skills in a directory. Scans both the workspace and
    /// home (`~/.astrid/home/{principal}/`) directories, merging results.
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

        let json = serde_json::to_string(&skills)?;
        Ok(json)
    }

    /// Read the full content of a specific skill by its ID. Returns the raw
    /// SKILL.md content including frontmatter. Checks the workspace directory
    /// first, then falls back to the home directory.
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
            Ok(content) => Ok(content),
            Err(e) => {
                log::warn(format!("failed to read skill '{}': {:?}", args.skill_id, e));
                Err(SysError::ApiError(format!(
                    "Skill '{}' could not be read",
                    args.skill_id
                )))
            }
        }
    }
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
