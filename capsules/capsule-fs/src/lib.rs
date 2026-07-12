#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]
#![allow(missing_docs)]

//! Filesystem tools capsule for Astrid OS.
//!
//! Provides `read_file`, `write_file`, `replace_in_file`, `list_directory`,
//! `grep_search`, `create_directory`, `delete_file`, and `move_file` tools to agents.

mod grep;

use astrid_sdk::prelude::*;
use astrid_sdk::schemars;
use grep::{GREP_MAX_DEPTH, GREP_MAX_FILES, GREP_MAX_MATCHES, grep_content};
use serde::Deserialize;

/// Maximum file size (10 MB) that `move_file` will transit through WASM guest memory.
const MOVE_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Default)]
pub struct FsTools;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ReadFileArgs {
    pub file_path: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct WriteFileArgs {
    pub file_path: String,
    pub content: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ReplaceInFileArgs {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ListDirectoryArgs {
    pub dir_path: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct GrepSearchArgs {
    pub dir_path: Option<String>,
    pub pattern: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct CreateDirectoryArgs {
    pub dir_path: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct DeleteFileArgs {
    /// The path to the file to delete.
    /// Note: Currently only supports deleting files created during the current session. Attempting to delete existing CWD files will fail due to lack of whiteout support.
    pub file_path: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct MoveFileArgs {
    /// The source path of the file to move.
    /// Note: Currently only supports moving files created during the current session. Attempting to move existing CWD files will fail due to lack of whiteout support.
    pub source_path: String,
    /// The destination path for the file.
    pub destination_path: String,
}

/// Sandboxed filesystem tools for reading, writing, searching, and managing files.
///
/// All operations go through the Astrid VFS — the agent cannot escape the
/// CWD boundary. Write operations are copy-on-write (changes are staged
/// in an overlay until committed).
#[capsule]
impl FsTools {
    /// Read the contents of a file. Returns the full file by default, or a
    /// specific line range if `start_line` and/or `end_line` are provided.
    /// Line numbers are 1-based. Use this to inspect source code, configs,
    /// logs, or any text file in the current directory.
    #[astrid::tool("read_file")]
    pub fn read_file(&self, args: ReadFileArgs) -> Result<String, SysError> {
        // Use the VFS Airlock to read the file
        // Note: SDK does not currently have read_to_string with lines, we can just use read_to_string and parse lines manually for now.
        let content = fs::read_to_string(&args.file_path)?;

        let lines: Vec<&str> = content.lines().collect();
        let start = args.start_line.unwrap_or(1).saturating_sub(1);
        let end = args.end_line.unwrap_or(lines.len()).min(lines.len());

        if start >= lines.len() || start >= end {
            return Ok(String::new());
        }

        let slice = &lines[start..end];
        Ok(slice.join("\n"))
    }

    /// Create or overwrite a file with the given content. The entire file is
    /// replaced — use `replace_in_file` for surgical edits to existing files.
    /// Parent directories must already exist (use `create_directory` first).
    #[astrid::tool("write_file", mutable)]
    pub fn write_file(&self, args: WriteFileArgs) -> Result<String, SysError> {
        fs::write(&args.file_path, args.content.as_bytes())?;
        Ok(format!("Successfully wrote to {}", args.file_path))
    }

    /// Replace an exact string in a file with a new string. The `old_string`
    /// must appear exactly once — if it appears zero times the tool errors
    /// (nothing to replace), and if it appears more than once it errors
    /// (ambiguous, provide more surrounding context to make the match unique).
    /// This is the preferred way to edit existing files.
    #[astrid::tool("replace_in_file", mutable)]
    pub fn replace_in_file(&self, args: ReplaceInFileArgs) -> Result<String, SysError> {
        let content = fs::read_to_string(&args.file_path)?;

        let count = content.matches(&args.old_string).count();
        if count == 0 {
            return Err(SysError::ApiError(format!(
                "Exact string not found in {}",
                args.file_path
            )));
        }
        if count > 1 {
            return Err(SysError::ApiError(format!(
                "Found {} occurrences of string in {}. Please be more specific.",
                count, args.file_path
            )));
        }

        let new_content = content.replace(&args.old_string, &args.new_string);
        fs::write(&args.file_path, new_content.as_bytes())?;

        Ok(format!("Successfully replaced text in {}", args.file_path))
    }

    /// List the contents of a directory. Returns file and subdirectory names.
    /// Use this to explore project structure, find files, or verify that
    /// expected files exist before reading them.
    #[astrid::tool("list_directory")]
    pub fn list_directory(&self, args: ListDirectoryArgs) -> Result<String, SysError> {
        let names: Vec<String> = fs::read_dir(&args.dir_path)?
            .map(|e| e.file_name().to_string())
            .collect();
        serde_json::to_string(&names).map_err(|e| SysError::ApiError(e.to_string()))
    }

    /// Search file contents for a pattern. Recursively walks the directory tree
    /// starting from `dir_path` (defaults to CWD root ".") and returns
    /// matching lines in `path:line_number:content` format. Use this to find
    /// function definitions, usages, error messages, or any text across the codebase.
    #[astrid::tool("grep_search")]
    pub fn grep_search(&self, args: GrepSearchArgs) -> Result<String, SysError> {
        if args.pattern.is_empty() {
            return Err(SysError::ApiError("pattern must not be empty".into()));
        }

        let root = args.dir_path.as_deref().unwrap_or(".");
        let mut matches: Vec<String> = Vec::new();
        let mut files_visited: usize = 0;

        walk_and_grep(root, &args.pattern, &mut matches, &mut files_visited, 0);

        if matches.is_empty() {
            return Ok("No matches found.".into());
        }

        Ok(matches.join("\n"))
    }

    /// Create a new directory. Parent directories must already exist.
    /// Use this before `write_file` if the target directory doesn't exist yet.
    #[astrid::tool("create_directory", mutable)]
    pub fn create_directory(&self, args: CreateDirectoryArgs) -> Result<String, SysError> {
        fs::create_dir(&args.dir_path)?;
        Ok(format!("Successfully created directory {}", args.dir_path))
    }

    /// Delete a file from the current directory. Only files can be deleted, not
    /// directories. Currently limited to files created during the current
    /// session (existing CWD files cannot be deleted due to VFS overlay
    /// limitations).
    #[astrid::tool("delete_file", mutable)]
    pub fn delete_file(&self, args: DeleteFileArgs) -> Result<String, SysError> {
        let stat = match file_stat(&args.file_path) {
            Ok(s) => s,
            Err(_) => {
                return Err(SysError::ApiError(format!(
                    "file does not exist: {}",
                    args.file_path
                )));
            }
        };
        if stat.is_dir {
            return Err(SysError::ApiError(format!(
                "{} is a directory, not a file; delete_file only supports files",
                args.file_path
            )));
        }
        fs::remove_file(&args.file_path)?;
        Ok(format!("Successfully deleted {}", args.file_path))
    }

    /// Move (rename) a file from one path to another. The destination must not
    /// already exist. Only files can be moved, not directories. The operation
    /// is atomic. Max file size: 10 MB. Note: only files created in the
    /// current session can be moved due to VFS overlay limitations.
    #[astrid::tool("move_file", mutable)]
    pub fn move_file(&self, args: MoveFileArgs) -> Result<String, SysError> {
        // Single stat covers both existence and directory checks.
        let src_stat = match file_stat(&args.source_path) {
            Ok(s) => s,
            Err(_) => {
                return Err(SysError::ApiError(format!(
                    "source path does not exist: {}",
                    args.source_path
                )));
            }
        };
        if src_stat.is_dir {
            return Err(SysError::ApiError(format!(
                "{} is a directory, not a file; move_file only supports files",
                args.source_path
            )));
        }
        if src_stat.size > MOVE_FILE_MAX_BYTES {
            return Err(SysError::ApiError(format!(
                "source file is too large to move ({} bytes, limit is {} bytes)",
                src_stat.size, MOVE_FILE_MAX_BYTES
            )));
        }
        if fs::exists(&args.destination_path)? {
            return Err(SysError::ApiError(format!(
                "destination already exists: {}",
                args.destination_path
            )));
        }

        let content = fs::read(&args.source_path)?;
        fs::write(&args.destination_path, &content)?;

        if let Err(e) = fs::remove_file(&args.source_path) {
            // Destination was written; clean up to avoid a phantom copy.
            let _ = fs::remove_file(&args.destination_path);
            return Err(SysError::ApiError(format!(
                "move failed: source could not be removed ({e}); destination write was rolled back"
            )));
        }

        Ok(format!(
            "Successfully moved {} to {}",
            args.source_path, args.destination_path
        ))
    }
}

/// Parsed VFS metadata for a single path.
struct FileStat {
    is_dir: bool,
    size: u64,
}

/// Returns parsed metadata for `path`, or a clear "not found" error.
fn file_stat(path: &str) -> Result<FileStat, SysError> {
    let meta = fs::metadata(path)?;
    Ok(FileStat {
        is_dir: meta.is_dir(),
        size: meta.len(),
    })
}

/// Recursively walks `dir` and collects lines containing `pattern`.
///
/// Respects depth, file-count, and match-count caps to prevent runaway searches.
fn walk_and_grep(
    dir: &str,
    pattern: &str,
    matches: &mut Vec<String>,
    files_visited: &mut usize,
    depth: usize,
) {
    if depth >= GREP_MAX_DEPTH
        || *files_visited >= GREP_MAX_FILES
        || matches.len() >= GREP_MAX_MATCHES
    {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            log::debug(format!("failed to read directory '{dir}': {e}"));
            return;
        }
    };

    for entry in entries {
        if matches.len() >= GREP_MAX_MATCHES || *files_visited >= GREP_MAX_FILES {
            return;
        }

        let path = entry.path().to_string();

        let is_dir = match fs::metadata(&path) {
            Ok(meta) => meta.is_dir(),
            Err(e) => {
                log::debug(format!("failed to stat path '{path}': {e}"));
                continue;
            }
        };

        if is_dir {
            walk_and_grep(&path, pattern, matches, files_visited, depth + 1);
        } else {
            *files_visited += 1;
            grep_file(&path, pattern, matches);
        }
    }
}

/// Searches a single file for lines containing `pattern`, appending
/// `path:line_number:content` to `matches`.
fn grep_file(path: &str, pattern: &str, matches: &mut Vec<String>) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            log::debug(format!("skipping unreadable file '{path}': {e}"));
            return;
        }
    };

    grep_content(path, &content, pattern, matches);
}
