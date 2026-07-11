param(
  [string]$HostName = "127.0.0.1",
  [int]$Port = 8881,
  [string]$WorkRoot = "F:\ccc",
  [string]$Store = "tmp\live-coder-selftest-suite\store",
  [string]$Provider = "deepseek",
  [string]$BaseUrl = "",
  [string]$Model = "",
  [ValidateSet("auto", "direct", "explicit", "environment")]
  [string]$ProviderProxyMode = "auto",
  [string]$ProviderProxyUrl = "",
  [string]$ApiKeyEnv = "",
  [switch]$Live,
  [switch]$LoadLocalEnv,
  [switch]$Force,
  [switch]$IncludeOpenEndedCases,
  [switch]$SkipIfMissingLiveConfig
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location -LiteralPath $repoRoot

function Stop-Or-Skip {
  param([string]$Reason)
  if ($SkipIfMissingLiveConfig) {
    [pscustomobject]@{ status = "skipped"; reason = $Reason } | ConvertTo-Json -Depth 4
    exit 0
  }
  throw $Reason
}

function Assert-SelfTest {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) { throw $Message }
}

function ConvertTo-JsonBody {
  param([hashtable]$Value)
  $Value | ConvertTo-Json -Depth 100
}

function Invoke-RestJsonWithRetry {
  param(
    [string]$Method,
    [string]$Uri,
    [hashtable]$Headers = @{},
    [string]$Body = $null,
    [int]$Attempts = 3
  )
  $lastError = $null
  foreach ($attempt in 1..$Attempts) {
    try {
      if ($PSBoundParameters.ContainsKey("Body") -and $null -ne $Body) {
        return Invoke-RestMethod -Method $Method -Uri $Uri -Headers $Headers -Body $Body
      }
      return Invoke-RestMethod -Method $Method -Uri $Uri -Headers $Headers
    } catch {
      $lastError = $_
      Start-Sleep -Milliseconds (250 * $attempt)
    }
  }
  throw $lastError
}

function Invoke-Native {
  param([string]$FilePath, [string[]]$Arguments)
  & $FilePath @Arguments | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "$FilePath $($Arguments -join ' ') failed"
  }
}

function Invoke-NativeCapture {
  param([string]$FilePath, [string[]]$Arguments)
  $output = & $FilePath @Arguments 2>&1
  [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = @($output) }
}

function Get-FirstEnvValue {
  param([string[]]$Names)
  foreach ($name in $Names) {
    if ([string]::IsNullOrWhiteSpace($name)) { continue }
    $value = [Environment]::GetEnvironmentVariable($name, "Process")
    if (-not [string]::IsNullOrWhiteSpace($value)) {
      return [pscustomobject]@{ Name = $name; Value = $value }
    }
  }
  $null
}

function Resolve-UnderRepo {
  param([string]$PathValue)
  $path = if ([System.IO.Path]::IsPathRooted($PathValue)) { $PathValue } else { Join-Path $repoRoot $PathValue }
  $fullPath = [System.IO.Path]::GetFullPath($path)
  $repoFullPath = [System.IO.Path]::GetFullPath($repoRoot).TrimEnd('\', '/')
  if (-not $fullPath.StartsWith($repoFullPath, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Path must stay under repository root: $PathValue"
  }
  $fullPath
}

function Initialize-EmptyGitRepo {
  param([string]$Path)
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
  $git = (Get-Command git).Source
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "init")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "config", "core.autocrlf", "false")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "config", "core.safecrlf", "false")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "config", "user.email", "coder-selftest@example.invalid")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "config", "user.name", "Coder Self Test")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "commit", "--allow-empty", "-m", "initial self-test fixture")
}

