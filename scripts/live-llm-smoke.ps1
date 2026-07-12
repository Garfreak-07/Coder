param(
  [string]$HostName = "127.0.0.1",
  [int]$Port = 8877,
  [string]$Store = ".tmp\live-llm-smoke",
  [string]$Provider = "openai-compatible",
  [string]$BaseUrl = "",
  [string]$Model = "",
  [string]$ApiKeyEnv = "",
  [ValidateSet("auto", "direct", "explicit", "environment")]
  [string]$ProviderProxyMode = "auto",
  [string]$ProviderProxyUrl = "",
  [switch]$LoadLocalEnv,
  [switch]$ProviderTestOnly,
  [switch]$PlannerTestOnly,
  [switch]$Live,
  [switch]$SkipIfMissingProvider
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location -LiteralPath $repoRoot

if ($LoadLocalEnv) {
  $localEnvPath = Join-Path $repoRoot ".local-env.ps1"
  if (-not (Test-Path -LiteralPath $localEnvPath)) {
    $reason = "Local env file not found: $localEnvPath"
    if ($SkipIfMissingProvider) {
      [pscustomobject]@{
        status = "skipped"
        reason = $reason
      } | ConvertTo-Json -Depth 4
      exit 0
    }
    throw $reason
  }
  . $localEnvPath
}

$liveSmokeFlag = [Environment]::GetEnvironmentVariable("CODER_LIVE_LLM_SMOKE", "Process")
if ($liveSmokeFlag -ne "1" -and -not $Live) {
  $reason = "Set CODER_LIVE_LLM_SMOKE=1 to run the live provider smoke."
  if ($SkipIfMissingProvider) {
    [pscustomobject]@{
      status = "skipped"
      reason = $reason
    } | ConvertTo-Json -Depth 4
    exit 0
  }
  throw $reason
}

function Get-FirstEnvValue {
  param([string[]]$Names)
  foreach ($name in $Names) {
    if (-not $name) {
      continue
    }
    $value = [Environment]::GetEnvironmentVariable($name, "Process")
    if (-not [string]::IsNullOrWhiteSpace($value)) {
      return [pscustomobject]@{ Name = $name; Value = $value }
    }
  }
  return $null
}

function Assert-ProviderTrace {
  param([object]$Turn, [string]$Label)
  if ($null -eq $Turn.provider_trace) {
    throw "$Label did not include provider_trace."
  }
  if ($Turn.provider_trace.requested_stream -ne $true) {
    throw "$Label did not request streaming provider output."
  }
  if ([string]::IsNullOrWhiteSpace([string]$Turn.provider_trace.response_transport)) {
    throw "$Label did not record provider response transport."
  }
  if ($normalizedProvider -eq "deepseek") {
    if ($Turn.provider_trace.response_transport -ne "event_stream") {
      throw "$Label used response_transport '$($Turn.provider_trace.response_transport)', expected event_stream for DeepSeek."
    }
    if ($Turn.provider_trace.streaming_fallback -eq $true) {
      throw "$Label unexpectedly fell back from streaming for DeepSeek."
    }
  }
}

$normalizedProvider = $Provider.Trim().ToLowerInvariant()
if ([string]::IsNullOrWhiteSpace($normalizedProvider)) {
  $normalizedProvider = "openai-compatible"
}

$apiKeyCandidates = @()
if (-not [string]::IsNullOrWhiteSpace($ApiKeyEnv)) {
  $apiKeyCandidates += $ApiKeyEnv
}
switch ($normalizedProvider) {
  "deepseek" {
    $apiKeyCandidates += @("DEEPSEEK_API_KEY", "LLM_API_KEY")
  }
  "openai" {
    $apiKeyCandidates += @("OPENAI_API_KEY", "LLM_API_KEY")
  }
  default {
    $apiKeyCandidates += @("LLM_API_KEY", "DEEPSEEK_API_KEY", "CODER_API_KEY")
  }
}

$apiKey = Get-FirstEnvValue -Names $apiKeyCandidates
if ($null -eq $apiKey) {
  $reason = "No live provider API key found in: $($apiKeyCandidates -join ', ')"
  if ($SkipIfMissingProvider) {
    [pscustomobject]@{
      status = "skipped"
      reason = $reason
    } | ConvertTo-Json -Depth 4
    exit 0
  }
  throw $reason
}

if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
  $BaseUrl = [Environment]::GetEnvironmentVariable("LLM_BASE_URL", "Process")
}
if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
  if ($normalizedProvider -eq "openai") {
    $BaseUrl = "https://api.openai.com/v1"
  } else {
    $BaseUrl = "https://api.deepseek.com"
  }
}

