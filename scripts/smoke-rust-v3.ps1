param(
  [string]$HostName = "127.0.0.1",
  [int]$Port = 8876,
  [string]$Store = ".coder-rust-smoke",
  [switch]$LiveProvider,
  [string]$Provider = "deepseek",
  [string]$Model = "deepseek-chat",
  [string]$BaseUrl = "https://api.deepseek.com",
  [string]$ProxyUrl = ""
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location -LiteralPath $repoRoot

$storePath = if ([System.IO.Path]::IsPathRooted($Store)) {
  $Store
} else {
  Join-Path $repoRoot $Store
}
$outLog = Join-Path $storePath "server.out.log"
$errLog = Join-Path $storePath "server.err.log"
New-Item -ItemType Directory -Force -Path $storePath | Out-Null

function Assert-Smoke {
  param(
    [bool]$Condition,
    [string]$Message
  )
  if (-not $Condition) {
    throw $Message
  }
}

function Invoke-Git {
  param(
    [string]$Repo,
    [string[]]$GitArgs
  )
  & git -C $Repo @GitArgs | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "git $($GitArgs -join ' ') failed in $Repo"
  }
}

function New-SmokeRepo {
  param([string]$Parent)

  $path = Join-Path $Parent ("scenario-repo-" + [guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Force -Path $path | Out-Null
  [System.IO.File]::WriteAllText((Join-Path $path "README.md"), "# Smoke repo`nbase`n")
  Invoke-Git -Repo $path -GitArgs @("init")
  Invoke-Git -Repo $path -GitArgs @("config", "core.autocrlf", "false")
  Invoke-Git -Repo $path -GitArgs @("config", "core.safecrlf", "false")
  Invoke-Git -Repo $path -GitArgs @("config", "user.email", "coder@example.test")
  Invoke-Git -Repo $path -GitArgs @("config", "user.name", "Coder Smoke")
  Invoke-Git -Repo $path -GitArgs @("add", "README.md")
  Invoke-Git -Repo $path -GitArgs @("commit", "-m", "base")
  $path
}

function New-SmokeConfig {
  param(
    [string]$Provider,
    [string]$Model
  )

  @{
    version = 1
    models = @{
      default = @{
        provider = $Provider
        model = $Model
        base_url_env = "LLM_BASE_URL"
        api_key_env = "LLM_API_KEY"
      }
    }
    agents = @{
      planner = @{
        role = "planner"
        model = "default"
        system = "Plan scoped repository work, keep execution behind Start Work, and summarize evidence without private reasoning."
        memory = @{
          read = @("user", "project", "run", "repo_facts", "knowledge_hints")
          write = @("run")
        }
        output_contract = "planner_conversation"
      }
      executor = @{
        role = "executor"
        model = "default"
        system = "Inspect the requested repository scope with native read-only tools and report evidence."
        memory = @{
          read = @("workflow", "run")
          write = @("run")
        }
        output_contract = "execution_result"
      }
    }
    harnesses = @{
      "planner-conversation" = @{
        backend = "planner-model"
        tools = @("memory_read", "repo_search", "read_file", "git_diff")
        permissions = @{
          read_files = "allow"
          write_files = "deny"
          run_commands = "deny"
          network = "deny"
          secrets = "deny"
          publish_external = "deny"
          git_commit = "deny"
          git_push = "deny"
          deploy = "deny"
        }
        memory = @{
          read = @("user", "project", "run", "repo_facts", "knowledge_hints")
          write = @("run")
        }
        verification = @{
          require_evidence = $false
        }
      }
      "review-only" = @{
        backend = "native-rust"
        tools = @("repo_find_files", "read_file", "git_diff")
        permissions = @{
          read_files = "allow"
          write_files = "deny"
          run_commands = "deny"
          network = "deny"
          secrets = "deny"
          publish_external = "deny"
          git_commit = "deny"
          git_push = "deny"
          deploy = "deny"
        }
        memory = @{
          read = @("workflow", "run")
          write = @("run")
        }
      }
    }
    workflows = @{
      "planner-led" = @{
        name = "Planner to Review Smoke"
        max_rounds = 1
        nodes = @(
          @{ id = "planner"; agent = "planner"; harness = "planner-conversation" },
          @{ id = "executor"; agent = "executor"; harness = "review-only" }
        )
        edges = @(
          @{ from = "planner"; to = "executor"; on = "ready" }
        )
        stop = @{
          on_status = @("completed", "blocked", "failed")
          final_report_agent = "planner"
        }
      }
    }
  }
}

# Some sandboxed Windows environments expose both Path and PATH. Start-Process
# rejects duplicate environment keys, so keep Path and drop the duplicate PATH
# from this script process before starting the server.
$processEnv = [Environment]::GetEnvironmentVariables("Process")
if ($processEnv.Contains("Path") -and $processEnv.Contains("PATH")) {
  [Environment]::SetEnvironmentVariable("PATH", $null, "Process")
}
[Environment]::SetEnvironmentVariable("CODER_RUNTIME_CACHE_DIR", (Join-Path $repoRoot "tmp\coder-runtime-cache"), "Process")

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
  foreach ($attempt in 1..60) {
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
  $normalizedProvider = $Provider.Trim().ToLowerInvariant()
  $validationMode = if ($LiveProvider) { "product_validation" } else { "plumbing" }
  $mockMode = -not $LiveProvider
  $baseUrls = @{}
  $baseUrls[$normalizedProvider] = $BaseUrl
  $proxyUrls = @{}
  if (-not [string]::IsNullOrWhiteSpace($ProxyUrl)) {
    $proxyUrls[$normalizedProvider] = $ProxyUrl
  }

  $settingsBody = @{
    default_provider = $normalizedProvider
    default_model = $Model
    base_urls = $baseUrls
    proxy_urls = $proxyUrls
    mock_mode = $mockMode
  } | ConvertTo-Json -Depth 20
  $settings = Invoke-RestMethod -Method Post -Uri "$base/api/v3/providers/settings" -Headers $jsonHeaders -Body $settingsBody
  Assert-Smoke ($settings.settings.default_provider -eq $normalizedProvider) "Provider settings did not save the requested provider."
  Assert-Smoke ($settings.settings.mock_mode -eq $mockMode) "Provider settings did not save the requested mock mode."

  $providerTestBody = @{
    provider = $normalizedProvider
    mock = $mockMode
  } | ConvertTo-Json -Depth 10
  $providerTest = Invoke-RestMethod -Method Post -Uri "$base/api/v3/providers/test" -Headers $jsonHeaders -Body $providerTestBody
  Assert-Smoke ($providerTest.test.ok -eq $true) "Provider test failed: $($providerTest.test.message)"
  if ($mockMode) {
    Assert-Smoke ($providerTest.test.mode -eq "mock") "Mock smoke must not make a live provider request."
  } else {
    Assert-Smoke ($providerTest.test.mode -eq "live") "Live smoke did not validate the live provider path."
  }

  $scenarioRepo = New-SmokeRepo -Parent $storePath
  $config = New-SmokeConfig -Provider $normalizedProvider -Model $Model

  $createBody = @{
    workflow_id = "planner-led"
    planner_agent_id = "planner"
    config = $config
    mode = "discuss"
  } | ConvertTo-Json -Depth 60
  $sessionResponse = Invoke-RestMethod -Method Post -Uri "$base/api/v3/planner-chat/sessions" -Headers $jsonHeaders -Body $createBody
  $sessionId = $sessionResponse.session.session_id
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($sessionId)) "Planner session creation did not return a session_id."

  function Send-PlannerTurn {
    param(
      [string]$Message,
      [bool]$Confirmed,
      [string]$Mode = "discuss"
    )
    $body = @{
      message = $Message
      confirmed = $Confirmed
      mode = $Mode
      planner_agent_id = "planner"
      config = $config
    } | ConvertTo-Json -Depth 60
    Invoke-RestMethod -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/turn" -Headers $jsonHeaders -Body $body
  }

  $firstTurn = Send-PlannerTurn -Message "Plan repository work for README.md. Acceptance: final report includes evidence." -Confirmed $false
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($firstTurn.assistant_message)) "First Planner turn did not return an assistant message."
  Assert-Smoke ($firstTurn.should_start_workflow -eq $false) "First Planner turn unexpectedly started work."

  $secondTurn = Send-PlannerTurn -Message "Confirm scope README.md and keep validation offline. Acceptance: timeline, final report, Review Changes, and Undo are exercised." -Confirmed $true -Mode "work"
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($secondTurn.assistant_message)) "Second Planner turn did not return an assistant message."
  Assert-Smoke ($secondTurn.should_start_workflow -eq $false) "Second Planner turn unexpectedly started work."
  Assert-Smoke (@($secondTurn.session.turns).Count -ge 4) "Planner session did not retain two user/assistant turns."
  Assert-Smoke ($secondTurn.ready -eq $true) "Planner did not mark the scoped plan ready for Start Work."

  $startBody = @{
    repo = $scenarioRepo
    workflow_id = "planner-led"
    planner_agent_id = "planner"
    config = $config
    scopes = @("README.md")
  } | ConvertTo-Json -Depth 60
  $startWork = Invoke-RestMethod -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/start-work" -Headers $jsonHeaders -Body $startBody
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($startWork.run_id)) "Start Work did not return a run_id: $($startWork | ConvertTo-Json -Depth 10)"
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($startWork.timeline_url)) "Start Work did not return a timeline_url."

  $events = Invoke-RestMethod -Method Get -Uri "$base$($startWork.events_url)"
  Assert-Smoke (@($events.events).Count -ge 1) "Run events were not visible."

  $timeline = Invoke-RestMethod -Method Get -Uri "$base$($startWork.timeline_url)"
  $timelineItems = @($timeline.items)
  Assert-Smoke ($timelineItems.Count -ge 1) "Run timeline was empty."
  Assert-Smoke (($timelineItems | Where-Object { $_.type -eq "final_summary" } | Select-Object -First 1) -ne $null) "Run timeline did not include a final summary."

  $report = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($startWork.run_id)/report/preview"
  Assert-Smoke ($report.report.status -eq "completed") "Report preview did not complete."

  $artifact = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($startWork.run_id)/artifacts/final-report.json"
  Assert-Smoke ($artifact.artifact_name -eq "final-report.json") "Final report artifact fetch failed."

  $readmePath = Join-Path $scenarioRepo "README.md"
  [System.IO.File]::WriteAllText($readmePath, "# Smoke repo`nchanged by review smoke`n")

  $review = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($startWork.run_id)/changes"
  $reviewChanges = @($review.changes)
  Assert-Smoke ($reviewChanges.Count -ge 1) "Review Changes returned no change sets after a controlled README.md change."
  $changeSetId = $reviewChanges[0].change_set_id
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($changeSetId)) "Review Changes did not return a change_set_id."

  $diff = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($startWork.run_id)/changes/$changeSetId/diff"
  Assert-Smoke ($diff.diff.Contains("README.md")) "Change diff did not mention README.md."
  Assert-Smoke ($diff.diff.Contains("changed by review smoke")) "Change diff did not include the controlled README.md edit."

  $undo = Invoke-RestMethod -Method Post -Uri "$base/api/v3/runs/$($startWork.run_id)/changes/$changeSetId/undo"
  Assert-Smoke ($undo.status -eq "undone") "Undo did not report undone."
  $readmeAfterUndo = [System.IO.File]::ReadAllText($readmePath).Replace("`r`n", "`n")
  Assert-Smoke ($readmeAfterUndo -eq "# Smoke repo`nbase`n") "Undo did not restore README.md to the committed content."

  $reviewAfterUndo = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($startWork.run_id)/changes"
  Assert-Smoke (@($reviewAfterUndo.changes).Count -eq 0) "Review Changes still reported changes after undo."

  [pscustomobject]@{
    status = "ok"
    validation = $validationMode
    provider = $normalizedProvider
    provider_test = $providerTest.test.mode
    mock_mode = $mockMode
    session_id = $sessionId
    turns = @($secondTurn.session.turns).Count
    run_id = $startWork.run_id
    start_work_status = $startWork.status
    events = @($events.events).Count
    timeline_items = $timelineItems.Count
    report_status = $report.report.status
    artifact = $artifact.artifact_name
    review_changes = $reviewChanges.Count
    undo_status = $undo.status
    review_changes_after_undo = @($reviewAfterUndo.changes).Count
  } | ConvertTo-Json -Depth 10
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
}