function Reset-TargetRepo {
  param([string]$Name)
  $root = [System.IO.Path]::GetFullPath($WorkRoot).TrimEnd('\', '/')
  $target = [System.IO.Path]::GetFullPath((Join-Path $root $Name))
  $rootWithSeparator = $root + [System.IO.Path]::DirectorySeparatorChar
  if (-not $target.StartsWith($rootWithSeparator, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Target escapes WorkRoot: $Name"
  }
  if (Test-Path -LiteralPath $target) {
    if (-not $Force) { throw "Target exists; pass -Force to replace: $target" }
    Remove-Item -LiteralPath $target -Recurse -Force
  }
  Initialize-EmptyGitRepo -Path $target
  $target
}

function Count-Words {
  param([string]$Text)
  if ([string]::IsNullOrWhiteSpace($Text)) { return 0 }
  @($Text -split '\s+' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }).Count
}

function Assert-NoSecretLeak {
  param([string]$Text, [object[]]$Secrets)
  foreach ($secret in $Secrets) {
    if ($null -eq $secret) { continue }
    $value = if ($secret.PSObject.Properties.Name -contains "Value") { [string]$secret.Value } else { [string]$secret }
    if ($value.Trim().Length -ge 4 -and $Text.Contains($value)) {
      throw "Secret value appeared in serialized self-test artifacts."
    }
  }
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
  if ([int]$Turn.provider_trace.provider_turns -lt 1) {
    throw "$Label did not record a provider turn."
  }
  if ([long]$Turn.provider_trace.estimated_input_tokens -lt 1) {
    throw "$Label did not record estimated input tokens."
  }
  if ($Turn.provider_trace.usage_reported -eq $true -and [long]$Turn.provider_trace.total_tokens -lt 1) {
    throw "$Label reported provider usage without a positive total token count."
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

function Assert-LocalPlannerConfirmation {
  param([object]$Turn, [string]$Label)
  if ($null -ne $Turn.provider_trace) {
    throw "$Label unexpectedly consumed a provider turn."
  }
  $localEvent = @($Turn.events | Where-Object { $_.type -eq "planner.confirmation.local" })
  if ($localEvent.Count -ne 1) {
    throw "$Label did not record planner.confirmation.local."
  }
}

function Get-JsonText {
  param([object]$Value)
  if ($null -eq $Value) { return "" }
  $Value | ConvertTo-Json -Depth 100
}

function Copy-JsonObject {
  param([object]$Value)
  $Value | ConvertTo-Json -Depth 100 | ConvertFrom-Json
}

if ($LoadLocalEnv) {
  $localEnvPath = Join-Path $repoRoot ".local-env.ps1"
  if (-not (Test-Path -LiteralPath $localEnvPath)) { Stop-Or-Skip -Reason "Local env file not found: $localEnvPath" }
  . $localEnvPath
}

$liveFlag = [Environment]::GetEnvironmentVariable("CODER_SELFTEST_LIVE", "Process")
if ($liveFlag -ne "1" -and -not $Live) {
  Stop-Or-Skip -Reason "Set CODER_SELFTEST_LIVE=1 or pass -Live to run live Coder self-tests."
}

$normalizedProvider = $Provider.Trim().ToLowerInvariant()
if ([string]::IsNullOrWhiteSpace($normalizedProvider)) { $normalizedProvider = "deepseek" }
$apiKeyCandidates = @()
if (-not [string]::IsNullOrWhiteSpace($ApiKeyEnv)) { $apiKeyCandidates += $ApiKeyEnv }
switch ($normalizedProvider) {
  "deepseek" { $apiKeyCandidates += @("DEEPSEEK_API_KEY", "LLM_API_KEY") }
  "openai" { $apiKeyCandidates += @("OPENAI_API_KEY", "LLM_API_KEY") }
  default { $apiKeyCandidates += @("LLM_API_KEY", "DEEPSEEK_API_KEY", "CODER_API_KEY") }
}
$apiKey = Get-FirstEnvValue -Names $apiKeyCandidates
if ($null -eq $apiKey) { Stop-Or-Skip -Reason "No live provider API key found in: $($apiKeyCandidates -join ', ')" }
if ([string]::IsNullOrWhiteSpace($BaseUrl)) { $BaseUrl = [Environment]::GetEnvironmentVariable("LLM_BASE_URL", "Process") }
if ([string]::IsNullOrWhiteSpace($BaseUrl)) { $BaseUrl = if ($normalizedProvider -eq "openai") { "https://api.openai.com/v1" } else { "https://api.deepseek.com" } }
if ([string]::IsNullOrWhiteSpace($Model)) { $Model = [Environment]::GetEnvironmentVariable("LLM_MODEL", "Process") }
if ([string]::IsNullOrWhiteSpace($Model)) { $Model = if ($normalizedProvider -eq "openai") { "gpt-5.5" } else { "deepseek-chat" } }
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

$storePath = Resolve-UnderRepo -PathValue $Store
if (Test-Path -LiteralPath $storePath) { Remove-Item -LiteralPath $storePath -Recurse -Force }
New-Item -ItemType Directory -Force -Path $storePath | Out-Null
New-Item -ItemType Directory -Force -Path $WorkRoot | Out-Null
$outLog = Join-Path $storePath "server.out.log"
$errLog = Join-Path $storePath "server.err.log"

$processEnv = [Environment]::GetEnvironmentVariables("Process")
if ($processEnv.Contains("Path") -and $processEnv.Contains("PATH")) {
  [Environment]::SetEnvironmentVariable("PATH", $null, "Process")
}
[Environment]::SetEnvironmentVariable("CODER_RUNTIME_CACHE_DIR", (Join-Path $repoRoot "tmp\coder-runtime-cache"), "Process")
[Environment]::SetEnvironmentVariable("LLM_BASE_URL", $BaseUrl, "Process")
[Environment]::SetEnvironmentVariable("LLM_MODEL", $Model, "Process")
[Environment]::SetEnvironmentVariable("LLM_API_KEY", $apiKey.Value, "Process")
[Environment]::SetEnvironmentVariable("NO_PROXY", "127.0.0.1,localhost,::1", "Process")
[Environment]::SetEnvironmentVariable("no_proxy", "127.0.0.1,localhost,::1", "Process")

if ($IncludeOpenEndedCases) {
  $node = (Get-Command node).Source
  & $node (Join-Path $repoRoot "scripts\prepare-browser-verifier-runtime.mjs") --check --fail-if-missing --json | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Browser verifier runtime is missing. Run npm run browser-verifier:install before open-ended live tests."
  }
}

$cargo = (Get-Command cargo).Source
& $cargo build -p coder-cli --bin coder-rust
if ($LASTEXITCODE -ne 0) { throw "Failed to build coder-rust for the live self-test." }
$cargoTargetRoot = [Environment]::GetEnvironmentVariable("CARGO_TARGET_DIR", "Process")
if ([string]::IsNullOrWhiteSpace($cargoTargetRoot)) { $cargoTargetRoot = Join-Path $repoRoot "target" }
$serverFileName = if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) { "coder-rust.exe" } else { "coder-rust" }
$serverExecutable = Join-Path $cargoTargetRoot (Join-Path "debug" $serverFileName)
if (-not (Test-Path -LiteralPath $serverExecutable)) {
  throw "Built coder-rust executable was not found at $serverExecutable"
}
$server = Start-Process -FilePath $serverExecutable `
  -ArgumentList @("server", "--host", $HostName, "--port", "$Port", "--store", $storePath) `
  -WorkingDirectory $repoRoot `
  -RedirectStandardOutput $outLog `
  -RedirectStandardError $errLog `
  -WindowStyle Hidden `
  -PassThru

try {
  $base = "http://${HostName}:${Port}"
  $health = $null
  foreach ($attempt in 1..160) {
    try {
      $health = Invoke-RestMethod -Method Get -Uri "$base/api/v3/health"
      break
    } catch {
      Start-Sleep -Milliseconds 500
    }
  }
  Assert-SelfTest ($null -ne $health -and $health.status -eq "ok") "Rust server health check failed. See $errLog"

  $jsonHeaders = @{ "Content-Type" = "application/json" }
  $providerBaseUrls = @{}
  $providerBaseUrls[$normalizedProvider] = $BaseUrl
  $providerProxyUrls = @{}
  if ($resolvedProviderProxyMode -eq "explicit") { $providerProxyUrls[$normalizedProvider] = $ProviderProxyUrl }
  $providerProxyModes = @{}
  $providerProxyModes[$normalizedProvider] = $resolvedProviderProxyMode
  $providerApiKeys = @{}
  $providerApiKeys[$normalizedProvider] = $apiKey.Value
  $providerSettings = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/providers/settings" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
    default_provider = $normalizedProvider
    default_model = $Model
    base_urls = $providerBaseUrls
    proxy_urls = $providerProxyUrls
    proxy_modes = $providerProxyModes
    api_keys = $providerApiKeys
    mock_mode = $false
  })
  Assert-SelfTest ($providerSettings.status.default_status.credential_configured -eq $true) "Provider Settings did not detect credentials."
  $providerTest = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/providers/test" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{ provider = $normalizedProvider; mock = $false })
  Assert-SelfTest ($providerTest.test.ok -eq $true) "Live provider test failed: $($providerTest.test.message)"

  $defaultWorkflow = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/workflows/default"
  $baseConfig = Copy-JsonObject -Value $defaultWorkflow.config
  $config = Copy-JsonObject -Value $baseConfig
  $config.harnesses."native-code-edit".permissions.run_commands = "allow"
  $config.harnesses."native-code-edit".permissions.child_harness_permissions = "allow"
  $config.harnesses."review-only".permissions.child_harness_permissions = "allow"
  $config.hooks = @{
    PreToolUse = @(
      @{
        matcher = "Read"
        hooks = @(
          @{
            type = "command"
            shell = "powershell"
            command = "Write-Output live-pre-hook"
          }
        )
      },
      @{
        matcher = "read_file"
        hooks = @(
          @{
            type = "prompt"
            model = $Model
            prompt = 'Return exactly one JSON object and no markdown: {"ok":true,"reason":"live prompt hook passed"}. Tool input: $ARGUMENTS'
          },
          @{
            type = "agent"
            model = $Model
            prompt = 'Call the StructuredOutput tool exactly once with {"ok":true,"reason":"live agent hook passed"}. Do not block this tool. Tool input: $ARGUMENTS'
          }
        )
      },
      @{
        matcher = "command_run"
        hooks = @(
          @{
            type = "command"
            shell = "powershell"
            command = "Write-Output live-async-rewake-hook; exit 2"
            asyncRewake = $true
          }
        )
      }
    )
  }

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

    $backgroundCommandOutput = Invoke-ModelTool -ToolUseId "toolu-selftest-command-output" -ToolName "BashOutputTool" -RunId $RunId -HarnessId "native-code-edit" -ToolInput @{
      task_id = $backgroundCommandTaskId
      block = $true
      timeout_ms = 15000
      run_id = $RunId
    }
    Assert-SelfTest ($backgroundCommandOutput.status -eq "completed") "Runtime probe: BashOutputTool call failed."
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
    $stopCommand = Invoke-ModelTool -ToolUseId "toolu-selftest-task-stop" -ToolName "TaskStop" -RunId $RunId -HarnessId "native-code-edit" -ToolInput @{
      task_id = $longCommandTaskId
      run_id = $RunId
    }
    Assert-SelfTest ($stopCommand.status -eq "completed") "Runtime probe: TaskStop call failed."
    Assert-SelfTest ($stopCommand.payload.cancelled -eq $true) "Runtime probe: TaskStop did not cancel the running command."

    $subagentStart = Invoke-ModelTool -ToolUseId "toolu-selftest-subagent-start" -ToolName "agent_subagent" -RunId $RunId -HarnessId "review-only" -AgentId "selftest-parent" -ToolInput @{
      repo_root = $RepoPath
      task = "Read README.md and report a one-line status for the live runtime boundary probe."
      run_in_background = $true
      workflow_id = "planner-led"
      node_id = "runtime-boundary"
      parent_harness_id = "review-only"
      parent_agent_id = "selftest-parent"
      subagent_name = "runtime-boundary-probe"
      run_id = $RunId
    }
    $subagentTask = $subagentStart.payload.background_task
    Assert-SelfTest ($null -ne $subagentTask) "Runtime probe: background subagent task metadata missing."
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($subagentTask.task_id)) "Runtime probe: background subagent task id missing."
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($subagentTask.transcript_ref)) "Runtime probe: background subagent transcript ref missing."

    $subagentStatus = Invoke-ModelTool -ToolUseId "toolu-selftest-subagent-status" -ToolName "TaskOutput" -RunId $RunId -HarnessId "review-only" -AgentId "selftest-parent" -ToolInput @{
      task_id = $subagentTask.task_id
      block = $true
      timeout_ms = 30000
      run_id = $RunId
    }
    Assert-SelfTest ($subagentStatus.status -eq "completed") "Runtime probe: TaskOutput for subagent failed."
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
    $subagentCancel = Invoke-ModelTool -ToolUseId "toolu-selftest-subagent-cancel-stop" -ToolName "TaskStop" -RunId $RunId -HarnessId "native-code-edit" -AgentId "selftest-parent" -ToolInput @{
      task_id = $subagentCancelTask.task_id
      run_id = $RunId
    }
    Assert-SelfTest ($subagentCancel.status -eq "completed") "Runtime probe: TaskStop for background subagent failed."
    Assert-SelfTest ($subagentCancel.payload.task_type -eq "local_agent") "Runtime probe: TaskStop did not resolve the task as a local agent."
    Assert-SelfTest ($subagentCancel.payload.cancelled -eq $true) "Runtime probe: TaskStop did not cancel the background subagent."
    Assert-SelfTest ($subagentCancel.payload.task_status -eq "cancelled") "Runtime probe: cancelled background subagent did not report cancelled status."

    $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/events?tail=true&limit=1000"
    $eventItems = @($events.events)
    Assert-SelfTest (@($eventItems | Where-Object { $_.kind -eq "model_tool.phase" -and $_.payload.phase -eq "pre_tool_use_hooks" }).Count -ge 1) "Runtime probe: model_tool.phase pre-hook event missing."
    Assert-SelfTest (@($eventItems | Where-Object { $_.kind -eq "command.started" }).Count -ge 1) "Runtime probe: command.started event missing."

    $serializedProbe = Get-JsonText -Value @($readResponse, $backgroundCommand, $backgroundCommandOutput, $longCommand, $stopCommand, $subagentStart, $subagentStatus, $subagentCancelStart, $subagentCancel, $events)
    Assert-NoSecretLeak -Text $serializedProbe -Secrets @($apiKey)

    [pscustomobject]@{
      hook_phase = "passed"
      background_command = "passed"
      task_stop = "passed"
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
      source = "live-selftest-verifier-repair-probe"
      summary = "intentional verifier failure before repair"
      reason = "intentional verifier repair probe failure"
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
      source = "live-selftest-verifier-repair-probe"
      summary = "intentional verifier repair completed"
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
    Assert-SelfTest (@($completed.report.checks | Where-Object { $_.Contains("intentional verifier repair probe failure") }).Count -ge 1) "Verification repair probe: repaired report dropped failed-check history."
    Assert-SelfTest (@($completed.report.checks | Where-Object { $_.Contains("intentional verifier repair completed") }).Count -ge 1) "Verification repair probe: repaired report did not include completed verification."

    $preview = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/report/preview"
    Assert-SelfTest ($preview.report.status -eq "completed") "Verification repair probe: report preview did not remain completed after repair."

    $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$RunId/events?tail=true&limit=1000"
    $eventItems = @($events.events)
    Assert-SelfTest (@($eventItems | Where-Object { $_.kind -eq "verification.failed" -and $_.payload.source -eq "live-selftest-verifier-repair-probe" }).Count -ge 1) "Verification repair probe: verification.failed event missing."
    Assert-SelfTest (@($eventItems | Where-Object { $_.kind -eq "verification.completed" -and $_.payload.source -eq "live-selftest-verifier-repair-probe" }).Count -ge 1) "Verification repair probe: verification.completed event missing."

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
    Assert-SelfTest (@($compaction.claude_sources).Count -ge 1) "Transcript compaction probe: Claude source metadata missing."

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
      claude_sources = @($compaction.claude_sources)
    }
  }

  function Invoke-CoderCase {
    param(
      [string]$Name,
      [string]$Difficulty,
      [string]$Task,
      [string]$ReadyMessage,
      [string[]]$ExpectedFiles,
      [string]$NodeCheckFile = "",
      [string[]]$RequireModelToolNames = @(),
      [switch]$ExpectProviderPlanner,
      [switch]$TestParallelPlanner
    )

    $repoPath = Reset-TargetRepo -Name $Name
    $sessionResponse = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      workflow_id = $defaultWorkflow.workflow_id
      planner_agent_id = "planner"
      config = $config
      mode = "discuss"
    })
    $sessionId = $sessionResponse.session.session_id
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($sessionId)) "${Name}: Planner session did not return session_id."

    $firstTurn = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/turn" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      message = $Task
      confirmed = $false
      mode = "discuss"
      planner_agent_id = "planner"
      config = $config
    })
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace($firstTurn.assistant_message)) "${Name}: first Planner turn returned no assistant text."
    Assert-SelfTest ((Count-Words $firstTurn.assistant_message) -le 600) "${Name}: first Planner response too long."
    Assert-SelfTest ($firstTurn.should_start_workflow -eq $false) "${Name}: Planner tried to execute during chat."
    Assert-ProviderTrace -Turn $firstTurn -Label "${Name}: first Planner turn"

    $secondTurn = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/turn" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      message = $ReadyMessage
      confirmed = $true
      mode = "work"
      planner_agent_id = "planner"
      config = $config
    })
    Assert-SelfTest ($secondTurn.ready -eq $true) "${Name}: Planner did not mark task ready."
    Assert-SelfTest ($secondTurn.assistant_message.Contains("Click Start Work")) "${Name}: Planner did not direct Start Work."
    Assert-SelfTest ($secondTurn.assistant_message.Contains("native executor")) "${Name}: Planner did not mention native executor."
    Assert-LocalPlannerConfirmation -Turn $secondTurn -Label "${Name}: second Planner turn"

    $startWorkUri = "$base/api/v3/planner-chat/sessions/$sessionId/start-work"
    $startWorkBody = ConvertTo-JsonBody @{
      repo = $repoPath
      workflow_id = $defaultWorkflow.workflow_id
      planner_agent_id = "planner"
      config = $config
      scopes = $ExpectedFiles
    }
    $parallelTurn = $null
    $startWork = Invoke-RestJsonWithRetry -Method Post -Uri $startWorkUri -Headers $jsonHeaders -Body $startWorkBody
    Assert-SelfTest ($startWork.status -eq "running") "${Name}: Start Work was not accepted asynchronously: $($startWork | ConvertTo-Json -Depth 20)"
    Assert-SelfTest (-not [string]::IsNullOrWhiteSpace([string]$startWork.run_id)) "${Name}: Start Work did not return run_id."
    Assert-SelfTest ($startWork.session.work_in_progress -eq $true) "${Name}: accepted work did not expose work_in_progress=true."
    $runId = [string]$startWork.run_id
    if ($TestParallelPlanner) {
      $parallelMessage = "While the current workflow runs, plan one optional follow-up improvement for a later task. Do not change or stop the active work."
      $parallelTurn = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/turn" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
        message = $parallelMessage
        confirmed = $true
        mode = "discuss"
        planner_agent_id = "planner"
        config = $config
      })
      Assert-SelfTest ($parallelTurn.session.work_in_progress -eq $true) "${Name}: Planner turn did not retain the active workflow state."
      Assert-ProviderTrace -Turn $parallelTurn -Label "${Name}: parallel Planner turn"
    }

    $deadline = [DateTime]::UtcNow.AddMinutes(30)
    $completedSession = $null
    while ([DateTime]::UtcNow -lt $deadline) {
      $candidate = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/planner-chat/sessions/$sessionId"
      if ($candidate.session.work_in_progress -ne $true) {
        $completedSession = $candidate.session
        break
      }
      Start-Sleep -Milliseconds 500
    }
    Assert-SelfTest ($null -ne $completedSession) "${Name}: background workflow did not reach a terminal session state within 30 minutes."
    Assert-SelfTest ($completedSession.latest_run_id -eq $runId) "${Name}: completed workflow did not update latest_run_id."
    if ($TestParallelPlanner) {
      $mergedParallelTurn = @($completedSession.turns | Where-Object { $_.content -eq $parallelMessage })
      Assert-SelfTest ($mergedParallelTurn.Count -eq 1) "${Name}: workflow completion overwrote the parallel Planner turn."
    }

    $terminalReport = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$runId/report/preview"
    Assert-SelfTest ($terminalReport.report.status -eq "completed") "${Name}: Start Work did not complete: $($terminalReport | ConvertTo-Json -Depth 20)"

    foreach ($file in $ExpectedFiles) {
      Assert-SelfTest (Test-Path -LiteralPath (Join-Path $repoPath $file)) "${Name}: missing expected file $file."
    }
    if (-not [string]::IsNullOrWhiteSpace($NodeCheckFile)) {
      $node = (Get-Command node).Source
      $nodeCheck = Invoke-NativeCapture -FilePath $node -Arguments @("--check", (Join-Path $repoPath $NodeCheckFile))
      Assert-SelfTest ($nodeCheck.ExitCode -eq 0) "${Name}: node --check failed: $($nodeCheck.Output -join "`n")"
    }

    $timeline = Invoke-RestJsonWithRetry -Method Get -Uri "$base$($startWork.timeline_url)"
    $timelineItems = @($timeline.items)
    Assert-SelfTest ($timelineItems.Count -ge 1) "${Name}: Timeline was empty."
    Assert-SelfTest (@($timelineItems | Where-Object { $_.title -in @("Executor backend: Native", "Executor backend: native-rust") }).Count -ge 1) "${Name}: Timeline did not show native backend."
    Assert-SelfTest (@($timelineItems | Where-Object { $_.type -eq "final_summary" }).Count -ge 1) "${Name}: Timeline did not include final summary."

    $report = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$runId/report/preview"
    Assert-SelfTest ($report.report.status -eq "completed") "${Name}: Final report preview did not complete."
    Assert-SelfTest ((Count-Words $report.report.summary) -le 500) "${Name}: Final summary too long."

    $changes = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$runId/changes"
    $changeSets = @($changes.changes)
    Assert-SelfTest ($changeSets.Count -ge 1) "${Name}: Review Changes returned no changes."
    $changedPaths = @($changeSets[0].changed_files | ForEach-Object { $_.path })
    foreach ($file in $ExpectedFiles) {
      Assert-SelfTest ($changedPaths -contains $file) "${Name}: Review Changes did not include $file."
    }

    $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base$($startWork.events_url)"
    $eventItems = @($events.events)
    $modelToolNames = @($eventItems | Where-Object {
      $_.kind -eq "model.tool_call.completed"
    } | ForEach-Object {
      [string]$_.payload.tool_name
    } | Where-Object {
      -not [string]::IsNullOrWhiteSpace($_)
    })
    foreach ($requiredToolName in $RequireModelToolNames) {
      Assert-SelfTest ($modelToolNames -contains $requiredToolName) "${Name}: expected model tool '$requiredToolName' was not called. Observed tools: $($modelToolNames -join ', ')"
    }
    $providerTurnEvents = @($eventItems | Where-Object { $_.kind -eq "model.provider_turn.completed" })
    Assert-SelfTest ($providerTurnEvents.Count -ge 1) "${Name}: no provider turn usage events were recorded."
    $workflowPlannerCalls = @($eventItems | Where-Object {
      $_.kind -eq "node.started" -and $_.payload.node_id -eq "workflow-planner"
    })
    Assert-SelfTest ($workflowPlannerCalls.Count -ge 1) "${Name}: verifier evidence did not reach workflow-planner."
    $providerPlannerDecisions = @($eventItems | Where-Object {
      $_.kind -eq "planner.workflow_decision" -and $_.payload.implementation -eq "provider-backed-bounded-planner"
    })
    if ($ExpectProviderPlanner) {
      Assert-SelfTest ($providerPlannerDecisions.Count -ge 1) "${Name}: open-ended quality goal did not invoke the provider-backed workflow Planner."
    } else {
      Assert-SelfTest ($providerPlannerDecisions.Count -eq 0) "${Name}: closed successful task spent a provider-backed workflow Planner turn."
    }
    $providerInputTokens = @($providerTurnEvents | ForEach-Object { if ($null -eq $_.payload.input_tokens) { 0 } else { [long]$_.payload.input_tokens } } | Measure-Object -Sum).Sum
    $providerOutputTokens = @($providerTurnEvents | ForEach-Object { if ($null -eq $_.payload.output_tokens) { 0 } else { [long]$_.payload.output_tokens } } | Measure-Object -Sum).Sum
    $providerCacheReadTokens = @($providerTurnEvents | ForEach-Object { if ($null -eq $_.payload.cache_read_tokens) { 0 } else { [long]$_.payload.cache_read_tokens } } | Measure-Object -Sum).Sum
    $estimatedInputTokens = @($providerTurnEvents | ForEach-Object { if ($null -eq $_.payload.estimated_input_tokens) { 0 } else { [long]$_.payload.estimated_input_tokens } } | Measure-Object -Sum).Sum
    $eventsPath = Join-Path $storePath "runs\$runId\events.jsonl"
    $reportPath = Join-Path $storePath "runs\$runId\artifacts\final-report.json"
    $serialized = (@($providerSettings, $providerTest, $firstTurn, $secondTurn, $parallelTurn, $startWork, $events, $timeline, $report, $changes) | ConvertTo-Json -Depth 100) + "`n"
    foreach ($file in @($eventsPath, $reportPath)) {
      if (Test-Path -LiteralPath $file) { $serialized += [System.IO.File]::ReadAllText($file) }
    }
    Assert-NoSecretLeak -Text $serialized -Secrets @($apiKey)

    [pscustomobject]@{
      name = $Name
      difficulty = $Difficulty
      repo = $repoPath
      session_id = $sessionId
      run_id = $runId
      status = $report.report.status
      start_work_status = $startWork.status
      planner_turns = @($secondTurn.session.turns).Count
      first_turn_provider_trace = $firstTurn.provider_trace
      second_turn_provider_trace = $secondTurn.provider_trace
      parallel_planner = if ($TestParallelPlanner) { "passed" } else { "not_applicable" }
      timeline_items = $timelineItems.Count
      review_changes = $changeSets.Count
      changed_files = $changedPaths
      model_tool_names = @($modelToolNames)
      provider_turns = $providerTurnEvents.Count
      provider_input_tokens = $providerInputTokens
      provider_output_tokens = $providerOutputTokens
      provider_cache_read_tokens = $providerCacheReadTokens
      estimated_input_tokens = $estimatedInputTokens
      workflow_planner_calls = $workflowPlannerCalls.Count
      provider_planner_decisions = $providerPlannerDecisions.Count
      final_summary_words = Count-Words $report.report.summary
      node_check = if ([string]::IsNullOrWhiteSpace($NodeCheckFile)) { "not_applicable" } else { "passed" }
    }
  }

  $results = @()
  $easyResult = Invoke-CoderCase `
    -Name "coder-selftest-easy-note" `
    -Difficulty "easy" `
    -ExpectedFiles @("README.md") `
    -Task "Self-test easy task. In this repository, plan to create README.md only. The file should contain a short title, one sentence explaining this is a Coder self-test, and three bullet points. Do not execute until Start Work." `
    -ReadyMessage "The plan looks good. Keep it simple."
  $results += $easyResult

  $mediumResult = Invoke-CoderCase `
    -Name "coder-selftest-medium-js" `
    -Difficulty "medium" `
    -ExpectedFiles @("README.md", "math.js") `
    -NodeCheckFile "math.js" `
    -Task "Self-test medium task. In this repository, plan a dependency-free JavaScript utility. Create math.js exporting add, subtract, multiply, and divide functions with divide throwing on division by zero. Create README.md documenting usage. Do not execute until Start Work." `
    -ReadyMessage "The plan looks good. Keep it simple."
  $results += $mediumResult

  $liveAsyncRewakeProbe = Invoke-LiveAsyncRewakeProbe -RunId $mediumResult.run_id
  $runtimeBoundaryProbe = Invoke-RuntimeBoundaryProbe -RunId $mediumResult.run_id -RepoPath $mediumResult.repo
  $liveModelHookProbe = Invoke-LiveModelHookProbe -RunId $mediumResult.run_id -RepoPath $mediumResult.repo
  $verificationRepairProbe = Invoke-VerificationRepairProbe -RunId $mediumResult.run_id
  $transcriptCompactionProbe = Invoke-TranscriptCompactionProbe -RunId $mediumResult.run_id
  $openEndedResult = $null

  if ($IncludeOpenEndedCases) {
    $openEndedConfig = Copy-JsonObject -Value $baseConfig
    $openEndedConfig.harnesses."native-code-edit".permissions.run_commands = "allow"
    $openEndedConfig.harnesses."native-code-edit".permissions.child_harness_permissions = "allow"
    $openEndedConfig.harnesses."review-only".permissions.child_harness_permissions = "allow"
    $config = $openEndedConfig
    $openEndedResult = Invoke-CoderCase `
      -Name "coder-selftest-open-ended-garden" `
      -Difficulty "open-ended" `
      -ExpectedFiles @("index.html") `
      -RequireModelToolNames @("agent_subagent") `
      -ExpectProviderPlanner `
      -TestParallelPlanner `
      -Task "Build a fun, polished browser garden defense game in this empty repository. Use your judgement. Ask a separate child agent to review it before finalizing. Do not execute until Start Work." `
      -ReadyMessage "The plan looks good. Keep it simple."
    $results += $openEndedResult
  }

  [pscustomobject]@{
    status = "ok"
    validation = "live_coder_selftest_suite"
    provider = $normalizedProvider
    model = $Model
    provider_test = $providerTest.test.mode
    backend_selected = "native-rust"
    cases = $results
    live_async_rewake_probe = $liveAsyncRewakeProbe
    runtime_boundary_probe = $runtimeBoundaryProbe
    live_model_hook_probe = $liveModelHookProbe
    verification_repair_probe = $verificationRepairProbe
    transcript_compaction_probe = $transcriptCompactionProbe
    open_ended_cases = if ($IncludeOpenEndedCases) { @($openEndedResult) } else { @() }
    secrets_check = "passed"
    store = $storePath
  } | ConvertTo-Json -Depth 20
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
}
