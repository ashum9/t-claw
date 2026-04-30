//! File system tools: read, write, edit, list

use super::base::{SimpleTool, ToolInput, ToolResult};
use crate::rbac::{RbacManager, Role};
use async_trait::async_trait;
use mofa_sdk::agent::ToolCategory;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

/// Tool to read file contents
pub struct ReadFileTool {
    rbac_manager: Option<Arc<RbacManager>>,
    user_role: Option<Role>,
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self {
            rbac_manager: None,
            user_role: None,
        }
    }

    /// Create with RBAC manager and user role
    pub fn with_rbac(rbac_manager: Arc<RbacManager>, user_role: Role) -> Self {
        Self {
            rbac_manager: Some(rbac_manager),
            user_role: Some(user_role),
        }
    }
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SimpleTool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file at the given path. \
        Optionally provide start_line and end_line (1-indexed, inclusive) to read \
        only a portion of large files."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to read"
                },
                "start_line": {
                    "type": "integer",
                    "description": "First line to read (1-indexed, inclusive). Omit to start from the beginning.",
                    "minimum": 1
                },
                "end_line": {
                    "type": "integer",
                    "description": "Last line to read (1-indexed, inclusive). Omit to read to the end of the file.",
                    "minimum": 1
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let path = match input.get_str("path") {
            Some(p) => p,
            None => return ToolResult::failure("Missing 'path' parameter"),
        };

        let path = expand_tilde(Path::new(path));

        // Check permissions if RBAC is enabled
        if let (Some(rbac), Some(role)) = (&self.rbac_manager, &self.user_role) {
            match rbac.check_path_access(*role, "read", &path) {
                crate::rbac::manager::PermissionResult::Allowed => {}
                crate::rbac::manager::PermissionResult::Denied(reason) => {
                    return ToolResult::failure(format!("Permission denied: {}", reason));
                }
            }
        }

        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return ToolResult::failure(format!("Error: File not found: {}", path.display()));
        }

        if !tokio::fs::metadata(&path)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return ToolResult::failure(format!("Error: Not a file: {}", path.display()));
        }

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::failure(format!("Error reading file: {}", e)),
        };

        // Extract optional line-range parameters.
        let start_line = input
            .get_number("start_line")
            .map(|v| v as usize)
            .unwrap_or(1);
        let end_line = input.get_number("end_line").map(|v| v as usize);

        // Validate start_line is at least 1.
        if start_line < 1 {
            return ToolResult::failure("start_line must be >= 1");
        }

        // Collect lines (0-indexed internally).
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        // Default end_line to last line.
        let end_line = end_line.unwrap_or(total_lines);

        // Validate range.
        if start_line > total_lines {
            return ToolResult::failure(format!(
                "start_line ({}) exceeds total line count ({})",
                start_line, total_lines
            ));
        }
        if end_line < start_line {
            return ToolResult::failure(format!(
                "end_line ({}) must be >= start_line ({})",
                end_line, start_line
            ));
        }

        // Clamp end_line to actual file length.
        let end_line = end_line.min(total_lines);

        // Slice the requested lines (convert to 0-indexed).
        let slice = &lines[(start_line - 1)..end_line];

        // Prepend a context header when a sub-range was requested so the LLM
        // knows where it is in the file and how many lines remain.
        let output = if start_line == 1 && end_line == total_lines {
            slice.join("\n")
        } else {
            format!(
                "// File: {} (lines {}-{} of {})\n{}",
                path.display(),
                start_line,
                end_line,
                total_lines,
                slice.join("\n")
            )
        };

        ToolResult::success_text(output)
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::File
    }
}


/// Tool to write content to a file
pub struct WriteFileTool {
    rbac_manager: Option<Arc<RbacManager>>,
    user_role: Option<Role>,
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self {
            rbac_manager: None,
            user_role: None,
        }
    }

    /// Create with RBAC manager and user role
    pub fn with_rbac(rbac_manager: Arc<RbacManager>, user_role: Role) -> Self {
        Self {
            rbac_manager: Some(rbac_manager),
            user_role: Some(user_role),
        }
    }
}

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SimpleTool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file at the given path. Creates parent directories if needed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to write to"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let path = match input.get_str("path") {
            Some(p) => p,
            None => return ToolResult::failure("Missing 'path' parameter"),
        };

        let content = match input.get_str("content") {
            Some(c) => c,
            None => return ToolResult::failure("Missing 'content' parameter"),
        };

        let path = expand_tilde(Path::new(path));

        // Check permissions if RBAC is enabled
        if let (Some(rbac), Some(role)) = (&self.rbac_manager, &self.user_role) {
            match rbac.check_path_access(*role, "write", &path) {
                crate::rbac::manager::PermissionResult::Allowed => {}
                crate::rbac::manager::PermissionResult::Denied(reason) => {
                    return ToolResult::failure(format!("Permission denied: {}", reason));
                }
            }
        }

        // Create parent directories
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent).await
        {
            return ToolResult::failure(format!("Error creating directory: {}", e).to_string());
        }

        match fs::write(&path, content).await {
            Ok(_) => ToolResult::success_text(format!(
                "Successfully wrote {} bytes to {}",
                content.len(),
                path.display()
            )),
            Err(e) => ToolResult::failure(format!("Error writing file: {}", e).to_string()),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::File
    }
}

