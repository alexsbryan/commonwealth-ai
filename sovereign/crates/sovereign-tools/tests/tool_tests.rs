use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use sovereign_tools::shell::ShellTool;

fn tool_ctx() -> ToolContext {
    ToolContext {
        conversation_id: "test".to_string(),
        task_id: None,
        working_directory: None,
    }
}

// ─── ShellTool Tests ───────────────────────────────────────────

#[test]
fn shell_tool_descriptor() {
    let tool = ShellTool;
    let desc = tool.descriptor();
    assert_eq!(desc.id, "shell");
    assert_eq!(desc.name, "Shell");
    assert!(!tool.required_permissions().is_empty());
    assert!(matches!(
        tool.required_permissions()[0],
        Permission::Shell
    ));
}

#[test]
fn shell_tool_validate_valid() {
    let tool = ShellTool;
    assert!(tool
        .validate(&serde_json::json!({"command": "echo hello"}))
        .is_ok());
}

#[test]
fn shell_tool_validate_missing_command() {
    let tool = ShellTool;
    assert!(tool.validate(&serde_json::json!({})).is_err());
    assert!(tool.validate(&serde_json::json!({"cmd": "echo"})).is_err());
}

#[tokio::test]
async fn shell_tool_echo() {
    let tool = ShellTool;
    let result = tool
        .execute(&serde_json::json!({"command": "echo hello"}), &tool_ctx())
        .await
        .unwrap();

    if let StepOutput::Text(output) = result {
        assert_eq!(output, "hello");
    } else {
        panic!("Expected StepOutput::Text");
    }
}

#[tokio::test]
async fn shell_tool_captures_stderr() {
    let tool = ShellTool;
    let result = tool
        .execute(
            &serde_json::json!({"command": "echo out && echo err >&2"}),
            &tool_ctx(),
        )
        .await
        .unwrap();

    if let StepOutput::Text(output) = result {
        assert!(output.contains("out"));
        assert!(output.contains("err"));
    } else {
        panic!("Expected StepOutput::Text");
    }
}

#[tokio::test]
async fn shell_tool_nonzero_exit() {
    let tool = ShellTool;
    let result = tool
        .execute(&serde_json::json!({"command": "exit 1"}), &tool_ctx())
        .await
        .unwrap();

    if let StepOutput::Text(output) = result {
        assert!(output.contains("exit code 1"));
    } else {
        panic!("Expected StepOutput::Text");
    }
}

#[tokio::test]
async fn shell_tool_multiline_output() {
    let tool = ShellTool;
    let result = tool
        .execute(
            &serde_json::json!({"command": "echo line1 && echo line2 && echo line3"}),
            &tool_ctx(),
        )
        .await
        .unwrap();

    if let StepOutput::Text(output) = result {
        assert!(output.contains("line1"));
        assert!(output.contains("line2"));
        assert!(output.contains("line3"));
    } else {
        panic!("Expected StepOutput::Text");
    }
}