if ([string]::IsNullOrWhiteSpace($Model)) {
  $Model = [Environment]::GetEnvironmentVariable("LLM_MODEL", "Process")
}
if ([string]::IsNullOrWhiteSpace($Model)) {
  $Model = if ($normalizedProvider -eq "openai") { "gpt-5.5" } else { "deepseek-chat" }
}

$resolvedProviderProxyMode = $ProviderProxyMode.Trim().ToLowerInvariant()
if ($resolvedProviderProxyMode -eq "auto") {
  $resolvedProviderProxyMode = if (-not [string]::IsNullOrWhiteSpace($ProviderProxyUrl)) {
    "explicit"
  } elseif ($normalizedProvider -eq "deepseek" -or $normalizedProvider -eq "ollama") {
    "direct"
  } else {
    "environment"
  }
}
if ($resolvedProviderProxyMode -eq "explicit" -and [string]::IsNullOrWhiteSpace($ProviderProxyUrl)) {
  throw "ProviderProxyMode explicit requires -ProviderProxyUrl."
}

$storePath = if ([System.IO.Path]::IsPathRooted($Store)) {
  $Store
} else {
  Join-Path $repoRoot $Store
}
$outLog = Join-Path $storePath "server.out.log"
$errLog = Join-Path $storePath "server.err.log"
New-Item -ItemType Directory -Force -Path $storePath | Out-Null

# Some sandboxed Windows environments expose both Path and PATH. Start-Process
# rejects duplicate environment keys, so keep Path and drop duplicate PATH.
$processEnv = [Environment]::GetEnvironmentVariables("Process")
if ($processEnv.Contains("Path") -and $processEnv.Contains("PATH")) {
  $preservedPath = [string]$processEnv["Path"]
  if ([string]::IsNullOrWhiteSpace($preservedPath)) { $preservedPath = [string]$processEnv["PATH"] }
  [Environment]::SetEnvironmentVariable("PATH", $null, "Process")
  [Environment]::SetEnvironmentVariable("Path", $preservedPath, "Process")
}

[Environment]::SetEnvironmentVariable("CODER_RUNTIME_CACHE_DIR", (Join-Path $repoRoot "tmp\coder-runtime-cache"), "Process")
[Environment]::SetEnvironmentVariable("LLM_BASE_URL", $BaseUrl, "Process")
[Environment]::SetEnvironmentVariable("LLM_MODEL", $Model, "Process")
[Environment]::SetEnvironmentVariable("LLM_API_KEY", $apiKey.Value, "Process")

