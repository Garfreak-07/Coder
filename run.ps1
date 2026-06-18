param(
    [switch]$Setup
)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

function Read-Default {
    param(
        [string]$Prompt,
        [string]$Default = ""
    )

    if ($Default) {
        $value = Read-Host "$Prompt [$Default]"
        if ([string]::IsNullOrWhiteSpace($value)) {
            return $Default
        }
        return $value
    }

    return Read-Host $Prompt
}

function Split-List {
    param([string]$Value)

    if ([string]::IsNullOrWhiteSpace($Value)) {
        return @()
    }

    return $Value -split "[,;]" |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ }
}

function Ensure-Venv {
    $python = Join-Path $PSScriptRoot ".venv\Scripts\python.exe"

    if (-not (Test-Path $python)) {
        $answer = Read-Default "No .venv found. Create it and install dependencies? y/n" "y"
        if ($answer.ToLowerInvariant() -notin @("y", "yes")) {
            throw "Cannot continue without a local Python environment."
        }

        python -m venv .venv
        & $python -m pip install --upgrade pip
        & $python -m pip install -e .
        return $python
    }

    try {
        & $python -c "import coder_graph" | Out-Null
    }
    catch {
        $answer = Read-Default "Dependencies are not installed in .venv. Install now? y/n" "y"
        if ($answer.ToLowerInvariant() -notin @("y", "yes")) {
            throw "Cannot continue without installed dependencies."
        }

        & $python -m pip install -e .
    }

    return $python
}

Write-Host ""
Write-Host "Coder - safe LangGraph coding workflow" -ForegroundColor Cyan
Write-Host "Default mode is safe: it plans and reviews, but does not modify code." -ForegroundColor DarkGray
Write-Host ""

$python = Ensure-Venv

if ($Setup) {
    Write-Host "Setup complete." -ForegroundColor Green
    exit 0
}

$repo = Read-Default "Target project path" "."
$request = Read-Default "What do you want Coder to do?" "Analyze this project and propose a safe improvement plan."
$scopeText = Read-Default "Limit scope inside repo, comma-separated. Leave empty for whole repo" ""
$referenceText = Read-Default "Reference project paths, comma-separated. Leave empty for none" ""
$check = Read-Default "Check command to run in target repo. Leave empty to skip" ""

Write-Host ""
Write-Host "Optional model override. Leave empty to use .env / environment variables." -ForegroundColor DarkGray
$provider = Read-Default "Provider, e.g. openai/deepseek/kimi/qwen/ollama" ""
$model = Read-Default "Model name" ""
$baseUrl = Read-Default "OpenAI-compatible base URL" ""

$approveText = Read-Default "Approve dry-run execution after planning? y/n" "n"

$argsList = @("-m", "coder_graph.cli", "--repo", $repo, "--request", $request)

foreach ($scope in (Split-List $scopeText)) {
    $argsList += @("--scope", $scope)
}

foreach ($reference in (Split-List $referenceText)) {
    $argsList += @("--reference", $reference)
}

if (-not [string]::IsNullOrWhiteSpace($check)) {
    $argsList += @("--check", $check)
}

if (-not [string]::IsNullOrWhiteSpace($provider)) {
    $argsList += @("--provider", $provider)
}

if (-not [string]::IsNullOrWhiteSpace($model)) {
    $argsList += @("--model", $model)
}

if (-not [string]::IsNullOrWhiteSpace($baseUrl)) {
    $argsList += @("--base-url", $baseUrl)
}

if ($approveText.ToLowerInvariant() -in @("y", "yes")) {
    $argsList += "--approve"
}

Write-Host ""
Write-Host "Running workflow..." -ForegroundColor Cyan
& $python @argsList
