param(
  [string]$HostName = "127.0.0.1",
  [int]$Port = 8877,
  [string]$Store = ".tmp\live-llm-smoke",
  [string]$Provider = "openai-compatible",
  [string]$BaseUrl = "",
  [string]$Model = "",
  [string]$ApiKeyEnv = "",
  [switch]$SkipIfMissingProvider
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location -LiteralPath $repoRoot

$liveSmokeFlag = [Environment]::GetEnvironmentVariable("CODER_LIVE_LLM_SMOKE", "Process")
if ($liveSmokeFlag -ne "1") {
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
  $Model = if ($normalizedProvider -eq "openai") { "gpt-5.5" } else { "deepseek-v4-flash" }
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
  [Environment]::SetEnvironmentVariable("PATH", $null, "Process")
}

[Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", (Join-Path $repoRoot ".tmp\cargo-target"), "Process")
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
  $settingsBody = @{
    default_provider = $normalizedProvider
    default_model = $Model
    base_urls = $baseUrls
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

  $defaultWorkflow = Invoke-RestMethod -Method Get -Uri "$base/api/v3/workflows/default"
  $config = $defaultWorkflow.config
  $config.models.default.provider = $normalizedProvider
  $config.models.default.model = $Model
  $config.models.default.base_url_env = "LLM_BASE_URL"
  $config.models.default.api_key_env = "LLM_API_KEY"

  $createBody = @{
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
      confirmed = $false
      mode = "discuss"
      planner_agent_id = "planner"
      config = $config
    } | ConvertTo-Json -Depth 50
    Invoke-RestMethod -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/turn" -Headers $jsonHeaders -Body $body
  }

  $firstTurn = Send-PlannerTurn -Message "Live smoke only: discuss how to inspect README.md. Do not start work."
  if ([string]::IsNullOrWhiteSpace($firstTurn.assistant_message)) {
    throw "First live Planner turn did not return an assistant message."
  }
  if ($firstTurn.should_start_workflow -eq $true) {
    throw "First Planner chat turn unexpectedly requested workflow start."
  }

  $secondTurn = Send-PlannerTurn -Message "Second live smoke turn: keep this conversational and side-effect free."
  if ([string]::IsNullOrWhiteSpace($secondTurn.assistant_message)) {
    throw "Second live Planner turn did not return an assistant message."
  }
  if ($secondTurn.should_start_workflow -eq $true) {
    throw "Second Planner chat turn unexpectedly requested workflow start."
  }
  if ($secondTurn.session.turns.Count -lt 4) {
    throw "Planner session did not retain two user/assistant turns."
  }

  $startBody = @{
    repo = "."
    workflow_id = $defaultWorkflow.workflow_id
    planner_agent_id = "planner"
    config = $config
    scopes = @("README.md")
  } | ConvertTo-Json -Depth 50
  $startWork = Invoke-RestMethod -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/start-work" -Headers $jsonHeaders -Body $startBody
  if ([string]::IsNullOrWhiteSpace($startWork.run_id) -and [string]::IsNullOrWhiteSpace($startWork.assistant_message)) {
    throw "Start Work returned neither a run_id nor a Planner clarification."
  }

  [pscustomobject]@{
    status = "ok"
    provider = $normalizedProvider
    model = $Model
    credential_source = $apiKey.Name
    provider_test = $providerTest.test.mode
    session_id = $sessionId
    turns = $secondTurn.session.turns.Count
    chat_started_run = $false
    start_work_status = $startWork.status
    run_started = -not [string]::IsNullOrWhiteSpace($startWork.run_id)
  } | ConvertTo-Json -Depth 10
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
}