$cargo = (Get-Command cargo).Source
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
    throw "Rust v3 health check failed. See $errLog"
  }

  $jsonHeaders = @{ "Content-Type" = "application/json" }
  $baseUrls = @{}
  $baseUrls[$normalizedProvider] = $BaseUrl
  $proxyModes = @{}
  $proxyModes[$normalizedProvider] = $resolvedProviderProxyMode
  $proxyUrls = @{}
  if (-not [string]::IsNullOrWhiteSpace($ProviderProxyUrl)) {
    $proxyUrls[$normalizedProvider] = $ProviderProxyUrl
  }
  $settingsBody = @{
    default_provider = $normalizedProvider
    default_model = $Model
    base_urls = $baseUrls
    proxy_modes = $proxyModes
    proxy_urls = $proxyUrls
    mock_mode = $false
  } | ConvertTo-Json -Depth 20
  $settings = Invoke-RestMethod -Method Post -Uri "$base/api/v3/providers/settings" -Headers $jsonHeaders -Body $settingsBody
  if ($settings.status.default_status.credential_configured -ne $true) {
    throw "Provider settings did not detect configured credentials."
  }

  $providerTestBody = @{
    provider = $normalizedProvider
    mock = $false
  } | ConvertTo-Json -Depth 10
  $providerTest = Invoke-RestMethod -Method Post -Uri "$base/api/v3/providers/test" -Headers $jsonHeaders -Body $providerTestBody
  if ($providerTest.test.ok -ne $true) {
    throw "Live provider test failed: $($providerTest.test.message)"
  }

  if ($ProviderTestOnly) {
    [pscustomobject]@{
      status = "ok"
      provider = $normalizedProvider
      model = $Model
      credential_source = $apiKey.Name
      proxy_mode = $settings.status.default_status.proxy_mode
      proxy_url_configured = -not [string]::IsNullOrWhiteSpace([string]$settings.status.default_status.proxy_url)
      provider_test = $providerTest.test.mode
      external_payload = "synthetic_provider_test_only_no_repo_content"
    } | ConvertTo-Json -Depth 10
    return
  }

  $defaultWorkflow = Invoke-RestMethod -Method Get -Uri "$base/api/v3/workflows/default"
  $config = $defaultWorkflow.config
  $config.models.default.provider = $normalizedProvider
  $config.models.default.model = $Model
  $config.models.default.base_url_env = "LLM_BASE_URL"
  $config.models.default.api_key_env = "LLM_API_KEY"

  $createBody = @{
    repo = "."
    workflow_id = $defaultWorkflow.workflow_id
    planner_agent_id = "planner"
    config = $config
    mode = "discuss"
  } | ConvertTo-Json -Depth 50
  $sessionResponse = Invoke-RestMethod -Method Post -Uri "$base/api/v3/planner-chat/sessions" -Headers $jsonHeaders -Body $createBody
  $sessionId = $sessionResponse.session.session_id
  if ([string]::IsNullOrWhiteSpace($sessionId)) {
    throw "Planner session creation did not return a session_id."
  }

  function Send-PlannerTurn {
    param([string]$Message)
    $body = @{
      message = $Message
      repo = "."
      confirmed = $false
      mode = "discuss"
      planner_agent_id = "planner"
      config = $config
    } | ConvertTo-Json -Depth 50
    Invoke-RestMethod -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/turn" -Headers $jsonHeaders -Body $body
  }

  $firstTurn = Send-PlannerTurn -Message "Plan a read-only qualitative review of README.md. Summarize its current strengths and gaps without modifying files. Do not start work."
  if ([string]::IsNullOrWhiteSpace($firstTurn.assistant_message)) {
    throw "First live Planner turn did not return an assistant message."
  }
  if ($firstTurn.should_start_workflow -eq $true) {
    throw "First Planner chat turn unexpectedly requested workflow start."
  }
  Assert-ProviderTrace -Turn $firstTurn -Label "First Planner chat turn"
  if ([int]$firstTurn.provider_trace.tool_calls -lt 1) {
    throw "First Planner chat turn did not inspect the bound repository."
  }
  if ([int]$firstTurn.provider_trace.tool_result_bytes -lt 1) {
    throw "First Planner chat turn did not return a bounded repository observation."
  }

  $secondTurn = Send-PlannerTurn -Message "Confirm this read-only qualitative review plan."
  if ([string]::IsNullOrWhiteSpace($secondTurn.assistant_message)) {
    throw "Second live Planner turn did not return an assistant message."
  }
  if ($secondTurn.should_start_workflow -eq $true) {
    throw "Second Planner chat turn unexpectedly requested workflow start."
  }
  if ($secondTurn.session.turns.Count -lt 4) {
    throw "Planner session did not retain two user/assistant turns."
  }
  if ($null -ne $secondTurn.provider_trace) {
    Assert-ProviderTrace -Turn $secondTurn -Label "Second Planner chat turn"
  } elseif ($secondTurn.readiness -ne "ready") {
    throw "Second Planner chat turn skipped the provider without confirming a ready plan."
  }
  if ($secondTurn.plan_draft.execution_mode -ne "read_only") {
    throw "Planner did not return execution_mode=read_only."
  }
  if ($secondTurn.plan_draft.review_mode -ne "qualitative") {
    throw "Planner did not return review_mode=qualitative."
  }

  if ($PlannerTestOnly) {
    [pscustomobject]@{
      status = "ok"
      provider = $normalizedProvider
      model = $Model
      credential_source = $apiKey.Name
      provider_test = $providerTest.test.mode
      first_turn_provider_trace = $firstTurn.provider_trace
      second_turn_provider_trace = $secondTurn.provider_trace
      execution_mode = $secondTurn.plan_draft.execution_mode
      review_mode = $secondTurn.plan_draft.review_mode
      turns = $secondTurn.session.turns.Count
      chat_started_run = $false
      api_key_exposed = $false
    } | ConvertTo-Json -Depth 10
    return
  }

  $startBody = @{
    repo = "."
    workflow_id = $defaultWorkflow.workflow_id
    planner_agent_id = "planner"
    config = $config
    scopes = @("README.md")
  } | ConvertTo-Json -Depth 50
  $startWork = Invoke-RestMethod -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/start-work" -Headers $jsonHeaders -Body $startBody
  if ([string]::IsNullOrWhiteSpace($startWork.run_id)) {
    throw "Start Work did not start the confirmed read-only workflow: $($startWork.status)"
  }

  $runEvents = $null
  $terminalEvent = $null
  foreach ($attempt in 1..180) {
    $runEvents = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($startWork.run_id)/events?tail=true&limit=1000"
    $terminalEvent = $runEvents.events | Where-Object { $_.kind -in @("run.completed", "run.blocked", "run.failed", "run.cancelled") } | Select-Object -Last 1
    if ($null -ne $terminalEvent) {
      break
    }
    Start-Sleep -Milliseconds 500
  }
  if ($null -eq $terminalEvent) {
    throw "Start Work did not reach a terminal state within 90 seconds."
  }
  if ($terminalEvent.kind -ne "run.completed") {
    throw "Start Work ended with $($terminalEvent.kind)."
  }
  $nativeStarted = $runEvents.events | Where-Object {
    $_.kind -eq "backend.native_rust.started" -and $_.payload.implementation -eq "native-model-tool-loop"
  } | Select-Object -First 1
  if ($null -eq $nativeStarted) {
    throw "Start Work did not use the native model tool loop."
  }
  $readCompleted = $runEvents.events | Where-Object {
    $_.kind -eq "model.tool_call.completed" -and $_.payload.tool_name -eq "repo_read_file" -and $_.payload.status -eq "completed"
  } | Select-Object -First 1
  if ($null -eq $readCompleted) {
    throw "The native Executor did not complete repo_read_file."
  }
  $workflowPlanner = $runEvents.events | Where-Object {
    $_.kind -eq "planner.workflow_decision" -and $_.payload.implementation -eq "provider-backed-bounded-planner"
  } | Select-Object -First 1
  if ($null -eq $workflowPlanner) {
    throw "The provider-backed Workflow Planner did not record a decision."
  }
  if ($runEvents.events | Where-Object { $_.kind -eq "file.written" }) {
    throw "The read-only live smoke unexpectedly wrote a file."
  }
  $serializedEvents = $runEvents | ConvertTo-Json -Depth 40 -Compress
  if ($serializedEvents.Contains($apiKey.Value)) {
    throw "The live run event payload exposed the provider API key."
  }

  [pscustomobject]@{
    status = "ok"
    provider = $normalizedProvider
    model = $Model
    credential_source = $apiKey.Name
    provider_test = $providerTest.test.mode
    first_turn_provider_trace = $firstTurn.provider_trace
    second_turn_provider_trace = $secondTurn.provider_trace
    execution_mode = $secondTurn.plan_draft.execution_mode
    review_mode = $secondTurn.plan_draft.review_mode
    session_id = $sessionId
    turns = $secondTurn.session.turns.Count
    chat_started_run = $false
    start_work_status = $startWork.status
    run_started = $true
    run_terminal_event = $terminalEvent.kind
    executor_implementation = $nativeStarted.payload.implementation
    workflow_planner_implementation = $workflowPlanner.payload.implementation
    workflow_planner_decision = $workflowPlanner.payload.decision
    repo_read_completed = $true
    file_write_events = 0
    api_key_exposed = $false
  } | ConvertTo-Json -Depth 10
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
}
