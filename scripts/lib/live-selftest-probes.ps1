# Dot-sourced by live-coder-selftest-suite.ps1 after runtime context is initialized.

  function Invoke-ModelTool {
    param(
      [string]$ToolUseId,
      [string]$ToolName,
      [string]$RunId,
      [string]$HarnessId,
      [hashtable]$ToolInput,
      [string]$AgentId = "live-selftest"
    )
    Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/tools/model/execute" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      tool_use_id = $ToolUseId
      tool_name = $ToolName
      run_id = $RunId
      harness_id = $HarnessId
      agent_id = $AgentId
      input = $ToolInput
    })
  }

  function Invoke-RuntimeBoundaryProbe {
    param(
      [string]$RunId,
      [string]$RepoPath
    )

    $readResponse = Invoke-ModelTool -ToolUseId "toolu-selftest-hook-read" -ToolName "repo_read_file" -RunId $RunId -HarnessId "native-code-edit" -ToolInput @{
      repo_root = $RepoPath
      path = "README.md"
      run_id = $RunId
    }
    Assert-SelfTest ($readResponse.status -eq "completed") "Runtime probe: repo_read_file failed through model-tool bridge."
    $preHookPhase = @($readResponse.phases | Where-Object { $_.phase -eq "pre_tool_use_hooks" } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $preHookPhase) "Runtime probe: missing pre_tool_use_hooks phase."
    Assert-SelfTest ($preHookPhase.status -eq "completed") "Runtime probe: pre hook phase did not complete."
    Assert-SelfTest ($preHookPhase.matched_hook_count -ge 1) "Runtime probe: configured Read hook was not matched."
    Assert-SelfTest ($preHookPhase.executed_hook_count -ge 1) "Runtime probe: configured Read hook was not executed."

    $backgroundCommand = Invoke-ModelTool -ToolUseId "toolu-selftest-command-background" -ToolName "command_run" -RunId $RunId -HarnessId "native-code-edit" -ToolInput @{
      repo_root = $RepoPath
      cwd = "."
      argv = @("powershell", "-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 2; Write-Output live-background-command-complete")
      foreground_timeout_seconds = 1
      background_on_timeout = $true
      timeout_seconds = 10
      max_output_bytes = 65536
      run_id = $RunId
    }
    Assert-SelfTest ($backgroundCommand.status -in @("completed", "backgrounded")) "Runtime probe: command_run model-tool call failed."
    Assert-SelfTest ($backgroundCommand.payload.result.status -eq "backgrounded") "Runtime probe: command_run did not hand off to background."
    $backgroundCommandTaskId = $backgroundCommand.payload.background_task.task_id
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($backgroundCommandTaskId)) "Runtime probe: background command task id missing."

    $backgroundCommandOutput = Invoke-ModelTool -ToolUseId "toolu-selftest-command-output" -ToolName "read_command_output" -RunId $RunId -HarnessId "native-code-edit" -ToolInput @{
      task_id = $backgroundCommandTaskId
      block = $true
      timeout_ms = 15000
      run_id = $RunId
    }
    Assert-SelfTest ($backgroundCommandOutput.status -eq "completed") "Runtime probe: read_command_output call failed."
    Assert-SelfTest ($backgroundCommandOutput.payload.retrieval_status -eq "success") "Runtime probe: background command output did not reach success."
    Assert-SelfTest ((Get-JsonText $backgroundCommandOutput.payload).Contains("live-background-command-complete")) "Runtime probe: background command output missing expected text."

    $longCommand = Invoke-ModelTool -ToolUseId "toolu-selftest-command-long" -ToolName "command_background" -RunId $RunId -HarnessId "native-code-edit" -ToolInput @{
      repo_root = $RepoPath
      cwd = "."
      argv = @("powershell", "-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 30; Write-Output should-have-been-stopped")
      timeout_seconds = 60
      max_output_bytes = 65536
      run_id = $RunId
    }
    $longCommandTaskId = $longCommand.payload.task_id
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($longCommandTaskId)) "Runtime probe: long background command task id missing."
    $stopCommand = Invoke-ModelTool -ToolUseId "toolu-selftest-command-cancel" -ToolName "cancel_command_background" -RunId $RunId -HarnessId "native-code-edit" -ToolInput @{
      task_id = $longCommandTaskId
      run_id = $RunId
    }
    Assert-SelfTest ($stopCommand.status -in @("completed", "cancelled")) "Runtime probe: cancel_command_background call failed."
    Assert-SelfTest ($stopCommand.payload.cancelled -eq $true) "Runtime probe: command cancellation failed."

    $subagentStart = Invoke-ModelTool -ToolUseId "toolu-selftest-subagent-start" -ToolName "agent_subagent" -RunId $RunId -HarnessId "native-code-edit" -AgentId "selftest-parent" -ToolInput @{
      repo_root = $RepoPath
      task = "Read README.md and report a one-line status for the live runtime boundary probe."
      run_in_background = $true
      workflow_id = "planner-led"
      node_id = "runtime-boundary"
      parent_harness_id = "native-code-edit"
      parent_agent_id = "selftest-parent"
      subagent_name = "runtime-boundary-probe"
      run_id = $RunId
    }
    $subagentTask = $subagentStart.payload.background_task
    Assert-SelfTest ($null -ne $subagentTask) "Runtime probe: background subagent task metadata missing."
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($subagentTask.task_id)) "Runtime probe: background subagent task id missing."
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($subagentTask.transcript_ref)) "Runtime probe: background subagent transcript ref missing."

    $subagentStatus = Invoke-ModelTool -ToolUseId "toolu-selftest-subagent-status" -ToolName "read_subagent_status" -RunId $RunId -HarnessId "native-code-edit" -AgentId "selftest-parent" -ToolInput @{
      task_id = $subagentTask.task_id
      block = $true
      timeout_ms = 30000
      run_id = $RunId
    }
    Assert-SelfTest ($subagentStatus.status -eq "completed") "Runtime probe: read_subagent_status failed."
    Assert-SelfTest ($subagentStatus.payload.retrieval_status -eq "success") "Runtime probe: background subagent did not reach terminal status."
    Assert-SelfTest ($subagentStatus.payload.status -in @("completed", "ready", "blocked")) "Runtime probe: unexpected background subagent terminal status: $($subagentStatus.payload.status)"
    Assert-SelfTest ($subagentStatus.payload.event_count -ge 1) "Runtime probe: background subagent reported no sidechain events."

    $subagentCancelStart = Invoke-ModelTool -ToolUseId "toolu-selftest-subagent-cancel-start" -ToolName "agent_subagent" -RunId $RunId -HarnessId "native-code-edit" -AgentId "selftest-parent" -ToolInput @{
      repo_root = $RepoPath
      task = "Runtime boundary cancellation probe. Start a long-running child agent task and wait for further instruction."
      run_in_background = $true
      workflow_id = "planner-led"
      node_id = "runtime-boundary-cancel"
      parent_harness_id = "native-code-edit"
      parent_agent_id = "selftest-parent"
      subagent_name = "runtime-boundary-cancel-probe"
      run_id = $RunId
    }
    $subagentCancelTask = $subagentCancelStart.payload.background_task
    Assert-SelfTest ($null -ne $subagentCancelTask) "Runtime probe: cancellable background subagent task metadata missing."
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($subagentCancelTask.task_id)) "Runtime probe: cancellable background subagent task id missing."
    $subagentCancel = Invoke-ModelTool -ToolUseId "toolu-selftest-subagent-cancel" -ToolName "cancel_subagent_background" -RunId $RunId -HarnessId "native-code-edit" -AgentId "selftest-parent" -ToolInput @{
      task_id = $subagentCancelTask.task_id
      run_id = $RunId
    }
    Assert-SelfTest ($subagentCancel.status -in @("completed", "cancelled")) "Runtime probe: cancel_subagent_background failed."
    Assert-SelfTest ($subagentCancel.payload.cancelled -eq $true) "Runtime probe: subagent cancellation failed."
    Assert-SelfTest ($subagentCancel.payload.status -eq "cancelled") "Runtime probe: cancelled background subagent did not report cancelled status."

    $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/events?tail=true&limit=1000"
    $eventItems = @($events.events)
    Assert-SelfTest (@($eventItems | Where-Object { $_.kind -eq "model_tool.phase" -and $_.payload.phase -eq "pre_tool_use_hooks" }).Count -ge 1) "Runtime probe: model_tool.phase pre-hook event missing."
    Assert-SelfTest (@($eventItems | Where-Object { $_.kind -eq "command.started" }).Count -ge 1) "Runtime probe: command.started event missing."

    $serializedProbe = Get-JsonText -Value @($readResponse, $backgroundCommand, $backgroundCommandOutput, $longCommand, $stopCommand, $subagentStart, $subagentStatus, $subagentCancelStart, $subagentCancel, $events)
    Assert-NoSecretLeak -Text $serializedProbe -Secrets @($apiKey)

    [pscustomobject]@{
      hook_phase = "passed"
      background_command = "passed"
      cancellation = "passed"
      background_subagent = "passed"
      background_subagent_cancel = "passed"
      command_task_id = $backgroundCommandTaskId
      subagent_task_id = $subagentTask.task_id
      cancelled_subagent_task_id = $subagentCancelTask.task_id
      subagent_terminal_status = $subagentStatus.payload.status
      event_tail_count = $eventItems.Count
    }
  }

  function Invoke-LiveModelHookProbe {
    param(
      [string]$RunId,
      [string]$RepoPath
    )

    $response = Invoke-ModelTool -ToolUseId "toolu-selftest-live-model-hooks" -ToolName "read_file" -RunId $RunId -HarnessId "native-code-edit" -ToolInput @{
      repo_root = $RepoPath
      path = "README.md"
      run_id = $RunId
    }
    Assert-SelfTest ($response.status -eq "completed") "Live model hook probe: aliased repo read did not complete."

    $preHookPhase = @($response.phases | Where-Object { $_.phase -eq "pre_tool_use_hooks" } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $preHookPhase) "Live model hook probe: missing pre_tool_use_hooks phase."
    Assert-SelfTest ($preHookPhase.status -eq "completed") "Live model hook probe: pre hook phase did not complete."
    Assert-SelfTest ($preHookPhase.hook_config_source -eq "run_config_snapshot") "Live model hook probe: hooks did not resolve from the run config snapshot."
    Assert-SelfTest ($preHookPhase.prompt_hook_count -eq 1) "Live model hook probe: expected exactly one prompt hook."
    Assert-SelfTest ($preHookPhase.agent_hook_count -eq 1) "Live model hook probe: expected exactly one agent hook."

    $promptResult = @($preHookPhase.hook_results | Where-Object { $_.type -eq "prompt" } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $promptResult) "Live model hook probe: prompt hook result missing."
    Assert-SelfTest ($promptResult.outcome -eq "success") "Live model hook probe: prompt hook outcome was $($promptResult.outcome)."
    Assert-SelfTest ($promptResult.provider -eq $normalizedProvider) "Live model hook probe: prompt hook provider mismatch."
    Assert-SelfTest ($promptResult.model -eq $Model) "Live model hook probe: prompt hook model mismatch."
    Assert-SelfTest ($promptResult.hook_json_output.ok -eq $true) "Live model hook probe: prompt hook did not return ok=true."

    $agentResult = @($preHookPhase.hook_results | Where-Object { $_.type -eq "agent" } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $agentResult) "Live model hook probe: agent hook result missing."
    Assert-SelfTest ($agentResult.outcome -eq "success") "Live model hook probe: agent hook outcome was $($agentResult.outcome)."
    Assert-SelfTest ($agentResult.provider -eq $normalizedProvider) "Live model hook probe: agent hook provider mismatch."
    Assert-SelfTest ($agentResult.model -eq $Model) "Live model hook probe: agent hook model mismatch."
    Assert-SelfTest ($agentResult.hook_output_kind -eq "agent_structured_output_tool") "Live model hook probe: agent hook did not use StructuredOutput."
    Assert-SelfTest ($agentResult.hook_json_output.ok -eq $true) "Live model hook probe: agent hook did not return ok=true."
    Assert-SelfTest ([string]$agentResult.hook_agent_id -like "hook-agent-*") "Live model hook probe: isolated hook agent id missing."

    $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/events?tail=true&limit=1000"
    $eventItems = @($events.events)
    $phaseEvent = @($eventItems | Where-Object {
      $_.kind -eq "model_tool.phase" -and
      $_.payload.tool_use_id -eq "toolu-selftest-live-model-hooks" -and
      $_.payload.phase -eq "pre_tool_use_hooks"
    } | Select-Object -Last 1)
    Assert-SelfTest ($null -ne $phaseEvent) "Live model hook probe: durable pre-hook phase event missing."
    Assert-SelfTest ($phaseEvent.payload.prompt_hook_count -eq 1) "Live model hook probe: durable prompt hook count mismatch."
    Assert-SelfTest ($phaseEvent.payload.agent_hook_count -eq 1) "Live model hook probe: durable agent hook count mismatch."

    $serializedProbe = Get-JsonText -Value @($response, $events)
    Assert-NoSecretLeak -Text $serializedProbe -Secrets @($apiKey)

    [pscustomobject]@{
      status = "passed"
      run_id = $RunId
      provider = $normalizedProvider
      model = $Model
      prompt_hook = $promptResult.outcome
      agent_hook = $agentResult.outcome
      agent_hook_output_kind = $agentResult.hook_output_kind
      hook_agent_id = $agentResult.hook_agent_id
      event_tail_count = $eventItems.Count
    }
  }

  function Invoke-LiveAsyncRewakeProbe {
    param(
      [string]$RunId
    )

    $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/events?tail=true&limit=1000"
    $eventItems = @($events.events)
    $notification = @($eventItems | Where-Object {
      $_.kind -eq "model_tool.async_rewake.notification" -and
      ([string]$_.payload.message).Contains("live-async-rewake-hook")
    } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $notification) "Live async rewake probe: notification event missing."
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace([string]$notification.payload.tool_use_id)) "Live async rewake probe: notification tool_use_id missing."

    $toolUseId = [string]$notification.payload.tool_use_id
    $phaseEvent = @($eventItems | Where-Object {
      $_.kind -eq "model_tool.phase" -and
      $_.payload.phase -eq "pre_tool_use_hooks" -and
      $_.payload.tool_use_id -eq $toolUseId
    } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $phaseEvent) "Live async rewake probe: pre-hook phase event missing."
    $rewakeHook = @($phaseEvent.payload.hook_results | Where-Object { $_.async_rewake -eq $true } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $rewakeHook) "Live async rewake probe: async rewake hook result missing."
    Assert-SelfTest ($rewakeHook.outcome -eq "backgrounded") "Live async rewake probe: hook was not backgrounded."
    Assert-SelfTest ($rewakeHook.rewake_supported -eq $true) "Live async rewake probe: hook metadata did not report rewake support."

    $delivery = @($eventItems | Where-Object {
      $_.kind -eq "model_tool.async_rewake.delivered" -and
      $_.payload.tool_use_id -eq $toolUseId -and
      $_.payload.delivery_channel -eq "model_tool_turn_attachment"
    } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $delivery) "Live async rewake probe: notification was not delivered to a model-tool turn."
    $attachmentDelivery = @($eventItems | Where-Object {
      $_.kind -eq "model.tool_turn.attachments_delivered" -and
      @($_.payload.attachment_types) -contains "queued_command"
    } | Select-Object -First 1)
    Assert-SelfTest ($null -ne $attachmentDelivery) "Live async rewake probe: queued command attachment was not delivered to the provider loop."

    $serializedProbe = Get-JsonText -Value $events
    Assert-NoSecretLeak -Text $serializedProbe -Secrets @($apiKey)

    [pscustomobject]@{
      status = "passed"
      run_id = $RunId
      tool_use_id = $toolUseId
      async_hook_id = $notification.payload.async_hook_id
      delivery_channel = $delivery.payload.delivery_channel
      attachment_type = "queued_command"
      event_tail_count = $eventItems.Count
    }
  }

  function Invoke-VerificationRepairProbe {
    param(
      [string]$RunId
    )

    $failed = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/runs/$RunId/verification/evidence" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      status = "failed"
      source = "live-selftest-verification-repair-probe"
      summary = "intentional verification failure before repair"
      reason = "intentional verification repair probe failure"
      remaining_work = @("record repaired verification evidence")
      evidence = @{
        synthetic = $true
        phase = "before_repair"
      }
    })
    Assert-SelfTest ($failed.status -eq "failed") "Verification repair probe: failed evidence was not recorded."
    Assert-SelfTest ($failed.report.status -eq "failed") "Verification repair probe: failed evidence did not make preview fail."

    $completed = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/runs/$RunId/verification/evidence" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      status = "completed"
      source = "live-selftest-verification-repair-probe"
      summary = "intentional verification repair completed"
      reason = ""
      remaining_work = @()
      evidence = @{
        synthetic = $true
        phase = "after_repair"
      }
    })
    Assert-SelfTest ($completed.status -eq "completed") "Verification repair probe: completed evidence was not recorded."
    Assert-SelfTest ($completed.report.status -eq "completed") "Verification repair probe: completed evidence did not clear repaired failure."
    Assert-SelfTest (@($completed.report.blockers).Count -eq 0) "Verification repair probe: repaired report retained blockers."
    Assert-SelfTest (@($completed.report.checks | Where-Object { $_.Contains("intentional verification repair probe failure") }).Count -ge 1) "Verification repair probe: repaired report dropped failed-check history."
    Assert-SelfTest (@($completed.report.checks | Where-Object { $_.Contains("intentional verification repair completed") }).Count -ge 1) "Verification repair probe: repaired report did not include completed verification."

    $preview = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/report/preview"
    Assert-SelfTest ($preview.report.status -eq "completed") "Verification repair probe: report preview did not remain completed after repair."

    $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/events?tail=true&limit=1000"
    $eventItems = @($events.events)
    Assert-SelfTest (@($eventItems | Where-Object { $_.kind -eq "verification.failed" -and $_.payload.source -eq "live-selftest-verification-repair-probe" }).Count -ge 1) "Verification repair probe: verification.failed event missing."
    Assert-SelfTest (@($eventItems | Where-Object { $_.kind -eq "verification.completed" -and $_.payload.source -eq "live-selftest-verification-repair-probe" }).Count -ge 1) "Verification repair probe: verification.completed event missing."

    $serializedRepair = Get-JsonText -Value @($failed, $completed, $preview, $events)
    Assert-NoSecretLeak -Text $serializedRepair -Secrets @($apiKey)

    [pscustomobject]@{
      status = "passed"
      run_id = $RunId
      failed_status = $failed.report.status
      repaired_status = $completed.report.status
      preview_status = $preview.report.status
      retained_failed_check = $true
      event_tail_count = $eventItems.Count
    }
  }

  function Invoke-TranscriptCompactionProbe {
    param(
      [string]$RunId
    )

    $scopeId = "live-selftest-transcript-compaction-$RunId"
    $compaction = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/runs/$RunId/transcript/compact" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      custom_instructions = "This is a live Coder self-test. Preserve the task outcome, verification evidence, runtime-boundary probe, and native executor runtime evidence."
      scope_id = $scopeId
      max_events = 200
    }) -Attempts 1
    Assert-SelfTest ($compaction.contract -eq "coder.run_transcript_compaction.v1") "Transcript compaction probe: unexpected contract."
    Assert-SelfTest ($compaction.status -eq "completed") "Transcript compaction probe: status was $($compaction.status)."
    Assert-SelfTest ($compaction.success -eq $true) "Transcript compaction probe: success was not true."
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace([string]$compaction.summary)) "Transcript compaction probe: summary missing."
    Assert-SelfTest ($compaction.summary_estimated_tokens -gt 0) "Transcript compaction probe: summary token estimate missing."
    Assert-SelfTest ($compaction.transcript_event_count -ge 1) "Transcript compaction probe: transcript event count missing."
    Assert-SelfTest ($compaction.transcript_events_included -ge 1) "Transcript compaction probe: no transcript events included."
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace([string]$compaction.artifact_ref)) "Transcript compaction probe: artifact ref missing."
    Assert-SelfTest ($compaction.event_sequence -gt 0) "Transcript compaction probe: event sequence missing."
    Assert-SelfTest ($compaction.circuit.scope_id -eq $scopeId) "Transcript compaction probe: circuit scope mismatch."
    Assert-SelfTest ($compaction.circuit.circuit_breaker_open -eq $false) "Transcript compaction probe: circuit unexpectedly open."

    $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/events?tail=true&limit=1000"
    $eventItems = @($events.events)
    $compactionEvent = @($eventItems | Where-Object {
      $_.kind -eq "run.transcript_compaction.outcome" -and $_.payload.circuit.scope_id -eq $scopeId
    } | Select-Object -Last 1)
    Assert-SelfTest ($null -ne $compactionEvent) "Transcript compaction probe: compaction outcome event missing."
    Assert-SelfTest ($compactionEvent.payload.success -eq $true) "Transcript compaction probe: outcome event did not report success."
    Assert-SelfTest ($compactionEvent.payload.artifact_ref -eq $compaction.artifact_ref) "Transcript compaction probe: event artifact ref mismatch."

    $serializedCompaction = Get-JsonText -Value @($compaction, $events)
    Assert-NoSecretLeak -Text $serializedCompaction -Secrets @($apiKey)

    [pscustomobject]@{
      status = "passed"
      run_id = $RunId
      provider = $compaction.provider
      model = $compaction.model
      endpoint = $compaction.endpoint
      summary_estimated_tokens = $compaction.summary_estimated_tokens
      transcript_events_included = $compaction.transcript_events_included
      transcript_events_omitted = $compaction.transcript_events_omitted
      transcript_truncated = $compaction.transcript_truncated
      artifact_ref = $compaction.artifact_ref
      event_sequence = $compaction.event_sequence
      circuit_scope = $compaction.circuit.scope_id
    }
  }