/// Tool to edit a file by replacing text
pub struct EditFileTool {
    rbac_manager: Option<Arc<RbacManager>>,
    user_role: Option<Role>,
}

impl EditFileTool {
    pub fn new() -> Self {
        Self {
            rbac_manager: None,
            user_role: None,
        }
    }

    /// Create with RBAC manager and user role
    pub fn with_rbac(rbac_manager: Arc<RbacManager>, user_role: Role) -> Self {
        Self {
            rbac_manager: Some(rbac_manager),
            user_role: Some(user_role),
        }
    }
}

impl Default for EditFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SimpleTool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing old_text with new_text. The old_text must exist exactly in the file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to edit"
                },
                "old_text": {
                    "type": "string",
                    "description": "The exact text to find and replace"
                },
                "new_text": {
                    "type": "string",
                    "description": "The text to replace with"
                }
            },
            "required": ["path", "old_text", "new_text"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let path = match input.get_str("path") {
            Some(p) => p,
            None => return ToolResult::failure("Missing 'path' parameter"),
        };

        let old_text = match input.get_str("old_text") {
            Some(t) => t,
            None => return ToolResult::failure("Missing 'old_text' parameter"),
        };

        let new_text = match input.get_str("new_text") {
            Some(t) => t,
            None => return ToolResult::failure("Missing 'new_text' parameter"),
        };

        let path = expand_tilde(Path::new(path));

        // Check permissions if RBAC is enabled
        if let (Some(rbac), Some(role)) = (&self.rbac_manager, &self.user_role) {
            match rbac.check_path_access(*role, "write", &path) {
                crate::rbac::manager::PermissionResult::Allowed => {}
                crate::rbac::manager::PermissionResult::Denied(reason) => {
                    return ToolResult::failure(format!("Permission denied: {}", reason));
                }
            }
        }

        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return ToolResult::failure(format!("Error: File not found: {}", path.display()));
        }

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::failure(format!("Error reading file: {}", e).to_string()),
        };

        if !content.contains(old_text) {
            return ToolResult::failure(
                "Error: old_text not found in file. Make sure it matches exactly.".to_string(),
            );
        }

        let count = content.matches(old_text).count();
        if count > 1 {
            return ToolResult::failure(format!(
                "Warning: old_text appears {} times. Please provide more context to make it unique.",
                count
            ));
        }

        let new_content = content.replacen(old_text, new_text, 1);

        match fs::write(&path, new_content).await {
            Ok(_) => ToolResult::success_text(format!("Successfully edited {}", path.display())),
            Err(e) => ToolResult::failure(format!("Error writing file: {}", e).to_string()),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::File
    }
}

/// Tool to list directory contents
pub struct ListDirTool {
    rbac_manager: Option<Arc<RbacManager>>,
    user_role: Option<Role>,
}

impl ListDirTool {
    pub fn new() -> Self {
        Self {
            rbac_manager: None,
            user_role: None,
        }
    }

    /// Create with RBAC manager and user role
    pub fn with_rbac(rbac_manager: Arc<RbacManager>, user_role: Role) -> Self {
        Self {
            rbac_manager: Some(rbac_manager),
            user_role: Some(user_role),
        }
    }
}

