use super::*;

#[tokio::test]
async fn model_tool_command_hooks_receive_claude_style_stdin() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hook-stdin");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Hook stdin\n").unwrap();
    let stdin_capture = repo.join("hook-input.json");
    let hook_command = hook_capture_stdin_command(&stdin_capture);
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("repo_read_file".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Command {
                command: hook_command.command,
                if_condition: None,
                shell: Some(hook_command.shell),
                timeout: None,
                status_message: None,
                once: false,
                run_async: false,
                async_rewake: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-stdin",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-hook-stdin"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let captured = fs::read_to_string(&stdin_capture).unwrap();
    let hook_input =
        serde_json::from_str::<Value>(captured.trim_start_matches('\u{feff}').trim()).unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "repo_read_file");
    assert_eq!(hook_input["tool_use_id"], "toolu-hook-stdin");
    assert_eq!(hook_input["tool_input"]["path"], "README.md");
    assert_eq!(hook_input["session_id"], "run-model-tool-hook-stdin");
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(
        phases[0]["hook_results"][0]["stdin_protocol"],
        "claude.hook_input.v1"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "plain_text"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn model_tool_hook_output_parser_applies_pretool_effects() {
    let output = json!({
        "continue": false,
        "stopReason": "policy stopped follow-up",
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": "needs review",
            "updatedInput": {
                "path": "after.txt"
            },
            "additionalContext": "rewrote path"
        }
    })
    .to_string();

    let parsed = crate::model_tool_hook_output::parse_model_tool_hook_output(
        &output,
        coder_config::HookEvent::PreToolUse,
        "hook-command",
    );

    assert_eq!(parsed.kind, "hook_json");
    assert_eq!(parsed.validation_error, None);
    assert_eq!(parsed.blocking_error, None);
    assert_eq!(parsed.effects.permission_behavior, Some("ask"));
    assert_eq!(
        parsed.effects.permission_decision_reason.as_deref(),
        Some("needs review")
    );
    assert_eq!(parsed.effects.updated_input.unwrap()["path"], "after.txt");
    assert_eq!(
        parsed.effects.additional_context.unwrap(),
        json!("rewrote path")
    );
    assert!(parsed.effects.prevent_continuation);
    assert_eq!(
        parsed.effects.stop_reason.as_deref(),
        Some("policy stopped follow-up")
    );
}

#[test]
fn model_tool_hook_output_parser_rejects_non_object_updated_input() {
    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": "not an object"
        }
    })
    .to_string();

    let parsed = crate::model_tool_hook_output::parse_model_tool_hook_output(
        &output,
        coder_config::HookEvent::PreToolUse,
        "hook-command",
    );

    assert_eq!(parsed.kind, "invalid_hook_json");
    assert_eq!(
        parsed.validation_error.as_deref(),
        Some("hookSpecificOutput.updatedInput must be an object")
    );
    assert_eq!(parsed.blocking_error, None);
}

#[test]
fn model_tool_command_hooks_parse_async_response_output() {
    let parsed = crate::model_tool_command_hooks::parse_async_hook_response_output(
        "debug line\n{\"systemMessage\":\"async note\"}\n",
    )
    .unwrap();
    assert_eq!(parsed.kind, "hook_json");
    assert_eq!(parsed.response["systemMessage"], "async note");

    let plain =
        crate::model_tool_command_hooks::parse_async_hook_response_output("plain hook output")
            .unwrap();
    assert_eq!(plain.kind, "plain_text");
    assert_eq!(plain.response, json!({}));

    assert!(crate::model_tool_command_hooks::parse_async_hook_response_output("   ").is_none());
}

