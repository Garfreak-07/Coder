use super::*;

#[tokio::test]
async fn mcp_manifest_validation_requires_stdio_command_and_forces_defaults_off() {
    let app = test_router();
    let missing_command = post_json(
        app.clone(),
        "/api/v3/mcp/manifests/validate",
        json!({"manifest": {"server_id": "missing"}}),
    )
    .await;
    let response = post_json(
        app,
        "/api/v3/mcp/manifests/validate",
        json!({
            "manifest": {
                "server_id": "github",
                "name": "GitHub",
                "command": "github-mcp",
                "args": ["stdio"],
                "startup_timeout_sec": 30,
                "tool_timeout_sec": 300,
                "enabled_by_default": true,
                "operations": [{
                    "name": "search_issues",
                    "risk": "low",
                    "side_effect": "read",
                    "enabled_by_default": true
                }]
            }
        }),
    )
    .await;

    assert_eq!(missing_command.status(), StatusCode::OK);
    let missing_body = response_json(missing_command).await;
    assert_eq!(missing_body["ok"], false);
    assert!(missing_body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error == "command is required for stdio MCP servers"));
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["manifest"]["command"], "github-mcp");
    assert_eq!(body["manifest"]["startup_timeout_sec"], 30);
    assert_eq!(body["manifest"]["tool_timeout_sec"], 300);
    assert_eq!(body["manifest"]["enabled_by_default"], false);
    assert_eq!(
        body["manifest"]["operations"][0]["enabled_by_default"],
        false
    );
}

#[tokio::test]
async fn mcp_server_and_tool_endpoints_start_empty_before_registration() {
    let app = test_router();
    let servers_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/mcp/servers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let tools_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/mcp/tools")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(servers_response.status(), StatusCode::OK);
    assert_eq!(tools_response.status(), StatusCode::OK);
    assert!(response_json(servers_response).await["servers"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(response_json(tools_response).await["tools"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn unknown_mcp_calls_keep_approval_and_evidence_boundaries() {
    let root = temp_root();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-1");
    store
        .write_metadata(&RunState::new(
            run_id.clone(),
            coder_core::WorkflowId::new("workflow"),
        ))
        .unwrap();
    let app = router(ApiState::new(store.clone()));

    let blocked = post_json(
        app.clone(),
        "/api/v3/mcp/tools/invoke",
        json!({
            "server_id": "missing",
            "tool_name": "unknown",
            "args": {},
            "run_id": "run-1",
            "approved": false
        }),
    )
    .await;
    let failed = post_json(
        app,
        "/api/v3/mcp/tools/invoke",
        json!({
            "server_id": "missing",
            "tool_name": "unknown",
            "args": {},
            "run_id": "run-1",
            "approved": true
        }),
    )
    .await;
    let blocked = response_json(blocked).await;
    let failed = response_json(failed).await;
    assert_eq!(blocked["status"], "blocked");
    assert_eq!(blocked["approval_key"], "mcp:missing:unknown");
    assert_eq!(failed["status"], "failed");
    assert!(failed["evidence_ref"]
        .as_str()
        .unwrap()
        .starts_with("blob://sha256/"));

    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| event.kind == "mcp.tool.blocked"));
    assert!(events
        .iter()
        .any(|event| event.kind == "mcp.tool.failed" && !event.refs.is_empty()));
    let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
fn model_mcp_test_server(root: &Path) -> (String, Vec<String>) {
    let path = root.join("model-mcp-server.ps1");
    fs::write(
        &path,
        r#"$ErrorActionPreference = 'Stop'
while ($null -ne ($line = [Console]::In.ReadLine())) {
  if ([string]::IsNullOrWhiteSpace($line)) { continue }
  $message = $line | ConvertFrom-Json
  if ($message.method -eq 'initialize') {
    $result = @{
      protocolVersion = $message.params.protocolVersion
      capabilities = @{ tools = @{ listChanged = $false } }
      serverInfo = @{ name = 'coder-model-mcp-test'; version = '1.0.0' }
    }
    [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $message.id; result = $result } | ConvertTo-Json -Depth 20 -Compress))
  } elseif ($message.method -eq 'tools/list') {
    $tool = @{
      name = 'lookup'
      description = 'Look up a value.'
      inputSchema = @{ type = 'object'; properties = @{ query = @{ type = 'string' } }; required = @('query') }
      annotations = @{ readOnlyHint = $true; openWorldHint = $false }
    }
    [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $message.id; result = @{ tools = @($tool) } } | ConvertTo-Json -Depth 20 -Compress))
  } elseif ($message.method -eq 'tools/call') {
    $result = @{
      content = @(@{ type = 'text'; text = $message.params.arguments.query })
      structuredContent = @{
        echo = $message.params.arguments.query
        secret = 'sk-test-123456789012345678901234567890'
      }
      isError = $false
    }
    [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $message.id; result = $result } | ConvertTo-Json -Depth 20 -Compress))
  }
}
"#,
    )
    .unwrap();
    (
        "powershell.exe".to_owned(),
        vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-ExecutionPolicy".to_owned(),
            "Bypass".to_owned(),
            "-File".to_owned(),
            path.display().to_string(),
        ],
    )
}

