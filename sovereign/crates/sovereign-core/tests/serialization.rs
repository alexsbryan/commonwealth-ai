use sovereign_core::types::*;

#[test]
fn intent_roundtrip() {
    let intents = vec![
        Intent::SimpleQuery,
        Intent::DeepQuery,
        Intent::KnowledgeQuery,
        Intent::SimpleAction {
            tool: "web_search".to_string(),
        },
        Intent::ComplexTask,
        Intent::Continuation {
            task_id: "task-123".to_string(),
        },
    ];

    for intent in &intents {
        let json = serde_json::to_string(intent).unwrap();
        let back: Intent = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&back).unwrap();
        assert_eq!(json, json2);
    }
}

#[test]
fn step_kind_roundtrip() {
    let kinds = vec![
        StepKind::Reason {
            prompt_template: "Analyze {0.output}".to_string(),
            speed: Speed::Fast,
        },
        StepKind::Tool {
            tool_id: "web_search".to_string(),
            params: serde_json::json!({"query": "rust async"}),
        },
        StepKind::UserInput {
            question: "Which option?".to_string(),
        },
        StepKind::Branch {
            condition: "Is there a conflict?".to_string(),
            if_true: 3,
            if_false: 4,
        },
    ];

    for kind in &kinds {
        let json = serde_json::to_string(kind).unwrap();
        let back: StepKind = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&back).unwrap();
        assert_eq!(json, json2);
    }
}

#[test]
fn plan_roundtrip() {
    let plan = Plan {
        id: "task-1".to_string(),
        goal: "Find flights and check calendar".to_string(),
        steps: vec![
            Step {
                id: 0,
                description: "Search for flights".to_string(),
                kind: StepKind::Tool {
                    tool_id: "web_search".to_string(),
                    params: serde_json::json!({"query": "SFO to ORD"}),
                },
                requires_approval: false,
                inputs: vec![],
            },
            Step {
                id: 1,
                description: "Check calendar".to_string(),
                kind: StepKind::Tool {
                    tool_id: "calendar_read".to_string(),
                    params: serde_json::json!({"date": "next Tuesday"}),
                },
                requires_approval: false,
                inputs: vec![],
            },
            Step {
                id: 2,
                description: "Compare and decide".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "Given flights {0.output} and calendar {1.output}, pick the best option.".to_string(),
                    speed: Speed::Slow,
                },
                requires_approval: false,
                inputs: vec![
                    StepInput { step_id: 0, key: "output".to_string() },
                    StepInput { step_id: 1, key: "output".to_string() },
                ],
            },
        ],
        edges: vec![(0, 2), (1, 2)],
    };

    let json = serde_json::to_string_pretty(&plan).unwrap();
    let back: Plan = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, plan.id);
    assert_eq!(back.steps.len(), 3);
    assert_eq!(back.edges.len(), 2);
}

#[test]
fn plan_topological_batches() {
    // Steps 0,1 are independent, step 2 depends on both.
    let plan = Plan {
        id: "t1".to_string(),
        goal: "test".to_string(),
        steps: vec![
            Step {
                id: 0,
                description: "A".to_string(),
                kind: StepKind::Reason { prompt_template: "a".to_string(), speed: Speed::Fast },
                requires_approval: false,
                inputs: vec![],
            },
            Step {
                id: 1,
                description: "B".to_string(),
                kind: StepKind::Reason { prompt_template: "b".to_string(), speed: Speed::Fast },
                requires_approval: false,
                inputs: vec![],
            },
            Step {
                id: 2,
                description: "C".to_string(),
                kind: StepKind::Reason { prompt_template: "c".to_string(), speed: Speed::Slow },
                requires_approval: false,
                inputs: vec![
                    StepInput { step_id: 0, key: "output".to_string() },
                    StepInput { step_id: 1, key: "output".to_string() },
                ],
            },
        ],
        edges: vec![(0, 2), (1, 2)],
    };

    let batches = plan.topological_batches();
    assert_eq!(batches.len(), 2);
    // First batch has steps 0 and 1 (independent)
    assert_eq!(batches[0].len(), 2);
    // Second batch has step 2 (depends on both)
    assert_eq!(batches[1].len(), 1);
    assert_eq!(batches[1][0].id, 2);
}

#[test]
fn step_output_roundtrip() {
    let outputs = vec![
        StepOutput::Text("hello world".to_string()),
        StepOutput::Json(serde_json::json!({"key": "value"})),
        StepOutput::Jump(3),
        StepOutput::Skipped,
    ];

    for output in &outputs {
        let json = serde_json::to_string(output).unwrap();
        let back: StepOutput = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&back).unwrap();
        assert_eq!(json, json2);
    }
}

#[test]
fn message_roundtrip() {
    let msg = Message {
        id: "msg-1".to_string(),
        conversation_id: "conv-1".to_string(),
        role: Role::User,
        content: "Hello!".to_string(),
        created_at: 1711900000,
        metadata: Some(serde_json::json!({"model": "qwen-1.7b"})),
    };

    let json = serde_json::to_string(&msg).unwrap();
    let back: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, msg.id);
    assert_eq!(back.role, Role::User);
}

#[test]
fn completion_request_builders() {
    let req = CompletionRequest::new("test prompt")
        .with_speed(Speed::Fast)
        .with_system("You are helpful.");

    assert_eq!(req.prompt, "test prompt");
    assert_eq!(req.preferred_speed, Speed::Fast);
    assert_eq!(req.system_message.as_deref(), Some("You are helpful."));

    let yn = CompletionRequest::yes_no("Is it raining?", "Weather: sunny");
    assert_eq!(yn.preferred_speed, Speed::Fast);
    assert_eq!(yn.max_tokens, Some(5));
}

#[test]
fn completion_response_as_bool() {
    let yes = CompletionResponse {
        text: "yes".to_string(),
        tokens_used: 1,
        model_id: "test".to_string(),
        latency_ms: 10,
        oicp_meta: None,
    };
    assert!(yes.as_bool());

    let no = CompletionResponse {
        text: "no".to_string(),
        tokens_used: 1,
        model_id: "test".to_string(),
        latency_ms: 10,
        oicp_meta: None,
    };
    assert!(!no.as_bool());
}