#[test]
fn model_tool_command_hooks_build_shell_argv() {
    assert_eq!(
        crate::model_tool_command_hooks::CLAUDE_DEFAULT_HOOK_SHELL,
        "bash"
    );
    let bash = crate::model_tool_command_hooks::shell_command_hook_argv("echo ok", Some("bash"));
    assert_eq!(bash[1..], ["-lc", "echo ok"]);
    assert_ne!(bash[0], "cmd.exe");
    if !cfg!(windows) {
        assert_eq!(bash[0], "bash");
    }

    let powershell = crate::model_tool_command_hooks::shell_command_hook_argv(
        "Write-Output ok",
        Some("powershell"),
    );
    assert_eq!(
        powershell,
        vec![
            if cfg!(windows) {
                "powershell.exe"
            } else {
                "pwsh"
            },
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Write-Output ok",
        ]
    );

    assert_eq!(
        crate::model_tool_command_hooks::shell_command_hook_argv("Write-Output ok", Some("pwsh")),
        vec![
            "pwsh",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Write-Output ok",
        ]
    );

    let default = crate::model_tool_command_hooks::shell_command_hook_argv("echo ok", None);
    assert_eq!(default[1..], ["-lc", "echo ok"]);
    assert_ne!(default[0], "cmd.exe");
    if !cfg!(windows) {
        assert_eq!(default[0], "bash");
    }

    let unknown =
        crate::model_tool_command_hooks::shell_command_hook_argv("echo ok", Some("unknown"));
    assert_eq!(unknown[1..], ["-lc", "echo ok"]);
    assert_ne!(unknown[0], "cmd.exe");
}

#[test]
fn model_tool_command_hooks_follow_claude_windows_path_rules() {
    assert_eq!(
        crate::model_tool_command_hooks::windows_path_to_posix_path(r"C:\Users\foo"),
        "/c/Users/foo"
    );
    assert_eq!(
        crate::model_tool_command_hooks::windows_path_to_posix_path(r"D:\Work\project"),
        "/d/Work/project"
    );
    assert_eq!(
        crate::model_tool_command_hooks::windows_path_to_posix_path(r"\\server\share\dir"),
        "//server/share/dir"
    );
    assert_eq!(
        crate::model_tool_command_hooks::windows_path_to_posix_path(r"src\main.ts"),
        "src/main.ts"
    );
}

#[test]
fn model_tool_command_hooks_derive_git_bash_from_git_executable() {
    let git_path = std::path::Path::new("/opt/git/cmd/git.exe");
    let bash_path =
        crate::model_tool_command_hooks::git_bash_path_from_git_executable(git_path).unwrap();
    assert_eq!(bash_path, std::path::Path::new("/opt/git/bin/bash.exe"));
}