#[cfg(not(windows))]
fn model_mcp_test_server(root: &Path) -> (String, Vec<String>) {
    let path = root.join("model-mcp-server.sh");
    fs::write(
        &path,
        r#"while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      protocol=$(printf '%s' "$line" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"coder-model-mcp-test","version":"1.0.0"}}}\n' "$id" "$protocol"
      ;;
    *'"method":"tools/list"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"lookup","description":"Look up a value.","inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]},"annotations":{"readOnlyHint":true,"openWorldHint":false}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"hello"}],"structuredContent":{"echo":"hello","secret":"sk-test-123456789012345678901234567890"},"isError":false}}\n' "$id"
      ;;
  esac
done
"#,
    )
    .unwrap();
    ("sh".to_owned(), vec![path.display().to_string()])
}

#[tokio::test]
async fn model_executor_routes_frozen_tool_to_real_stdio_mcp_and_redacts_events() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let store = RunStore::new(&root);
    let run_id = RunId::from_string("run-model-mcp");
    store
        .write_metadata(&RunState::new(
            run_id.clone(),
            coder_core::WorkflowId::new("workflow"),
        ))
        .unwrap();
    let state = ApiState::new(store.clone());
    let (command, args) = model_mcp_test_server(&root);
    state
        .mcp_runtime
        .register(coder_harness::McpServerManifest {
            server_id: "local data".to_owned(),
            name: "Local Data".to_owned(),
            command,
            args,
            cwd: Some(root.display().to_string()),
            env_vars: Vec::new(),
            startup_timeout_sec: Some(10),
            tool_timeout_sec: Some(10),
            operations: Vec::new(),
            enabled_by_default: false,
        })
        .await
        .unwrap();
    let snapshot = crate::native_model_mcp::snapshot_native_model_mcp_tools(
        state.mcp_runtime.list_tools().await,
    );
    assert_eq!(snapshot.len(), 1);
    let provider_name = snapshot[0].provider_name.clone();
    assert_eq!(provider_name, "mcp__local_data__lookup");
    let executor = crate::model_tool_server_executor::server_model_tool_executor_with_mcp(
        state.clone(),
        crate::native_model_mcp::native_model_mcp_routes(&snapshot),
    );

    let result = executor
        .execute_model_tool(coder_workflow::ModelToolExecutionRequest {
            tool_use_id: "call-mcp".to_owned(),
            tool_name: provider_name.clone(),
            input: json!({"query": "hello", "approved": false}),
            turn_context: coder_workflow::TurnContext {
                run_id: Some(run_id.to_string()),
                selected_tools: vec![provider_name],
                start_work_authorized: true,
                ..coder_workflow::TurnContext::default()
            },
        })
        .await
        .unwrap();

    assert_eq!(result.status, "completed");
    assert!(!result.is_error);
    assert_eq!(
        result.payload["output"]["structuredContent"]["echo"],
        "hello"
    );
    let events = store.read_events(&run_id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "mcp.server.registered" && event.payload["enabled"] == true
    }));
    assert!(events.iter().any(|event| event.kind == "mcp.tool.started"));
    assert!(events
        .iter()
        .any(|event| event.kind == "mcp.tool.completed"));
    let persisted = serde_json::to_string(&events).unwrap();
    assert!(!persisted.contains("sk-test-123456789012345678901234567890"));

    assert!(state.mcp_runtime.remove("local data").await);
    let _ = fs::remove_dir_all(root);
}
