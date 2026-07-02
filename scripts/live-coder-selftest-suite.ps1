param(
  [string]$HostName = "127.0.0.1",
  [int]$Port = 8881,
  [string]$WorkRoot = "F:\ccc",
  [string]$Store = ".tmp\live-coder-selftest-suite\store",
  [string]$Provider = "deepseek",
  [string]$BaseUrl = "",
  [string]$Model = "",
  [string]$ProviderProxyUrl = "",
  [string]$ApiKeyEnv = "",
  [switch]$Live,
  [switch]$LoadLocalEnv,
  [switch]$Force,
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

function Resolve-AgentCanvasBin {
  $existing = Get-Command agent-canvas -ErrorAction SilentlyContinue
  if ($null -ne $existing) { return $null }

  $cachePath = Resolve-UnderRepo -PathValue ".tmp\npm-cache"
  $candidate = Get-ChildItem -Path (Join-Path $cachePath "_npx") -Recurse -Filter "agent-canvas.mjs" -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -like "*node_modules*@openhands*agent-canvas*bin*" } |
    Select-Object -First 1
  if ($null -eq $candidate) {
    $npx = Get-Command npx.cmd -ErrorAction SilentlyContinue
    if ($null -eq $npx) { $npx = Get-Command npx -ErrorAction SilentlyContinue }
    if ($null -eq $npx) { throw "Managed OpenHands runtime needs bundled agent-canvas or npx." }
    Invoke-Native -FilePath $npx.Source -Arguments @("--cache", $cachePath, "--yes", "@openhands/agent-canvas", "--info")
    $candidate = Get-ChildItem -Path (Join-Path $cachePath "_npx") -Recurse -Filter "agent-canvas.mjs" -ErrorAction SilentlyContinue |
      Where-Object { $_.FullName -like "*node_modules*@openhands*agent-canvas*bin*" } |
      Select-Object -First 1
  }
  if ($null -eq $candidate) { throw "Managed OpenHands runtime could not locate agent-canvas." }
  $candidate.FullName
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
if ([string]::IsNullOrWhiteSpace($Model)) { $Model = if ($normalizedProvider -eq "openai") { "gpt-5.5" } else { "deepseek-v4-flash" } }
if ([string]::IsNullOrWhiteSpace($ProviderProxyUrl)) { $ProviderProxyUrl = [Environment]::GetEnvironmentVariable("HTTPS_PROXY", "Process") }
if ([string]::IsNullOrWhiteSpace($ProviderProxyUrl)) { $ProviderProxyUrl = [Environment]::GetEnvironmentVariable("HTTP_PROXY", "Process") }

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
[Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", (Join-Path $repoRoot ".tmp\cargo-target"), "Process")
[Environment]::SetEnvironmentVariable("LLM_BASE_URL", $BaseUrl, "Process")
[Environment]::SetEnvironmentVariable("LLM_MODEL", $Model, "Process")
[Environment]::SetEnvironmentVariable("LLM_API_KEY", $apiKey.Value, "Process")
[Environment]::SetEnvironmentVariable("OPENHANDS_SESSION_API_KEY", $null, "Process")
$agentCanvasBin = Resolve-AgentCanvasBin
if (-not [string]::IsNullOrWhiteSpace($agentCanvasBin)) {
  $node = (Get-Command node).Source
  [Environment]::SetEnvironmentVariable("CODER_OPENHANDS_COMMAND", $node, "Process")
  [Environment]::SetEnvironmentVariable("CODER_OPENHANDS_ARGS", "$agentCanvasBin --backend-only --port {port}", "Process")
}
$python312 = "F:\bbb\python312\python.exe"
if (Test-Path -LiteralPath $python312) {
  [Environment]::SetEnvironmentVariable("CODER_OPENHANDS_PYTHON", $python312, "Process")
}
[Environment]::SetEnvironmentVariable("NO_PROXY", "127.0.0.1,localhost,::1", "Process")
[Environment]::SetEnvironmentVariable("no_proxy", "127.0.0.1,localhost,::1", "Process")

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
  if (-not [string]::IsNullOrWhiteSpace($ProviderProxyUrl)) { $providerProxyUrls[$normalizedProvider] = $ProviderProxyUrl }
  $providerApiKeys = @{}
  $providerApiKeys[$normalizedProvider] = $apiKey.Value
  $providerSettings = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/providers/settings" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
    default_provider = $normalizedProvider
    default_model = $Model
    base_urls = $providerBaseUrls
    proxy_urls = $providerProxyUrls
    api_keys = $providerApiKeys
    mock_mode = $false
  })
  Assert-SelfTest ($providerSettings.status.default_status.credential_configured -eq $true) "Provider Settings did not detect credentials."
  $providerTest = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/providers/test" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{ provider = $normalizedProvider; mock = $false })
  Assert-SelfTest ($providerTest.test.ok -eq $true) "Live provider test failed: $($providerTest.test.message)"

  $openHandsSettings = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/openhands/settings" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
    enabled = $true
    runtime_mode = "managed"
    workspace_mode = "local"
    allow_native_fallback = $false
  })
  $openHandsStatus = $openHandsSettings.status
  foreach ($attempt in 1..420) {
    if ($openHandsStatus.status -eq "connected") { break }
    Start-Sleep -Seconds 1
    $openHandsStatus = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/openhands/status"
  }
  Assert-SelfTest ($openHandsStatus.status -eq "connected") "Managed OpenHands did not connect: $($openHandsStatus.detail)"

  $defaultWorkflow = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/workflows/default"
  $config = $defaultWorkflow.config
  $config.workflows."planner-led".max_rounds = 1
  $config.workflows."planner-led".edges = @(@{ from = "planner"; to = "executor"; on = "ready" })
  $config.harnesses."openhands-code-edit".openhands.prefer_websocket = $false
  $config.harnesses."openhands-code-edit".openhands.poll_interval_ms = 1000
  $config.harnesses."openhands-code-edit".openhands.max_event_poll_seconds = 420
  $config.harnesses."openhands-code-edit".openhands.max_events = 100
  $config.harnesses."openhands-code-edit".openhands.api_paths = @{}
  $config.harnesses."openhands-code-edit".openhands.run_start_strategy = "post_user_event_with_run_true"

  function Invoke-CoderCase {
    param(
      [string]$Name,
      [string]$Difficulty,
      [string]$Task,
      [string]$ReadyMessage,
      [string[]]$ExpectedFiles,
      [string]$NodeCheckFile = ""
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

    $secondTurn = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/turn" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      message = $ReadyMessage
      confirmed = $true
      mode = "work"
      planner_agent_id = "planner"
      config = $config
    })
    Assert-SelfTest ($secondTurn.ready -eq $true) "${Name}: Planner did not mark task ready."
    Assert-SelfTest ($secondTurn.assistant_message.Contains("Click Start Work")) "${Name}: Planner did not direct Start Work."
    Assert-SelfTest ($secondTurn.assistant_message.Contains("OpenHands executor")) "${Name}: Planner did not mention OpenHands executor."

    $startWork = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/start-work" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      repo = $repoPath
      workflow_id = $defaultWorkflow.workflow_id
      planner_agent_id = "planner"
      config = $config
      scopes = $ExpectedFiles
    })
    Assert-SelfTest ($startWork.status -eq "completed") "${Name}: Start Work did not complete: $($startWork | ConvertTo-Json -Depth 20)"
    $runId = $startWork.run_id

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
    Assert-SelfTest (@($timelineItems | Where-Object { $_.title -eq "Executor backend: OpenHands" }).Count -ge 1) "${Name}: Timeline did not show OpenHands backend."
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
    $eventsPath = Join-Path $storePath "runs\$runId\events.jsonl"
    $reportPath = Join-Path $storePath "runs\$runId\artifacts\final-report.json"
    $serialized = (@($providerSettings, $providerTest, $openHandsSettings, $firstTurn, $secondTurn, $startWork, $events, $timeline, $report, $changes) | ConvertTo-Json -Depth 100) + "`n"
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
      status = $startWork.status
      planner_turns = @($secondTurn.session.turns).Count
      timeline_items = $timelineItems.Count
      review_changes = $changeSets.Count
      changed_files = $changedPaths
      final_summary_words = Count-Words $report.report.summary
      node_check = if ([string]::IsNullOrWhiteSpace($NodeCheckFile)) { "not_applicable" } else { "passed" }
    }
  }

  $results = @()
  $results += Invoke-CoderCase `
    -Name "coder-selftest-easy-note" `
    -Difficulty "easy" `
    -ExpectedFiles @("README.md") `
    -Task "Self-test easy task. In this repository, plan to create README.md only. The file should contain a short title, one sentence explaining this is a Coder self-test, and three bullet points. Do not execute until Start Work." `
    -ReadyMessage "Confirm this easy self-test is ready. Execute only after Start Work through OpenHands. Create README.md only, leave it uncommitted, and stop with a short final summary."

  $results += Invoke-CoderCase `
    -Name "coder-selftest-medium-js" `
    -Difficulty "medium" `
    -ExpectedFiles @("README.md", "math.js") `
    -NodeCheckFile "math.js" `
    -Task "Self-test medium task. In this repository, plan a dependency-free JavaScript utility. Create math.js exporting add, subtract, multiply, and divide functions with divide throwing on division by zero. Create README.md documenting usage. Do not execute until Start Work." `
    -ReadyMessage "Confirm this medium self-test is ready. Execute only after Start Work through OpenHands. Create README.md and math.js only, run node --check math.js if available, leave changes uncommitted, and stop with a short final summary."

  [pscustomobject]@{
    status = "ok"
    validation = "live_coder_selftest_suite"
    provider = $normalizedProvider
    model = $Model
    provider_test = $providerTest.test.mode
    openhands_status = $openHandsStatus.status
    backend_selected = "openhands"
    cases = $results
    secrets_check = "passed"
    store = $storePath
  } | ConvertTo-Json -Depth 20
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
}