impl Default for ListDirTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SimpleTool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List the contents of a directory."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory path to list"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let path = match input.get_str("path") {
            Some(p) => p,
            None => return ToolResult::failure("Missing 'path' parameter"),
        };

        let path = expand_tilde(Path::new(path));

        // Check permissions if RBAC is enabled
        if let (Some(rbac), Some(role)) = (&self.rbac_manager, &self.user_role) {
            match rbac.check_path_access(*role, "read", &path) {
                crate::rbac::manager::PermissionResult::Allowed => {}
                crate::rbac::manager::PermissionResult::Denied(reason) => {
                    return ToolResult::failure(format!("Permission denied: {}", reason));
                }
            }
        }

        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return ToolResult::failure(format!("Error: Directory not found: {}", path.display()));
        }

        if !tokio::fs::metadata(&path)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return ToolResult::failure(format!("Error: Not a directory: {}", path.display()));
        }

        let mut entries = match fs::read_dir(&path).await {
            Ok(e) => e,
            Err(e) => {
                return ToolResult::failure(format!("Error listing directory: {}", e).to_string());
            }
        };

        let mut items = Vec::new();
        loop {
            let entry = match entries.next_entry().await {
                Ok(e) => e,
                Err(err) => {
                    return ToolResult::failure(format!("Error reading directory: {}", err));
                }
            };
            let entry = match entry {
                Some(e) => e,
                None => break,
            };
            let name = entry.file_name().to_string_lossy().to_string();

            // Fast path: use DirEntry::file_type which doesn't require an extra stat call on most platforms
            let is_dir = if let Ok(file_type) = entry.file_type().await {
                file_type.is_dir()
            } else {
                // Fallback to metadata if file_type fails
                tokio::fs::metadata(entry.path())
                    .await
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
            };

            let prefix = if is_dir { "[dir] " } else { "[file] " };
            items.push(format!("{}{}", prefix, name));
        }

        if items.is_empty() {
            return ToolResult::success_text(format!("Directory {} is empty", path.display()));
        }

        items.sort();
        ToolResult::success_text(items.join("\n"))
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::File
    }
}

/// Expand tilde in path to home directory
fn expand_tilde(path: &Path) -> PathBuf {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path.as_os_str().to_string_lossy()[2..]);
        }
    } else if path == Path::new("~")
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[tokio::test]
    async fn test_expand_tilde() {
        let expanded = expand_tilde(Path::new("~/test"));
        // Should not start with ~ anymore
        assert!(!expanded.starts_with("~"));
    }

    /// Write `content` to a temp file and return the NamedTempFile (keeps it alive).
    async fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("create temp file");
        f.write_all(content.as_bytes()).expect("write temp file");
        f
    }

    #[tokio::test]
    async fn read_file_full_returns_all_content() {
        let content = "line1\nline2\nline3\n";
        let tmp = write_temp(content).await;
        let tool = ReadFileTool::new();
        let input = ToolInput::from_json(json!({"path": tmp.path().to_str().unwrap()}));
        let result = tool.execute(input).await;
        assert!(result.success, "expected success");
        // Full read must NOT include the range header.
        let text = result.as_text().unwrap();
        assert!(!text.contains("(lines"), "no header expected for full read");
        assert!(text.contains("line1"));
        assert!(text.contains("line3"));
    }

    #[tokio::test]
    async fn read_file_line_range_returns_slice() {
        let content = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
        let tmp = write_temp(content).await;
        let tool = ReadFileTool::new();
        let input = ToolInput::from_json(json!({
            "path": tmp.path().to_str().unwrap(),
            "start_line": 2,
            "end_line": 3
        }));
        let result = tool.execute(input).await;
        assert!(result.success, "expected success: {:?}", result.error);
        let text = result.as_text().unwrap();
        // Header must be present for a sub-range read.
        assert!(text.contains("lines 2-3 of 5"), "header missing: {}", text);
        assert!(text.contains("beta"), "beta missing");
        assert!(text.contains("gamma"), "gamma missing");
        assert!(!text.contains("alpha"), "alpha should not appear");
        assert!(!text.contains("delta"), "delta should not appear");
    }

    #[tokio::test]
    async fn read_file_invalid_start_line_returns_error() {
        let tmp = write_temp("a\nb\nc\n").await;
        let tool = ReadFileTool::new();
        // start_line=10 exceeds 3-line file.
        let input = ToolInput::from_json(json!({
            "path": tmp.path().to_str().unwrap(),
            "start_line": 10
        }));
        let result = tool.execute(input).await;
        assert!(!result.success, "should fail for out-of-range start_line");
        assert!(result.error.unwrap().contains("exceeds total line count"));
    }

    #[tokio::test]
    async fn read_file_end_line_before_start_returns_error() {
        let tmp = write_temp("a\nb\nc\n").await;
        let tool = ReadFileTool::new();
        let input = ToolInput::from_json(json!({
            "path": tmp.path().to_str().unwrap(),
            "start_line": 3,
            "end_line": 1
        }));
        let result = tool.execute(input).await;
        assert!(!result.success, "should fail when end_line < start_line");
        assert!(result.error.unwrap().contains("end_line"));
    }

    #[tokio::test]
    async fn read_file_end_line_clamped_to_total() {
        let content = "x\ny\nz\n";
        let tmp = write_temp(content).await;
        let tool = ReadFileTool::new();
        // end_line=999 should be silently clamped to the last line.
        let input = ToolInput::from_json(json!({
            "path": tmp.path().to_str().unwrap(),
            "start_line": 2,
            "end_line": 999
        }));
        let result = tool.execute(input).await;
        assert!(result.success, "should succeed with clamped end_line");
        let text = result.as_text().unwrap();
        assert!(text.contains("y"));
        assert!(text.contains("z"));
    }
}