#[tokio::test]
async fn model_tool_pre_hook_json_updated_input_rewrites_tool_execution() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hook-updated-input");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("before.txt"), "before\n").unwrap();
    fs::write(repo.join("after.txt"), "after\n").unwrap();
    let hook_output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": {
                "repo_root": repo,
                "path": "after.txt",
                "run_id": "run-model-tool-hook-updated-input"
            },
            "additionalContext": "rewrote path"
        }
    });
    let hook_output_path = repo.join("hook-output.json");
    fs::write(
        &hook_output_path,
        serde_json::to_string(&hook_output).unwrap(),
    )
    .unwrap();
    let hook_command = hook_emit_file_command(&hook_output_path);
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Command {
                command: hook_command.command,
                if_condition: None,
                shell: Some(hook_command.shell),
                timeout: None,
                status_message: None,
                once: false,
                run_async: false,
                async_rewake: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-updated-input",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "before.txt",
                "run_id": "run-model-tool-hook-updated-input"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("after"));
    assert!(!body["content"].as_str().unwrap().contains("before"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["updated_input_applied"], true);
    assert_eq!(phases[0]["additional_contexts"][0], "rewrote path");
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "hook_json"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["updated_input"]["path"],
        "after.txt"
    );
    assert_eq!(phases[2]["phase"], "tool_execution");
    assert_eq!(phases[2]["status"], "completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_prompt_pre_hook_blocks_when_model_returns_not_ok() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-prompt-hook-block");
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "content": "{\"ok\": false, \"reason\": \"verifier rejected tool input\"}"
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.models.insert(
        "hook_verifier".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "prompt-hook-model".to_owned(),
            base_url_env: None,
            api_key_env: None,
            capabilities: coder_config::ModelCapabilities::default(),
        },
    );
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Prompt {
                prompt: "Verify this tool input: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: Some(5),
                model: Some("hook_verifier".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Prompt hook\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-prompt-hook-block",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-prompt-hook-block"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("verifier rejected tool input"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "blocked");
    assert_eq!(phases[0]["prompt_hook_count"], 1);
    assert_eq!(phases[0]["unsupported_hook_count"], 0);
    assert_eq!(phases[0]["hook_results"][0]["type"], "prompt");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "blocking");
    assert_eq!(phases[0]["hook_results"][0]["model"], "prompt-hook-model");
    assert_eq!(
        phases[0]["hook_results"][0]["model_source"],
        "hook_config_model"
    );
    assert_eq!(phases[0]["hook_results"][0]["default_timeout_seconds"], 30);
    assert_eq!(
        phases[0]["hook_results"][0]["request_protocol"],
        "claude.hook_input.v1"
    );
    assert_eq!(phases[1]["phase"], "permission_decision");
    assert_eq!(phases[1]["status"], "skipped_pre_tool_use_hook_blocked");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_pre_hook_blocks_when_structured_output_not_ok() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-block");
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [
                        {
                            "id": "call-agent-structured-output",
                            "type": "function",
                            "function": {
                                "name": "StructuredOutput",
                                "arguments": "{\"ok\": false, \"reason\": \"agent rejected tool input\"}"
                            }
                        }
                    ]
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.models.insert(
        "hook_agent".to_owned(),
        ConfigModelSpec {
            provider: "openai-compatible".to_owned(),
            model: "agent-hook-model".to_owned(),
            base_url_env: None,
            api_key_env: None,
            capabilities: coder_config::ModelCapabilities::default(),
        },
    );
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook block\n").unwrap();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Verify this tool input: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: None,
                model: Some("hook_agent".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-block",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-block"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "blocked");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("agent rejected tool input"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "blocked");
    assert_eq!(phases[0]["agent_hook_count"], 1);
    assert_eq!(phases[0]["unsupported_hook_count"], 0);
    assert_eq!(phases[0]["executed_hook_count"], 1);
    assert_eq!(phases[0]["execution_status"], "blocked");
    let hook_result = &phases[0]["hook_results"][0];
    assert_eq!(hook_result["type"], "agent");
    assert_eq!(hook_result["outcome"], "blocking");
    assert_eq!(hook_result["default_timeout_seconds"], 60);
    assert_eq!(hook_result["max_agent_turns"], 50);
    assert_eq!(hook_result["model"], "agent-hook-model");
    assert_eq!(hook_result["model_source"], "hook_config_model");
    assert_eq!(hook_result["structured_output_tool"], "StructuredOutput");
    assert_eq!(
        hook_result["hook_output_kind"],
        "agent_structured_output_tool"
    );
    assert_eq!(hook_result["hook_json_output"]["ok"], false);
    assert_eq!(
        hook_result["runtime_contract"]["isolated_agent_id_prefix"],
        "hook-agent-"
    );
    assert!(hook_result["processed_prompt_preview"]
        .as_str()
        .unwrap()
        .contains("\"path\":\"README.md\""));
    assert!(
        hook_result["available_tools_policy"]["filtered_tool_families"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().contains("agent_subagent"))
    );
    assert_eq!(phases[1]["phase"], "permission_decision");
    assert_eq!(phases[1]["status"], "skipped_pre_tool_use_hook_blocked");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_hook_sends_structured_output_tool_request_and_allows_ok() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-request");
    let (provider_base_url, captured) = spawn_openai_compatible_capture_test_server(json!({
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [
                        {
                            "id": "call-agent-structured-output-ok",
                            "type": "function",
                            "function": {
                                "name": "StructuredOutput",
                                "arguments": "{\"ok\": true}"
                            }
                        }
                    ]
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Check argument payload: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: None,
                model: Some("literal-agent-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook request\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-request",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-request"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Agent hook request"));
    let captured = captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured["model"], "literal-agent-hook-model");
    assert_eq!(captured["temperature"], 0);
    assert_eq!(captured["max_tokens"], 256);
    assert_eq!(captured["tool_choice"], "auto");
    assert_eq!(captured["tools"].as_array().unwrap().len(), 8);
    let tool_names = captured["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"StructuredOutput"));
    assert!(tool_names.contains(&"repo_read_file"));
    assert!(!tool_names.contains(&"agent_subagent"));
    assert!(!tool_names.contains(&"command_run"));
    assert_eq!(
        captured["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "StructuredOutput")
            .unwrap()["function"]["parameters"]["required"],
        json!(["ok"])
    );
    let user_prompt = captured["messages"][1]["content"].as_str().unwrap();
    assert!(user_prompt.contains("Check argument payload:"));
    assert!(user_prompt.contains("\"hook_event_name\":\"PreToolUse\""));
    assert!(user_prompt.contains("\"tool_use_id\":\"toolu-agent-hook-request\""));
    assert!(user_prompt.contains("\"path\":\"README.md\""));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "agent");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "success");
    assert_eq!(
        phases[0]["hook_results"][0]["model_source"],
        "hook_literal_model"
    );
    assert_eq!(phases[0]["hook_results"][0]["default_timeout_seconds"], 60);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_hook_can_use_read_only_tool_before_structured_output() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-tool-loop");
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook tool loop\n").unwrap();
    let (provider_base_url, captured) = spawn_openai_compatible_sequence_capture_test_server(vec![
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent-read-file",
                                "type": "function",
                                "function": {
                                    "name": "repo_read_file",
                                    "arguments": json!({
                                        "repo_root": repo,
                                        "path": "README.md"
                                    }).to_string()
                                }
                            }
                        ]
                    }
                }
            ]
        }),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent-structured-output-after-read",
                                "type": "function",
                                "function": {
                                    "name": "StructuredOutput",
                                    "arguments": "{\"ok\": true}"
                                }
                            }
                        ]
                    }
                }
            ]
        }),
    ])
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Inspect before allowing: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: None,
                model: Some("literal-agent-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-tool-loop",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-tool-loop"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Agent hook tool loop"));
    let captured = captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let second_messages = captured[1]["messages"].as_array().unwrap();
    assert!(second_messages.iter().any(|message| {
        message["role"] == "tool"
            && message["tool_call_id"] == "call-agent-read-file"
            && message["content"]
                .as_str()
                .unwrap()
                .contains("Agent hook tool loop")
    }));
    let phases = body["phases"].as_array().unwrap();
    let hook_result = &phases[0]["hook_results"][0];
    assert_eq!(hook_result["type"], "agent");
    assert_eq!(hook_result["outcome"], "success");
    assert_eq!(hook_result["assistant_turns"], 2);
    assert_eq!(hook_result["tool_call_count"], 2);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_hook_malformed_structured_output_is_non_blocking() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-malformed");
    let provider_base_url = spawn_openai_compatible_test_server_with_payload(json!({
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [
                        {
                            "id": "call-agent-malformed-structured-output",
                            "type": "function",
                            "function": {
                                "name": "StructuredOutput",
                                "arguments": "{\"reason\": \"missing ok\"}"
                            }
                        }
                    ]
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Malformed output should not block: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: Some(5),
                model: Some("literal-agent-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook malformed\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-malformed",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-malformed"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Agent hook malformed"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "agent");
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "non_blocking_error"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "invalid_agent_hook_structured_output"
    );
    assert!(phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .contains("boolean field 'ok'"));
    assert_eq!(phases[0]["hook_results"][0]["assistant_turns"], 1);
    assert_eq!(phases[1]["phase"], "permission_decision");
    assert_eq!(phases[1]["status"], "delegated_to_tool_endpoint");
    assert_eq!(phases[2]["phase"], "tool_execution");
    assert_eq!(phases[2]["status"], "completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_agent_hook_provider_timeout_is_non_blocking() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-agent-hook-timeout");
    let provider_base_url = spawn_delayed_openai_compatible_test_server(
        Duration::from_millis(1_500),
        json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": Value::Null,
                        "tool_calls": [
                            {
                                "id": "call-agent-timeout-structured-output",
                                "type": "function",
                                "function": {
                                    "name": "StructuredOutput",
                                    "arguments": "{\"ok\": true}"
                                }
                            }
                        ]
                    }
                }
            ]
        }),
    )
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Agent {
                prompt: "Timeout should not block: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: Some(1),
                model: Some("literal-agent-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Agent hook timeout\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-agent-hook-timeout",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-agent-hook-timeout"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Agent hook timeout"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["hook_results"][0]["type"], "agent");
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "non_blocking_error"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "request_timeout"
    );
    assert_eq!(phases[0]["hook_results"][0]["timeout_seconds"], 1);
    let validation_error = phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(validation_error.contains("timed out after 1 second(s)"));
    assert_eq!(phases[2]["phase"], "tool_execution");
    assert_eq!(phases[2]["status"], "completed");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_prompt_hook_sends_claude_arguments_and_schema_request() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-prompt-hook-request");
    let (provider_base_url, captured) = spawn_openai_compatible_capture_test_server(json!({
        "choices": [
            {
                "message": {
                    "content": "{\"ok\": true}"
                }
            }
        ]
    }))
    .await;
    let mut config = default_project_config();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Prompt {
                prompt: "Check argument payload: $ARGUMENTS".to_owned(),
                if_condition: None,
                timeout: None,
                model: Some("literal-hook-model".to_owned()),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let state = ApiState::new(store.clone());
    configure_test_provider(&state, provider_base_url, "default-provider-model");
    let app = router(state);
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Prompt hook request\n").unwrap();

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-prompt-hook-request",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-prompt-hook-request"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let captured = captured.lock().unwrap().clone().unwrap();
    assert_eq!(captured["model"], "literal-hook-model");
    assert_eq!(captured["temperature"], 0);
    assert_eq!(captured["max_tokens"], 256);
    assert_eq!(captured["response_format"]["type"], "json_schema");
    assert_eq!(
        captured["response_format"]["json_schema"]["schema"]["required"],
        json!(["ok"])
    );
    let user_prompt = captured["messages"][1]["content"].as_str().unwrap();
    assert!(user_prompt.contains("Check argument payload:"));
    assert!(user_prompt.contains("\"hook_event_name\":\"PreToolUse\""));
    assert!(user_prompt.contains("\"tool_use_id\":\"toolu-prompt-hook-request\""));
    assert!(user_prompt.contains("\"path\":\"README.md\""));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "prompt");
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "success");
    assert_eq!(
        phases[0]["hook_results"][0]["model_source"],
        "hook_literal_model"
    );
    assert_eq!(phases[0]["hook_results"][0]["default_timeout_seconds"], 30);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_pre_hook_updates_input_and_receives_claude_payload() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("before.txt"), "before\n").unwrap();
    fs::write(repo.join("after.txt"), "after-webhook\n").unwrap();
    let hook_response = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": {
                "repo_root": repo,
                "path": "after.txt",
                "run_id": "run-model-tool-webhook"
            },
            "additionalContext": "webhook context"
        }
    });
    let (hook_url, captured) = spawn_webhook_test_server(hook_response).await;
    let token_env = "CODER_WEBHOOK_TEST_TOKEN";
    let previous_token = std::env::var_os(token_env);
    std::env::set_var(token_env, "secret-token");
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::from([
                    ("Authorization".to_owned(), format!("Bearer ${token_env}")),
                    (
                        "X-Not-Allowed".to_owned(),
                        "$CODER_WEBHOOK_DENIED".to_owned(),
                    ),
                ]),
                allowed_env_vars: vec![token_env.to_owned()],
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "before.txt",
                "run_id": "run-model-tool-webhook"
            }
        }),
    )
    .await;
    restore_env_var(token_env, previous_token);

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("after-webhook"));
    assert!(!body["content"].as_str().unwrap().contains("before"));
    let captured = captured.lock().unwrap();
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer secret-token")
    );
    assert_eq!(captured.not_allowed.as_deref(), Some(""));
    let hook_input = captured.body.as_ref().unwrap();
    assert_eq!(hook_input["hook_event_name"], "PreToolUse");
    assert_eq!(hook_input["tool_name"], "repo_read_file");
    assert_eq!(hook_input["tool_use_id"], "toolu-webhook");
    assert_eq!(hook_input["tool_input"]["path"], "before.txt");
    drop(captured);
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["updated_input_applied"], true);
    assert_eq!(phases[0]["webhook_hook_count"], 1);
    assert_eq!(phases[0]["hook_results"][0]["type"], "webhook");
    assert_eq!(phases[0]["hook_results"][0]["hook_transport"], "webhook");
    assert_eq!(
        phases[0]["hook_results"][0]["request_protocol"],
        "claude.hook_input.v1"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "hook_json"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["ssrf_guard"]["mode"],
        "ip_literal"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["ssrf_guard"]["socket_bound"],
        true
    );
    assert_eq!(
        phases[0]["hook_results"][0]["updated_input"]["path"],
        "after.txt"
    );
    assert_eq!(phases[0]["additional_contexts"][0], "webhook context");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_hooks_respect_allowed_url_policy() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-url-policy");
    let mut config = default_project_config();
    config.allowed_webhook_urls = Some(vec!["https://hooks.example.com/*".to_owned()]);
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# URL policy\n").unwrap();
    let (hook_url, captured) = spawn_webhook_test_server(json!({})).await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url.clone(),
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-url-policy",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-url-policy"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("URL policy"));
    assert!(captured.lock().unwrap().body.is_none());
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "webhook");
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "non_blocking_error"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "webhook_policy_blocked"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["policy"],
        "allowed_webhook_urls"
    );
    assert!(phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .contains("allowedWebhookUrls"));
    assert_eq!(phases[0]["executed_hook_count"], 1);
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn model_tool_webhook_transport_policy_requires_https_for_external_urls() {
    assert!(crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "https://hooks.example.com/coder",
        None,
    )
    .is_ok());
    assert!(crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "http://localhost:8765/hooks",
        None,
    )
    .is_ok());
    assert!(crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "http://127.0.0.1:8765/hooks",
        None,
    )
    .is_ok());
    assert!(crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "http://[::1]:8765/hooks",
        None,
    )
    .is_ok());

    let error = crate::model_tool_webhook_hooks::enforce_webhook_url_policy(
        "http://hooks.example.com/coder",
        Some(&["*".to_owned()]),
    )
    .unwrap_err();
    assert!(error.contains("must use https://"));
    assert!(error.contains("loopback"));
}

