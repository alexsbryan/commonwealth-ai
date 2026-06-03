use std::path::{Path, PathBuf};

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

/// Scoped filesystem tool. All operations are restricted to allowed root directories.
pub struct FileTool {
    allowed_roots: Vec<PathBuf>,
}

impl FileTool {
    pub fn new(allowed_roots: Vec<PathBuf>) -> Self {
        Self { allowed_roots }
    }

    /// Validate that a path is within one of the allowed roots.
    fn validate_path(&self, path: &Path) -> Result<PathBuf> {
        let canonical = path
            .canonicalize()
            .map_err(|e| Error::InvalidInput(format!("Invalid path: {e}")))?;

        for root in &self.allowed_roots {
            if let Ok(root_canonical) = root.canonicalize() {
                if canonical.starts_with(&root_canonical) {
                    return Ok(canonical);
                }
            }
        }

        Err(Error::InvalidInput(format!(
            "Path {} is outside allowed directories",
            path.display()
        )))
    }
}

#[async_trait]
impl Tool for FileTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "file".to_string(),
            name: "File".to_string(),
            description: "Read, write, list, and search files within allowed directories"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["read", "write", "list", "search"] },
                    "path": { "type": "string" },
                    "content": { "type": "string", "description": "For write action" },
                    "pattern": { "type": "string", "description": "Glob pattern for search" }
                },
                "required": ["action", "path"]
            }),
            examples: vec![],
            effect: Effect::ReadWrite,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            // Shape depends on action — read returns file contents,
            // write returns a status string, list returns a newline
            // list. Leave unschema'd rather than promise structure
            // that doesn't hold across actions.
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        // The tool multiplexes read/write/list/search on `action`.
        // The approval gate walks this vec per invocation and stores
        // a (tool_id, scope) grant per permission — so the operator
        // grants `FileRead` the first time a `read`/`list`/`search`
        // fires, and `FileWrite` the first time a `write` fires.
        // Returning both keeps the check correct regardless of
        // action; the Effect::ReadWrite declaration on the
        // descriptor matches.
        vec![Permission::FileRead, Permission::FileWrite]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'action' parameter".to_string()))?;

        let path_str = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'path' parameter".to_string()))?;

        let path = PathBuf::from(path_str);

        match action {
            "read" => {
                let validated = self.validate_path(&path)?;
                let content = tokio::fs::read_to_string(&validated)
                    .await
                    .map_err(|e| Error::Execution(format!("Failed to read file: {e}")))?;
                // Truncate large files.
                let max = 8000;
                if content.len() > max {
                    Ok(StepOutput::Text(format!(
                        "{}\n\n[Truncated: {} bytes total]",
                        &content[..max],
                        content.len()
                    )))
                } else {
                    Ok(StepOutput::Text(content))
                }
            }
            "write" => {
                let content = params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Error::InvalidInput("Missing 'content' for write".to_string())
                    })?;

                // Validate parent directory exists within roots.
                if let Some(parent) = path.parent() {
                    if parent.exists() {
                        self.validate_path(parent)?;
                    }
                }
                // For new files, validate against root prefixes directly.
                let valid = self.allowed_roots.iter().any(|root| path.starts_with(root));
                if !valid {
                    return Err(Error::InvalidInput(format!(
                        "Path {} is outside allowed directories",
                        path.display()
                    )));
                }

                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        Error::Execution(format!("Failed to create directories: {e}"))
                    })?;
                }

                tokio::fs::write(&path, content)
                    .await
                    .map_err(|e| Error::Execution(format!("Failed to write file: {e}")))?;

                Ok(StepOutput::Text(format!(
                    "Written {} bytes to {}",
                    content.len(),
                    path.display()
                )))
            }
            "list" => {
                let validated = self.validate_path(&path)?;
                let mut entries = Vec::new();
                let mut reader = tokio::fs::read_dir(&validated)
                    .await
                    .map_err(|e| Error::Execution(format!("Failed to list directory: {e}")))?;

                while let Ok(Some(entry)) = reader.next_entry().await {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                    entries.push(if is_dir { format!("{name}/") } else { name });
                }

                entries.sort();
                Ok(StepOutput::Text(entries.join("\n")))
            }
            "search" => {
                let pattern = params
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Error::InvalidInput("Missing 'pattern' for search".to_string())
                    })?;

                let validated = self.validate_path(&path)?;
                let glob_pattern = format!("{}/{pattern}", validated.display());
                let matches: Vec<String> = glob::glob(&glob_pattern)
                    .map_err(|e| Error::InvalidInput(format!("Invalid glob: {e}")))?
                    .filter_map(|entry| entry.ok())
                    .take(100)
                    .map(|p| p.display().to_string())
                    .collect();

                if matches.is_empty() {
                    Ok(StepOutput::Text(
                        "No files matched the pattern.".to_string(),
                    ))
                } else {
                    Ok(StepOutput::Text(matches.join("\n")))
                }
            }
            _ => Err(Error::InvalidInput(format!("Unknown action: {action}"))),
        }
    }
}
