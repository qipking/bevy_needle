//! 纯 ECS 逻辑测试：不加载引擎（保证 CI/离线可跑）。

use bevy_ecs::prelude::*;
use bevy_needle::prelude::*;
use serde_json::json;

#[test]
fn tool_registry_and_handlers_roundtrip() {
    let mut world = World::new();
    world.init_resource::<ToolRegistry>();
    world.init_resource::<ToolHandlers>();
    world.init_resource::<Messages<ToolCallRequested>>();
    world.init_resource::<Messages<ToolCallCompleted>>();
    world.init_resource::<Messages<ToolCallFailed>>();

    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "double",
            "Doubles a number",
            ParametersBuilder::new().integer("value", "input").build(),
        )))
        .id();

    rebuild_tool_registry(&mut world);
    assert_eq!(
        world.resource::<ToolRegistry>().get_by_name("double"),
        Some(tool)
    );

    register_tool_handler(&mut world, "double", |call| {
        let value = call.args.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(ToolOutput::json(json!({ "doubled": value * 2 })))
    });

    let run = world.spawn_empty().id();
    let call = ToolCall::new(run, tool, "double", json!({ "value": 21 }));
    world.write_message(ToolCallRequested { call: call.clone() });

    queue_requested_tool_calls(&mut world);
    dispatch_registered_tool_calls(&mut world);
    publish_tool_invocation_results(&mut world);

    let completed: Vec<ToolCallCompleted> = {
        let mut messages = world.resource_mut::<Messages<ToolCallCompleted>>();
        messages.drain().collect()
    };
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].output.value["doubled"], 42);
}

#[test]
fn unknown_tool_fails_invocation_immediately() {
    let mut world = World::new();
    world.init_resource::<ToolHandlers>();
    let call = ToolCall::new(
        world.spawn_empty().id(),
        world.spawn_empty().id(),
        "missing",
        json!({}),
    );
    let invocation = world.spawn(ToolInvocationBundle::new(call)).id();
    fail_tool_invocation(&mut world, invocation, "unknown tool: missing");
    assert_eq!(
        world.get::<ToolInvocationStatus>(invocation),
        Some(&ToolInvocationStatus::Failed)
    );
}

#[test]
fn schema_validation_catches_broken_required() {
    let bad = json!({ "type": "object", "required": ["ghost"] });
    assert!(normalize_tool_schema(&bad).is_err());
    let good = ParametersBuilder::new()
        .string("text", "x")
        .optional_boolean("on", "y")
        .build();
    assert!(normalize_tool_schema(&good).is_ok());
}

#[test]
fn session_transcript_is_ordered() {
    let mut world = World::new();
    let session = spawn_session(&mut world);
    spawn_chat_message(&mut world.commands(), session, ChatMessageRole::User, "a");
    spawn_chat_message(
        &mut world.commands(),
        session,
        ChatMessageRole::Assistant,
        "b",
    );
    spawn_chat_message(&mut world.commands(), session, ChatMessageRole::User, "c");
    // 独占世界上下文需手动应用延迟命令（调度表内由同步点自动应用）
    world.flush();
    let transcript = collect_transcript(&mut world, session);
    let texts: Vec<&str> = transcript.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, vec!["a", "b", "c"]);
}

#[test]
fn parameters_builder_enums_and_ranges_feed_needle_grammar() {
    let params = ParametersBuilder::new()
        .str_enum("target", &["title", "status"], "要修改的标签")
        .int_range("value", 0, 100, "百分比")
        .optional_boolean("on", "开关")
        .build();
    let normalized = normalize_tool_schema(&params).unwrap();
    assert_eq!(normalized["required"], json!(["target", "value"]));
    assert_eq!(
        normalized["properties"]["target"]["enum"],
        json!(["title", "status"])
    );
    let tools = tools_json(&[normalized]).unwrap();
    assert!(tools.contains("target"));
}
