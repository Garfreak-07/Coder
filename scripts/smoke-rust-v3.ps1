param(
  [string]$HostName = "127.0.0.1",
  [int]$Port = 8876,
  [string]$Store = ".coder-rust-smoke"
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

# Some sandboxed Windows environments expose both Path and PATH. Start-Process
# rejects duplicate environment keys, so keep Path and drop the duplicate PATH
# from this script process before starting the server.
$processEnv = [Environment]::GetEnvironmentVariables("Process")
if ($processEnv.Contains("Path") -and $processEnv.Contains("PATH")) {
  [Environment]::SetEnvironmentVariable("PATH", $null, "Process")
}
[Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", (Join-Path $repoRoot ".tmp\cargo-target"), "Process")

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

  $config = @{
    version = 1
    models = @{
      default = @{
        provider = "openai-compatible"
        model = "mock"
        api_key_env = "LLM_API_KEY"
      }
    }
    agents = @{
      planner = @{
        role = "planner"
        model = "default"
        system = "Plan and decide readiness."
        output_contract = "planner_order"
      }
      executor = @{
        role = "executor"
        model = "default"
        system = "Execute approved work and report evidence."
        output_contract = "execution_result"
      }
    }
    harnesses = @{
      review = @{
        backend = "native-rust"
        tools = @("repo_search", "read_file", "git_diff")
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
      }
    }
    workflows = @{
      smoke = @{
        name = "Rust v3 smoke"
        max_rounds = 1
        nodes = @(
          @{ id = "planner"; agent = "planner"; harness = "review" }
        )
        edges = @()
        stop = @{
          on_status = @("completed", "blocked", "failed")
          final_report_agent = "planner"
        }
      }
    }
  }

  $jsonHeaders = @{ "Content-Type" = "application/json" }
  $saveBody = @{ workflow_id = "smoke"; workflow = $config.workflows.smoke } | ConvertTo-Json -Depth 20
  $save = Invoke-RestMethod -Method Post -Uri "$base/api/v3/library/workflows" -Headers $jsonHeaders -Body $saveBody
  if ($save.saved -ne $true) {
    throw "Workflow save failed."
  }

  $loaded = Invoke-RestMethod -Method Get -Uri "$base/api/v3/library/workflows/smoke"
  if ($loaded.workflow_id -ne "smoke") {
    throw "Workflow load failed."
  }

  $runBody = @{
    config = $config
    workflow_id = "smoke"
    task = "smoke test Rust v3 product path"
  } | ConvertTo-Json -Depth 30

  $preview = Invoke-RestMethod -Method Post -Uri "$base/api/v3/runs/preview" -Headers $jsonHeaders -Body $runBody
  if ($preview.status -ne "ready") {
    throw "Run preview did not report ready: $($preview | ConvertTo-Json -Depth 10)"
  }

  $run = Invoke-RestMethod -Method Post -Uri "$base/api/v3/runs/mock" -Headers $jsonHeaders -Body $runBody
  if (-not $run.run_id) {
    throw "Mock run did not return a run_id."
  }

  $events = Invoke-RestMethod -Method Get -Uri "$base$($run.events_url)"
  if (-not $events.events -or $events.events.Count -lt 1) {
    throw "Run events were not visible."
  }

  $report = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($run.run_id)/report/preview"
  if ($report.report.status -ne "completed") {
    throw "Report preview did not complete."
  }

  $artifact = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($run.run_id)/artifacts/final-report.json"
  if ($artifact.artifact_name -ne "final-report.json") {
    throw "Final report artifact fetch failed."
  }

  $repoEvidence = Invoke-RestMethod -Method Get -Uri "$base/api/v3/runs/$($run.run_id)/repo-evidence"

  [pscustomobject]@{
    status = "ok"
    health = $health.status
    run_id = $run.run_id
    events = $events.events.Count
    report_status = $report.report.status
    artifact = $artifact.artifact_name
    repo_evidence_count = $repoEvidence.items.Count
  } | ConvertTo-Json -Depth 10
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
}