#[tokio::test]
async fn model_tool_webhook_hooks_block_private_metadata_addresses() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-ssrf");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# SSRF\n").unwrap();
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: "https://169.254.169.254/latest/meta-data".to_owned(),
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-ssrf",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-ssrf"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("SSRF"));
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["hook_results"][0]["type"], "webhook");
    assert_eq!(phases[0]["hook_results"][0]["policy"], "ssrf_guard");
    assert!(phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .contains("private/link-local"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[test]
fn model_tool_webhook_ssrf_resolver_validates_connection_addresses() {
    let blocked_metadata = std::net::SocketAddr::from(([169, 254, 169, 254], 0));
    let blocked_cgnat = std::net::SocketAddr::from(([100, 100, 100, 200], 0));
    let blocked_mapped_v6 =
        std::net::SocketAddr::new(std::net::IpAddr::V6("::ffff:a9fe:a9fe".parse().unwrap()), 0);
    let allowed_loopback = std::net::SocketAddr::from(([127, 0, 0, 1], 0));

    for addr in [blocked_metadata, blocked_cgnat, blocked_mapped_v6] {
        let error = crate::model_tool_webhook_hooks::validate_webhook_resolved_socket_addrs(
            "metadata.example",
            vec![addr],
        )
        .unwrap_err();
        assert!(error.contains("private/link-local"));
    }

    assert!(
        crate::model_tool_webhook_hooks::validate_webhook_resolved_socket_addrs(
            "localhost",
            vec![allowed_loopback],
        )
        .is_ok()
    );
}

#[test]
fn model_tool_webhook_proxy_policy_uses_shared_environment_route_rules() {
    assert!(crate::outbound_http::url_matches_no_proxy(
        "https://api.example.com/v1",
        Some("*")
    ));
    assert!(crate::outbound_http::url_matches_no_proxy(
        "https://api.example.com/v1",
        Some(".example.com")
    ));
    assert!(crate::outbound_http::url_matches_no_proxy(
        "https://example.com/v1",
        Some(".example.com")
    ));
    assert!(!crate::outbound_http::url_matches_no_proxy(
        "https://notexample.com/v1",
        Some(".example.com")
    ));
    assert!(crate::outbound_http::url_matches_no_proxy(
        "https://api.example.com:8443/v1",
        Some("api.example.com:8443")
    ));
    assert!(!crate::outbound_http::url_matches_no_proxy(
        "https://api.example.com:8443/v1",
        Some("api.example.com:443")
    ));

    let env_map = BTreeMap::from([
        ("HTTP_PROXY".to_owned(), "http://upper-http:8080".to_owned()),
        (
            "HTTPS_PROXY".to_owned(),
            "http://upper-https:8080".to_owned(),
        ),
        (
            "https_proxy".to_owned(),
            "http://lower-https:8080".to_owned(),
        ),
        ("NO_PROXY".to_owned(), "blocked.example".to_owned()),
        ("no_proxy".to_owned(), ".internal.example".to_owned()),
    ]);
    let proxied = crate::model_tool_webhook_hooks::webhook_proxy_policy_for_env(
        "https://external.example/hook",
        &env_map,
    );
    assert!(proxied.uses_proxy());
    let proxied_report = proxied.report();
    assert_eq!(proxied_report["mode"], "env_proxy");
    assert_eq!(proxied_report["proxy_source"], "https_proxy");
    assert_eq!(proxied_report["no_proxy_source"], "no_proxy");

    let bypassed = crate::model_tool_webhook_hooks::webhook_proxy_policy_for_env(
        "https://api.internal.example/hook",
        &env_map,
    );
    assert!(!bypassed.uses_proxy());
    let bypassed_report = bypassed.report();
    assert_eq!(bypassed_report["mode"], "env_proxy_bypassed");
    assert_eq!(bypassed_report["proxy_configured"], true);
    assert_eq!(bypassed_report["proxy_bypassed"], true);
}

#[tokio::test]
async fn model_tool_webhook_header_env_vars_intersect_project_policy() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-env-policy");
    let mut config = default_project_config();
    config.webhook_allowed_env_vars = Some(Vec::new());
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Env policy\n").unwrap();
    let (hook_url, captured) = spawn_webhook_test_server(json!({})).await;
    let token_env = "CODER_WEBHOOK_GLOBAL_DENY_TOKEN";
    let previous_token = std::env::var_os(token_env);
    std::env::set_var(token_env, "secret-token");
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::from([("X-Not-Allowed".to_owned(), format!("${token_env}"))]),
                allowed_env_vars: vec![token_env.to_owned()],
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-env-policy",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-env-policy"
            }
        }),
    )
    .await;
    restore_env_var(token_env, previous_token);

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    let captured = captured.lock().unwrap();
    assert_eq!(captured.not_allowed.as_deref(), Some(""));
    drop(captured);
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(
        phases[0]["hook_results"][0]["effective_allowed_env_vars"],
        json!([])
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_hooks_respect_network_permission() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-deny");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Deny;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Network denied\n").unwrap();
    let (hook_url, captured) = spawn_webhook_test_server(json!({})).await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-deny",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-deny"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(captured.lock().unwrap().body.is_none());
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "permission_blocked");
    assert_eq!(phases[0]["execution_status"], "skipped_permission_required");
    assert_eq!(phases[0]["webhook_hook_count"], 1);
    assert_eq!(phases[0]["executed_hook_count"], 0);
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "skipped_permission_required"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["required_permission"],
        "network"
    );
    assert_eq!(phases[0]["hook_results"][0]["permission_behavior"], "deny");
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_non_json_success_is_non_blocking() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-non-json");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Non JSON hook\n").unwrap();
    let (hook_url, captured) = spawn_raw_webhook_test_server(
        StatusCode::OK,
        "text/plain",
        "plain hook output",
        Duration::ZERO,
    )
    .await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(5),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-non-json",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-non-json"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"].as_str().unwrap().contains("Non JSON hook"));
    assert!(captured.lock().unwrap().body.is_some());
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["webhook_hook_count"], 1);
    assert_eq!(phases[0]["executed_hook_count"], 1);
    assert_eq!(
        phases[0]["hook_results"][0]["outcome"],
        "non_blocking_error"
    );
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "invalid_webhook_json"
    );
    assert_eq!(phases[0]["hook_results"][0]["status_code"], 200);
    assert_eq!(
        phases[0]["hook_results"][0]["output_preview"],
        "plain hook output"
    );
    assert!(phases[0]["hook_results"][0]["hook_output_validation_error"]
        .as_str()
        .unwrap()
        .contains("must return JSON"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_webhook_timeout_is_non_blocking() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-webhook-timeout");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .network = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "# Webhook timeout\n").unwrap();
    let (hook_url, captured) = spawn_raw_webhook_test_server(
        StatusCode::OK,
        "application/json",
        "{}",
        Duration::from_secs(2),
    )
    .await;
    config.hooks = coder_config::HookSettings {
        pre_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Webhook {
                url: hook_url,
                if_condition: None,
                timeout: Some(1),
                headers: BTreeMap::new(),
                allowed_env_vars: Vec::new(),
                status_message: None,
                once: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-webhook-timeout",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-webhook-timeout"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("Webhook timeout"));
    assert!(captured.lock().unwrap().body.is_some());
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[0]["phase"], "pre_tool_use_hooks");
    assert_eq!(phases[0]["status"], "completed");
    assert_eq!(phases[0]["webhook_hook_count"], 1);
    assert_eq!(phases[0]["executed_hook_count"], 1);
    assert_eq!(phases[0]["hook_results"][0]["outcome"], "execution_error");
    assert_eq!(
        phases[0]["hook_results"][0]["hook_output_kind"],
        "request_timeout"
    );
    assert_eq!(phases[0]["hook_results"][0]["aborted"], true);
    assert_eq!(phases[0]["hook_results"][0]["timeout_seconds"], 1);
    assert_eq!(
        phases[0]["hook_results"][0]["default_timeout_seconds"],
        CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS
    );
    let error_text = phases[0]["hook_results"][0]["error"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(error_text.contains("timeout") || error_text.contains("timed out"));
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}

#[tokio::test]
async fn model_tool_post_hook_json_updated_output_rewrites_model_result() {
    let store_root = temp_root();
    let store = RunStore::new(&store_root);
    let run_id = RunId::from_string("run-model-tool-hook-updated-output");
    let mut config = default_project_config();
    config
        .harnesses
        .get_mut("native-code-edit")
        .unwrap()
        .permissions
        .run_commands = ConfigPermissionDecision::Allow;
    let repo = temp_root();
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "original tool output\n").unwrap();
    let hook_output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedMCPToolOutput": {
                "replacement": "post hook output"
            },
            "additionalContext": "post context"
        }
    });
    let hook_output_path = repo.join("post-hook-output.json");
    fs::write(
        &hook_output_path,
        serde_json::to_string(&hook_output).unwrap(),
    )
    .unwrap();
    let hook_command = hook_emit_file_command(&hook_output_path);
    config.hooks = coder_config::HookSettings {
        post_tool_use: vec![coder_config::HookMatcherSpec {
            matcher: Some("Read".to_owned()),
            hooks: vec![coder_config::HookCommandSpec::Command {
                command: hook_command.command,
                if_condition: None,
                shell: Some(hook_command.shell),
                timeout: None,
                status_message: None,
                once: false,
                run_async: false,
                async_rewake: false,
            }],
        }],
        ..coder_config::HookSettings::default()
    };
    store.write_run_config_snapshot(&run_id, &config).unwrap();
    let app = router(ApiState::new(store.clone()));

    let response = post_json(
        app,
        "/api/v3/tools/model/execute",
        json!({
            "tool_use_id": "toolu-hook-updated-output",
            "tool_name": "repo_read_file",
            "harness_id": "native-code-edit",
            "input": {
                "repo_root": repo,
                "path": "README.md",
                "run_id": "run-model-tool-hook-updated-output"
            }
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("post hook output"));
    assert!(!body["content"]
        .as_str()
        .unwrap()
        .contains("original tool output"));
    assert_eq!(body["payload"]["hook_updated_output"], true);
    assert_eq!(body["payload"]["output"]["replacement"], "post hook output");
    assert!(body["payload"]["original_payload"]
        .to_string()
        .contains("original tool output"));
    assert_eq!(body["refs"][0]["label"], "repo_evidence");
    let phases = body["phases"].as_array().unwrap();
    assert_eq!(phases[3]["phase"], "post_tool_use_hooks");
    assert_eq!(phases[3]["status"], "completed");
    assert_eq!(phases[3]["updated_tool_output_applied"], true);
    assert_eq!(phases[3]["additional_contexts"][0], "post context");
    assert_eq!(
        phases[3]["hook_results"][0]["updated_tool_output"]["replacement"],
        "post hook output"
    );
    let _ = fs::remove_dir_all(repo);
    let _ = fs::remove_dir_all(store_root);
}
