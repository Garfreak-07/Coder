param(
  [string]$ServerUrl = "",
  [string]$SessionApiKeyEnv = "",
  [ValidateSet("legacy-sdk", "default")]
  [string]$ApiProfile = "legacy-sdk",
  [string]$WorkRoot = ".tmp\openhands-live-smoke",
  [string]$Store = ".tmp\openhands-live-smoke\store",
  [string]$HostName = "127.0.0.1",
  [int]$Port = 8878,
  [switch]$SkipIfMissingOpenHands
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location -LiteralPath $repoRoot

function Write-Skip {
  param([string]$Reason)
  [pscustomobject]@{
    status = "skipped"
    reason = $Reason
  } | ConvertTo-Json -Depth 6
}

function Stop-Or-Skip {
  param([string]$Reason)
  if ($SkipIfMissingOpenHands) {
    Write-Skip -Reason $Reason
    exit 0
  }
  throw $Reason
}

function Get-FirstEnvValue {
  param([string[]]$Names)
  foreach ($name in $Names) {
    if ([string]::IsNullOrWhiteSpace($name)) {
      continue
    }
    $value = [Environment]::GetEnvironmentVariable($name, "Process")
    if (-not [string]::IsNullOrWhiteSpace($value)) {
      return [pscustomobject]@{ Name = $name; Value = $value }
    }
  }
  return $null
}

function Resolve-UnderRepo {
  param([string]$PathValue)
  $path = if ([System.IO.Path]::IsPathRooted($PathValue)) {
    $PathValue
  } else {
    Join-Path $repoRoot $PathValue
  }
  $full = [System.IO.Path]::GetFullPath($path)
  $rootFull = [System.IO.Path]::GetFullPath($repoRoot).TrimEnd('\') + '\'
  if (-not $full.StartsWith($rootFull, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Path must stay under repository root: $full"
  }
  return $full
}

function Invoke-Native {
  param(
    [string]$FilePath,
    [string[]]$Arguments
  )
  & $FilePath @Arguments | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Command failed: $FilePath $($Arguments -join ' ')"
  }
}

function Invoke-NativeCapture {
  param(
    [string]$FilePath,
    [string[]]$Arguments
  )
  $previousErrorActionPreference = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try {
    $output = & $FilePath @Arguments 2>&1
    $exitCode = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $previousErrorActionPreference
  }
  [pscustomobject]@{
    ExitCode = $exitCode
    Output = @($output)
  }
}

function Set-Utf8NoBomContent {
  param(
    [string]$LiteralPath,
    [string[]]$Value
  )
  $encoding = New-Object System.Text.UTF8Encoding($false)
  $text = ($Value -join [Environment]::NewLine) + [Environment]::NewLine
  [System.IO.File]::WriteAllText($LiteralPath, $text, $encoding)
}

function Set-Utf8NoBomText {
  param(
    [string]$LiteralPath,
    [string]$Value
  )
  $encoding = New-Object System.Text.UTF8Encoding($false)
  [System.IO.File]::WriteAllText($LiteralPath, $Value, $encoding)
}

$liveSmokeFlag = [Environment]::GetEnvironmentVariable("OPENHANDS_LIVE_SMOKE", "Process")
if ($liveSmokeFlag -ne "1") {
  Stop-Or-Skip -Reason "Set OPENHANDS_LIVE_SMOKE=1 to run the live OpenHands smoke."
}

if ([string]::IsNullOrWhiteSpace($ServerUrl)) {
  $server = Get-FirstEnvValue -Names @("OPENHANDS_AGENT_SERVER_URL", "OPENHANDS_SERVER_URL")
  if ($null -ne $server) {
    $ServerUrl = $server.Value
  }
}
if ([string]::IsNullOrWhiteSpace($ServerUrl)) {
  Stop-Or-Skip -Reason "Set OPENHANDS_AGENT_SERVER_URL or pass -ServerUrl to run the live OpenHands smoke."
}

if ([string]::IsNullOrWhiteSpace($SessionApiKeyEnv)) {
  $sessionKey = Get-FirstEnvValue -Names @("OPENHANDS_SESSION_API_KEY", "SESSION_API_KEY")
  if ($null -ne $sessionKey) {
    $SessionApiKeyEnv = $sessionKey.Name
  }
}

$llmModel = [Environment]::GetEnvironmentVariable("LLM_MODEL", "Process")
if ([string]::IsNullOrWhiteSpace($llmModel)) {
  Stop-Or-Skip -Reason "Set LLM_MODEL to run the live OpenHands smoke."
}
$llmBaseUrl = [Environment]::GetEnvironmentVariable("LLM_BASE_URL", "Process")
if ([string]::IsNullOrWhiteSpace($llmBaseUrl)) {
  Stop-Or-Skip -Reason "Set LLM_BASE_URL to run the live OpenHands smoke."
}
$llmApiKey = [Environment]::GetEnvironmentVariable("LLM_API_KEY", "Process")
if ([string]::IsNullOrWhiteSpace($llmApiKey)) {
  $deepSeekKey = [Environment]::GetEnvironmentVariable("DEEPSEEK_API_KEY", "Process")
  if (-not [string]::IsNullOrWhiteSpace($deepSeekKey)) {
    [Environment]::SetEnvironmentVariable("LLM_API_KEY", $deepSeekKey, "Process")
  } else {
    Stop-Or-Skip -Reason "Set LLM_API_KEY or DEEPSEEK_API_KEY to run the live OpenHands smoke."
  }
}
$llmModelYaml = $llmModel.Replace("\", "\\").Replace('"', '\"')

$workRootPath = Resolve-UnderRepo -PathValue $WorkRoot
$storePath = Resolve-UnderRepo -PathValue $Store
$repoPath = Join-Path $workRootPath "repo"
$configPath = Join-Path $workRootPath "coder-openhands-smoke.yaml"

if (Test-Path -LiteralPath $workRootPath) {
  $resolvedWorkRoot = [System.IO.Path]::GetFullPath($workRootPath)
  $rootFull = [System.IO.Path]::GetFullPath($repoRoot).TrimEnd('\') + '\'
  if (-not $resolvedWorkRoot.StartsWith($rootFull, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to remove path outside repository root: $resolvedWorkRoot"
  }
  Remove-Item -LiteralPath $workRootPath -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $repoPath | Out-Null
New-Item -ItemType Directory -Force -Path $storePath | Out-Null

Set-Utf8NoBomContent -LiteralPath (Join-Path $repoPath "README.md") -Value @(
  "# OpenHands live smoke"
  ""
  "This temporary repository is used by Coder's opt-in live OpenHands smoke."
  "The task is documentation-only and must not commit or push."
)
$docsPath = Join-Path $repoPath "docs"
New-Item -ItemType Directory -Force -Path $docsPath | Out-Null
$resultDocPath = Join-Path $docsPath "OPENHANDS_LIVE_SMOKE_RESULT.md"
Set-Utf8NoBomContent -LiteralPath $resultDocPath -Value @(
  "# OpenHands Live Smoke Result"
  ""
  "Initial fixture. The live OpenHands smoke must update this file."
)

$git = (Get-Command git).Source
Invoke-Native -FilePath $git -Arguments @("-C", $repoPath, "init")
Invoke-Native -FilePath $git -Arguments @("-C", $repoPath, "config", "user.email", "coder-smoke@example.invalid")
Invoke-Native -FilePath $git -Arguments @("-C", $repoPath, "config", "user.name", "Coder Smoke")
Invoke-Native -FilePath $git -Arguments @("-C", $repoPath, "add", "README.md", "docs/OPENHANDS_LIVE_SMOKE_RESULT.md")
Invoke-Native -FilePath $git -Arguments @("-C", $repoPath, "commit", "-m", "initial smoke fixture")

$sessionLine = if ([string]::IsNullOrWhiteSpace($SessionApiKeyEnv)) {
  "      session_api_key_env:"
} else {
  "      session_api_key_env: $SessionApiKeyEnv"
}

if ($ApiProfile -eq "legacy-sdk") {
  $apiProfileYaml = @"
      api_paths:
        api_prefix: "/api"
        events_search_path: "/conversations/{conversation_id}/events/search"
        run_endpoint_path: "/conversations/{conversation_id}/run"
        websocket_path_template: "/sockets/events/{conversation_id}"
        auth_header: x_session_api_key
      run_start_strategy: post_run_endpoint
"@
} else {
  $apiProfileYaml = @"
      api_paths: {}
      run_start_strategy: post_user_event_with_run_true
"@
}

Set-Utf8NoBomText -LiteralPath $configPath -Value @"
version: 1
models:
  default:
    provider: openai-compatible
    model: "$llmModelYaml"
    base_url_env: LLM_BASE_URL
    api_key_env: LLM_API_KEY
agents:
  planner:
    role: planner
    model: default
    system: "Prepare the executor handoff only."
    output_contract: planner_conversation
  executor:
    role: executor
    model: default
    system: "Use OpenHands to inspect README.md. Keep work documentation-only and report evidence."
    output_contract: execution_result
harnesses:
  planner-conversation:
    backend: planner-model
    tools: [repo_search, read_file]
    permissions:
      read_files: allow
      write_files: deny
      run_commands: deny
      network: deny
      secrets: deny
      publish_external: deny
      git_commit: deny
      git_push: deny
      deploy: deny
  openhands-code-edit:
    backend: openhands
    openhands:
      server_url: "$ServerUrl"
$sessionLine
      workspace_mode: local
      prefer_websocket: false
      poll_interval_ms: 1000
      max_event_poll_seconds: 300
      max_events: 100
$apiProfileYaml
    tools: [terminal, file_editor, task_tracker]
    permissions:
      read_files: allow
      write_files: ask
      run_commands: ask
      network: ask
      secrets: ask
      publish_external: deny
      git_commit: deny
      git_push: deny
      deploy: deny
    verification:
      require_evidence: true
workflows:
  planner-led:
    name: OpenHands live smoke
    max_rounds: 1
    nodes:
      - id: planner
        agent: planner
        harness: planner-conversation
      - id: executor
        agent: executor
        harness: openhands-code-edit
    edges:
      - from: planner
        to: executor
        on: ready
    stop:
      on_status: [completed, blocked, failed]
      final_report_agent: planner
"@

$processEnv = [Environment]::GetEnvironmentVariables("Process")
if ($processEnv.Contains("Path") -and $processEnv.Contains("PATH")) {
  [Environment]::SetEnvironmentVariable("PATH", $null, "Process")
}
[Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", (Join-Path $repoRoot ".tmp\cargo-target"), "Process")

$cargo = (Get-Command cargo).Source
$doctorArgs = @("run", "-p", "coder-cli", "--bin", "coder-rust", "--", "openhands", "doctor", "--server", $ServerUrl)
if (-not [string]::IsNullOrWhiteSpace($SessionApiKeyEnv)) {
  $doctorArgs += @("--session-api-key-env", $SessionApiKeyEnv)
}
& $cargo @doctorArgs | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw "OpenHands doctor failed for the configured server."
}

$task = "Live OpenHands smoke: update docs/OPENHANDS_LIVE_SMOKE_RESULT.md in repo root '$repoPath' with a short dated smoke result. Keep the task documentation-only. Run the harmless verification command 'git status --short'. Do not commit, push, or publish. Return a concise final summary with evidence."
$runArgs = @(
  "run", "-p", "coder-cli", "--bin", "coder-rust", "--",
  "workflow", "run",
  "--config", $configPath,
  "--store", $storePath,
  "--repo", $repoPath,
  "planner-led",
  $task
)
$runResult = Invoke-NativeCapture -FilePath $cargo -Arguments $runArgs
$runOutput = @($runResult.Output)
if ($runResult.ExitCode -ne 0) {
  $text = ($runOutput | ForEach-Object { $_.ToString() }) -join "`n"
  throw "OpenHands workflow run failed.`n$text"
}

$runIdLine = $runOutput | ForEach-Object { $_.ToString() } | Where-Object { $_ -match "^run_id=" } | Select-Object -First 1
if ([string]::IsNullOrWhiteSpace($runIdLine)) {
  throw "OpenHands workflow run did not print run_id."
}
$runId = $runIdLine.Substring("run_id=".Length).Trim()
$eventsPath = Join-Path $storePath "runs\$runId\events.jsonl"
$reportPath = Join-Path $storePath "runs\$runId\artifacts\final-report.json"
if (-not (Test-Path -LiteralPath $eventsPath)) {
  throw "Run events were not written: $eventsPath"
}
if (-not (Test-Path -LiteralPath $reportPath)) {
  throw "Final report was not written: $reportPath"
}

$events = @(Get-Content -LiteralPath $eventsPath | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | ForEach-Object { $_ | ConvertFrom-Json })
$reactKinds = @(
  "executor.reasoning_summary",
  "executor.action_selected",
  "tool.started",
  "tool.completed",
  "observation.recorded",
  "executor.next_step",
  "executor.completed",
  "executor.blocked",
  "executor.failed",
  "command.previewed",
  "command.completed",
  "command.failed",
  "patch.applied"
)
$reactEvents = @($events | Where-Object { $reactKinds -contains $_.kind })
if ($reactEvents.Count -lt 1) {
  throw "OpenHands run did not emit public ReAct timeline events."
}
$rawOpenHandsEvents = @($events | Where-Object { $_.kind -like "backend.openhands.*" })
if ($rawOpenHandsEvents.Count -lt 1) {
  throw "OpenHands run did not persist raw OpenHands event refs."
}

$report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
if ([string]::IsNullOrWhiteSpace($report.summary)) {
  throw "Final report did not include a summary."
}
if ($report.status -ne "completed") {
  throw "Final report status was '$($report.status)', expected 'completed'."
}

if (-not (Test-Path -LiteralPath $resultDocPath)) {
  throw "OpenHands did not leave docs/OPENHANDS_LIVE_SMOKE_RESULT.md in the temporary repo."
}
$changed = @(git -C $repoPath status --porcelain)
$resultDocChanged = @($changed | Where-Object {
  $_ -match "docs/OPENHANDS_LIVE_SMOKE_RESULT\.md$"
})
if ($resultDocChanged.Count -lt 1) {
  throw "OpenHands did not update docs/OPENHANDS_LIVE_SMOKE_RESULT.md."
}

$backendSelected = @($events | Where-Object {
  $_.kind -eq "backend.selected" -and $_.payload.backend -eq "openhands"
})
if ($backendSelected.Count -lt 1) {
  throw "OpenHands run did not record backend.selected with backend=openhands."
}
$runStarted = @($events | Where-Object { $_.kind -eq "run.started" })
if ($runStarted.Count -lt 1) {
  throw "OpenHands run did not record run.started."
}

function Get-HttpErrorStatus {
  param([object]$ErrorRecord)
  $response = $ErrorRecord.Exception.Response
  if ($null -eq $response) {
    return $null
  }
  try {
    return [int]$response.StatusCode
  } catch {
    try {
      return [int]$response.StatusCode.value__
    } catch {
      return $null
    }
  }
}

function Get-HttpErrorText {
  param([object]$ErrorRecord)
  $response = $ErrorRecord.Exception.Response
  if ($null -eq $response) {
    return $ErrorRecord.Exception.Message
  }
  try {
    $stream = $response.GetResponseStream()
    if ($null -ne $stream) {
      $reader = [System.IO.StreamReader]::new($stream)
      return $reader.ReadToEnd()
    }
  } catch {
  }
  return $ErrorRecord.Exception.Message
}

function Assert-NoSecretLeak {
  param(
    [string]$Text,
    [string[]]$SecretNames
  )
  $seen = @{}
  foreach ($name in $SecretNames) {
    if ([string]::IsNullOrWhiteSpace($name)) {
      continue
    }
    $value = [Environment]::GetEnvironmentVariable($name, "Process")
    if ([string]::IsNullOrWhiteSpace($value) -or $value.Length -lt 8) {
      continue
    }
    if ($seen.ContainsKey($value)) {
      continue
    }
    $seen[$value] = $true
    if ($Text.Contains($value)) {
      throw "Secret value from $name appeared in live smoke artifacts."
    }
  }
}

$reviewChangesCount = 0
$undoStatus = "not_run"
$timelineItemCount = 0
$timelineBackendCount = 0
$timelineReactCount = 0
$reportPreviewStatus = ""
$outLog = Join-Path $workRootPath "server.out.log"
$errLog = Join-Path $workRootPath "server.err.log"
$server = Start-Process -FilePath $cargo `
  -ArgumentList @("run", "-p", "coder-cli", "--bin", "coder-rust", "--", "server", "--host", $HostName, "--port", "$Port", "--store", $storePath) `
  -WorkingDirectory $repoRoot `
  -RedirectStandardOutput $outLog `
  -RedirectStandardError $errLog `
  -WindowStyle Hidden `
  -PassThru
try {
  $base = "http://${HostName}:${Port}"
  $health = $null
  foreach ($attempt in 1..90) {
    try {
      $health = Invoke-RestMethod -Method Get -Uri "$base/api/v3/health"
      break
    } catch {
      Start-Sleep -Milliseconds 500
    }
  }
  if ($null -eq $health -or $health.status -ne "ok") {
    throw "Rust v3 health check failed while checking timeline and Review Changes. See $errLog"
  }

  $timeline = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$runId/timeline"
  $timelineItems = @($timeline.items)
  $timelineItemCount = $timelineItems.Count
  if ($timelineItemCount -lt 1) {
    throw "Run timeline was empty."
  }
  $timelineBackendCount = @($timelineItems | Where-Object {
    $_.type -eq "executor_step" -and $_.title -eq "Executor backend: OpenHands"
  }).Count
  if ($timelineBackendCount -lt 1) {
    throw "Run timeline did not show Executor backend: OpenHands."
  }
  $timelineReactCount = @($timelineItems | Where-Object {
    $_.type -in @("reasoning_summary", "tool_call", "command_execution", "file_change", "verification") -or
      ($_.type -eq "executor_step" -and $_.title -match "Action selected|Observation recorded|Next step|Executor completed|Executor blocked|Executor failed")
  }).Count
  if ($timelineReactCount -lt 1) {
    throw "Run timeline did not include public ReAct items."
  }

  $reportPreview = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$runId/report/preview"
  $reportPreviewStatus = $reportPreview.report.status
  if ([string]::IsNullOrWhiteSpace($reportPreview.report.summary)) {
    throw "Report preview did not include a readable summary."
  }

  $review = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$runId/changes"
  $reviewChanges = @($review.changes)
  $reviewChangesCount = $reviewChanges.Count
  if ($reviewChangesCount -lt 1) {
    throw "OpenHands updated the result doc, but Review Changes returned no changes."
  }
  $changeSetId = $reviewChanges[0].change_set_id
  try {
    $undo = Invoke-RestMethod -Method Post -Uri "$base/api/v3/runs/$runId/changes/$changeSetId/undo"
    $undoStatus = $undo.status
    if ($undoStatus -ne "undone") {
      throw "Undo returned unexpected status: $undoStatus"
    }
  } catch {
    $statusCode = Get-HttpErrorStatus -ErrorRecord $_
    $errorText = Get-HttpErrorText -ErrorRecord $_
    if ($statusCode -in @(400, 409) -and $errorText -match "conflict|unsupported|refus") {
      $undoStatus = "safe_${statusCode}"
    } else {
      throw
    }
  }

  $artifactText = @(
    Get-Content -LiteralPath $eventsPath -Raw
    Get-Content -LiteralPath $reportPath -Raw
    ($timeline | ConvertTo-Json -Depth 20)
    ($reportPreview | ConvertTo-Json -Depth 20)
    ($review | ConvertTo-Json -Depth 20)
  ) -join "`n"
  Assert-NoSecretLeak -Text $artifactText -SecretNames @(
    $SessionApiKeyEnv,
    "OPENHANDS_SESSION_API_KEY",
    "SESSION_API_KEY",
    "LLM_API_KEY",
    "DEEPSEEK_API_KEY",
    "OPENAI_API_KEY"
  )
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
}

[pscustomobject]@{
  status = "ok"
  server_url = $ServerUrl
  api_profile = $ApiProfile
  run_id = $runId
  events = $events.Count
  backend_selected = $backendSelected.Count
  timeline_items = $timelineItemCount
  timeline_backend_items = $timelineBackendCount
  timeline_react_items = $timelineReactCount
  react_events = $reactEvents.Count
  raw_openhands_events = $rawOpenHandsEvents.Count
  report_preview_status = $reportPreviewStatus
  final_summary = $report.summary
  files_changed = $changed.Count
  result_doc_changed = $resultDocChanged.Count
  review_changes = $reviewChangesCount
  undo_status = $undoStatus
  secrets_check = "passed"
  result_doc = $resultDocPath
  repo = $repoPath
  store = $storePath
} | ConvertTo-Json -Depth 10
