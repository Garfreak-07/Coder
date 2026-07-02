param(
  [string]$HostName = "127.0.0.1",
  [int]$Port = 8879,
  [string]$WorkRoot = "F:\ccc",
  [string]$ProjectName = "coder-snake-game",
  [string]$Store = ".tmp\live-snake-game-smoke\store",
  [string]$Provider = "deepseek",
  [string]$BaseUrl = "",
  [string]$Model = "",
  [string]$ProviderProxyUrl = "",
  [string]$ApiKeyEnv = "",
  [ValidateSet("managed", "external")]
  [string]$OpenHandsRuntimeMode = "managed",
  [string]$OpenHandsServerUrl = "",
  [string]$OpenHandsSessionApiKey = "",
  [string]$OpenHandsSessionApiKeyEnv = "OPENHANDS_SESSION_API_KEY",
  [ValidateSet("auto", "legacy-sdk", "default")]
  [string]$OpenHandsApiProfile = "auto",
  [switch]$Live,
  [switch]$LoadLocalEnv,
  [switch]$SkipIfMissingLiveConfig,
  [switch]$Force,
  [switch]$UndoAfterReview
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location -LiteralPath $repoRoot

function Stop-Or-Skip {
  param([string]$Reason)
  if ($SkipIfMissingLiveConfig) {
    [pscustomobject]@{
      status = "skipped"
      reason = $Reason
    } | ConvertTo-Json -Depth 6
    exit 0
  }
  throw $Reason
}

function Assert-Smoke {
  param(
    [bool]$Condition,
    [string]$Message
  )
  if (-not $Condition) {
    throw $Message
  }
}

function ConvertTo-JsonBody {
  param([object]$Value)
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
  for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
    try {
      if ($PSBoundParameters.ContainsKey("Body")) {
        return Invoke-RestMethod -Method $Method -Uri $Uri -Headers $Headers -Body $Body
      }
      return Invoke-RestMethod -Method $Method -Uri $Uri -Headers $Headers
    } catch {
      if ($attempt -ge $Attempts) {
        throw
      }
      Start-Sleep -Seconds $attempt
    }
  }
}

function Invoke-Native {
  param(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$WorkingDirectory = $repoRoot
  )
  & $FilePath @Arguments | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "$FilePath $($Arguments -join ' ') failed in $WorkingDirectory"
  }
}

function Invoke-NativeCapture {
  param(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$WorkingDirectory = $repoRoot
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

function Resolve-PlaywrightNodeModules {
  $candidates = @(
    (Join-Path $repoRoot "node_modules"),
    (Join-Path $repoRoot "frontend\node_modules"),
    (Resolve-UnderRepo -PathValue ".tmp\playwright-smoke\node_modules")
  )
  foreach ($nodeModules in $candidates) {
    if (Test-Path -LiteralPath (Join-Path $nodeModules "playwright\package.json")) {
      return $nodeModules
    }
  }

  $installRoot = Resolve-UnderRepo -PathValue ".tmp\playwright-smoke"
  New-Item -ItemType Directory -Force -Path $installRoot | Out-Null
  $npm = Get-Command npm.cmd -ErrorAction SilentlyContinue
  if ($null -eq $npm) {
    $npm = Get-Command npm -ErrorAction SilentlyContinue
  }
  if ($null -eq $npm) {
    throw "Browser gameplay validation requires npm so Playwright can be bootstrapped."
  }
  Invoke-Native -FilePath $npm.Source -Arguments @("--prefix", $installRoot, "install", "--no-audit", "--fund=false", "playwright@1.57.0")
  $nodeModules = Join-Path $installRoot "node_modules"
  if (-not (Test-Path -LiteralPath (Join-Path $nodeModules "playwright\package.json"))) {
    throw "Playwright bootstrap completed without installing playwright."
  }
  $nodeModules
}

function Invoke-SnakeBrowserValidation {
  param([string]$IndexPath)

  $node = (Get-Command node).Source
  $nodeModules = Resolve-PlaywrightNodeModules
  $scriptPath = Resolve-UnderRepo -PathValue ".tmp\live-snake-game-smoke\browser-gameplay-validation.mjs"
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $scriptPath) | Out-Null
  Set-Content -LiteralPath $scriptPath -Encoding UTF8 -Value @'
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";

const [, , indexPath, nodeModules] = process.argv;
const require = createRequire(import.meta.url);
const { chromium } = require(path.join(nodeModules, "playwright"));

function fail(message, detail = {}) {
  console.error(JSON.stringify({ status: "failed", message, ...detail }, null, 2));
  process.exit(1);
}

async function launchBrowser() {
  const attempts = [
    { name: "bundled", options: {} },
    { name: "msedge", options: { channel: "msedge" } },
    { name: "chrome", options: { channel: "chrome" } },
  ];
  const errors = [];
  for (const attempt of attempts) {
    try {
      const browser = await chromium.launch({ headless: true, ...attempt.options });
      return { browser, browserName: attempt.name };
    } catch (error) {
      errors.push(`${attempt.name}: ${error.message}`);
    }
  }
  fail("Could not launch a Playwright Chromium-compatible browser.", { errors });
}

function canvasDigest(dataUrl) {
  return createHash("sha256").update(dataUrl).digest("hex");
}

async function readCanvas(page) {
  return canvasDigest(await page.$eval("canvas", (canvas) => canvas.toDataURL("image/png")));
}

async function readScore(page) {
  return page.evaluate(() => {
    const score = document.querySelector("#score");
    return score ? score.textContent?.trim() ?? "" : "";
  });
}

function visibleScoreIsZero(scoreText) {
  const trimmed = String(scoreText ?? "").trim();
  if (trimmed === "") return true;
  const matches = trimmed.match(/-?\d+/g);
  if (!matches || matches.length === 0) return false;
  return Number(matches[matches.length - 1]) === 0;
}

async function readState(page) {
  return page.evaluate(() => {
    const raw = window.__snakeTestState;
    let value = null;
    if (typeof raw === "function") {
      value = raw();
    } else if (raw && typeof raw.snapshot === "function") {
      value = raw.snapshot();
    } else if (raw && typeof raw === "object") {
      value = raw;
    }
    if (!value) return null;
    return JSON.parse(JSON.stringify(value));
  });
}

function stateGameOver(state) {
  if (!state) return false;
  const status = String(state.status ?? state.state ?? "").toLowerCase();
  return state.gameOver === true || status === "game_over" || status === "game over" || status === "ended";
}

function headOf(state) {
  if (!state) return null;
  return state.head ?? state.snakeHead ?? (Array.isArray(state.snake) ? state.snake[0] : null);
}

function directionOf(state) {
  if (!state) return null;
  return state.direction ?? state.dir ?? state.velocity ?? null;
}

function numberValue(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function progressed(before, after) {
  if (!before || !after) return false;
  const beforeTick = numberValue(before.tick ?? before.ticks ?? before.frame);
  const afterTick = numberValue(after.tick ?? after.ticks ?? after.frame);
  if (beforeTick !== null && afterTick !== null && afterTick > beforeTick) return true;

  const beforeHead = headOf(before);
  const afterHead = headOf(after);
  if (beforeHead && afterHead && (beforeHead.x !== afterHead.x || beforeHead.y !== afterHead.y)) {
    return true;
  }
  return false;
}

async function assertNotGameOver(page, label) {
  const state = await readState(page);
  if (stateGameOver(state)) {
    fail(`Game Over appeared during ${label}.`, { state });
  }
  return state;
}

async function waitForMovement(page, label, previousState, previousCanvas) {
  let lastState = previousState;
  let lastCanvas = previousCanvas;
  for (let attempt = 0; attempt < 10; attempt++) {
    await page.waitForTimeout(150);
    const state = await assertNotGameOver(page, label);
    const canvas = await readCanvas(page);
    const changedFromStart = progressed(previousState, state) || canvas !== previousCanvas;
    const changedSinceLast = progressed(lastState, state) || canvas !== lastCanvas;
    if (changedFromStart && changedSinceLast) {
      return { state, canvas };
    }
    lastState = state;
    lastCanvas = canvas;
  }
  fail(`Canvas or test state did not keep changing during ${label}.`, {
    previousState,
    lastState,
    previousCanvas,
    lastCanvas,
  });
}

const { browser, browserName } = await launchBrowser();
const consoleMessages = [];
try {
  const page = await browser.newPage({ viewport: { width: 900, height: 700 } });
  page.on("console", (message) => {
    if (["error", "warning"].includes(message.type())) {
      consoleMessages.push(`${message.type()}: ${message.text()}`);
    }
  });
  page.on("pageerror", (error) => {
    consoleMessages.push(`pageerror: ${error.message}`);
  });

  await page.goto(pathToFileURL(indexPath).href, { waitUntil: "load" });
  const title = await page.title();
  if (!/snake/i.test(title)) {
    fail("Browser did not open the Snake page.", { title });
  }
  await page.waitForSelector("canvas", { state: "visible", timeout: 10000 });
  const canvasBox = await page.locator("canvas").boundingBox();
  if (!canvasBox || canvasBox.width <= 0 || canvasBox.height <= 0) {
    fail("Canvas was not visible.");
  }

  const initialState = await assertNotGameOver(page, "initial load");
  const initialCanvas = await readCanvas(page);
  await page.waitForTimeout(350);
  const afterLoadState = await assertNotGameOver(page, "post-load idle");
  const afterLoadCanvas = await readCanvas(page);

  await page.keyboard.press("ArrowRight");
  const afterRight = await waitForMovement(page, "ArrowRight movement", afterLoadState, afterLoadCanvas);

  await page.keyboard.press("ArrowLeft");
  await page.waitForTimeout(220);
  const afterReverseState = await assertNotGameOver(page, "opposite-direction guard");
  const reverseDirection = directionOf(afterReverseState);
  if (reverseDirection && Number(reverseDirection.x) < 0) {
    fail("Opposite-direction reversal was not prevented.", { state: afterReverseState });
  }

  await page.keyboard.press("ArrowDown");
  const afterDown = await waitForMovement(page, "second valid direction", afterReverseState ?? afterRight.state, afterRight.canvas);

  const restartButton = page.locator("#restart-btn, [data-testid='restart-button']").first();
  if (await restartButton.count() === 0) {
    fail("Restart button was missing. Expected #restart-btn or data-testid='restart-button'.");
  }
  await restartButton.click({ timeout: 5000 });
  await page.waitForTimeout(250);
  const afterRestartState = await assertNotGameOver(page, "restart");
  const restartScore = await readScore(page);
  if (!visibleScoreIsZero(restartScore)) {
    fail("Restart did not reset the visible score.", { restartScore });
  }
  const afterRestartCanvas = await readCanvas(page);
  await page.keyboard.press("ArrowRight");
  await waitForMovement(page, "post-restart movement", afterRestartState, afterRestartCanvas);

  if (consoleMessages.some((message) => message.toLowerCase().includes("error"))) {
    fail("Browser console reported errors.", { consoleMessages });
  }

  console.log(JSON.stringify({
    status: "ok",
    browser: browserName,
    title,
    state_hook: Boolean(initialState || afterLoadState || afterRight.state || afterDown.state),
    checks: {
      page_opened: true,
      canvas_visible: true,
      no_immediate_game_over: true,
      direction_key_moves_game: true,
      opposite_reversal_prevented: true,
      second_direction_continues: true,
      restart_works: true,
      canvas_changed: initialCanvas !== afterDown.canvas || initialCanvas !== afterRight.canvas,
    },
    console_messages: consoleMessages,
  }));
} finally {
  await browser.close();
}
'@

  $result = Invoke-NativeCapture -FilePath $node -Arguments @($scriptPath, $IndexPath, $nodeModules)
  if ($result.ExitCode -ne 0) {
    throw "Browser gameplay validation failed: $($result.Output -join "`n")"
  }
  $jsonText = ($result.Output | Where-Object { -not [string]::IsNullOrWhiteSpace([string]$_) }) -join "`n"
  if ([string]::IsNullOrWhiteSpace($jsonText)) {
    throw "Browser gameplay validation produced no JSON output."
  }
  $jsonText | ConvertFrom-Json
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
  $fullPath = [System.IO.Path]::GetFullPath($path)
  $repoFullPath = [System.IO.Path]::GetFullPath($repoRoot).TrimEnd('\', '/')
  if (-not $fullPath.StartsWith($repoFullPath, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Path must stay under repository root: $PathValue"
  }
  $fullPath
}

function Resolve-AgentCanvasBin {
  $existing = Get-Command agent-canvas -ErrorAction SilentlyContinue
  if ($null -ne $existing) {
    return $null
  }

  $cachePath = Resolve-UnderRepo -PathValue ".tmp\npm-cache"
  $candidate = Get-ChildItem -Path (Join-Path $cachePath "_npx") -Recurse -Filter "agent-canvas.mjs" -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -like "*node_modules*@openhands*agent-canvas*bin*" } |
    Select-Object -First 1
  if ($null -eq $candidate) {
    $npx = (Get-Command npx.cmd -ErrorAction SilentlyContinue)
    if ($null -eq $npx) {
      $npx = Get-Command npx -ErrorAction SilentlyContinue
    }
    if ($null -eq $npx) {
      throw "Managed OpenHands runtime needs bundled agent-canvas or npx for this smoke."
    }
    Invoke-Native -FilePath $npx.Source -Arguments @("--cache", $cachePath, "--yes", "@openhands/agent-canvas", "--info")
    $candidate = Get-ChildItem -Path (Join-Path $cachePath "_npx") -Recurse -Filter "agent-canvas.mjs" -ErrorAction SilentlyContinue |
      Where-Object { $_.FullName -like "*node_modules*@openhands*agent-canvas*bin*" } |
      Select-Object -First 1
  }
  if ($null -eq $candidate) {
    throw "Managed OpenHands runtime could not locate agent-canvas after npm bootstrap."
  }
  $candidate.FullName
}

function Resolve-SnakeProjectPath {
  param(
    [string]$Root,
    [string]$Name,
    [bool]$UseForce
  )
  if ([string]::IsNullOrWhiteSpace($Name)) {
    throw "ProjectName must not be empty."
  }
  if ($Name.IndexOfAny([System.IO.Path]::GetInvalidFileNameChars()) -ge 0 -or $Name.Contains('\') -or $Name.Contains('/')) {
    throw "ProjectName must be a single directory name, not a path: $Name"
  }
  $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
  $rootPath = [System.IO.Path]::GetPathRoot($rootFull).TrimEnd('\', '/')
  if ($rootFull.Equals($rootPath, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "WorkRoot must not be a drive root."
  }
  $target = [System.IO.Path]::GetFullPath((Join-Path $rootFull $Name))
  $rootWithSeparator = $rootFull + [System.IO.Path]::DirectorySeparatorChar
  if (-not $target.StartsWith($rootWithSeparator, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Target project path escapes WorkRoot."
  }
  if ((Test-Path -LiteralPath $target) -and -not $UseForce) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $target = [System.IO.Path]::GetFullPath((Join-Path $rootFull "$Name-$stamp"))
  }
  [pscustomobject]@{
    WorkRoot = $rootFull
    ProjectPath = $target
  }
}

function Initialize-EmptyGitRepo {
  param([string]$Path)
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
  $git = (Get-Command git).Source
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "init")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "config", "core.autocrlf", "false")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "config", "core.safecrlf", "false")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "config", "user.email", "coder-snake-smoke@example.invalid")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "config", "user.name", "Coder Snake Smoke")
  Invoke-Native -FilePath $git -Arguments @("-C", $Path, "commit", "--allow-empty", "-m", "initial empty snake smoke fixture")
}

function Get-SerializedArtifactsText {
  param(
    [object[]]$Objects,
    [string[]]$Files
  )
  $parts = @()
  foreach ($object in $Objects) {
    if ($null -ne $object) {
      $parts += ($object | ConvertTo-Json -Depth 100)
    }
  }
  foreach ($file in $Files) {
    if (Test-Path -LiteralPath $file) {
      $parts += [System.IO.File]::ReadAllText($file)
    }
  }
  $parts -join "`n"
}

function Assert-NoSecretLeak {
  param(
    [string]$Text,
    [object[]]$Secrets
  )
  foreach ($secret in $Secrets) {
    if ($null -eq $secret) {
      continue
    }
    $value = if ($secret.PSObject.Properties.Name -contains "Value") {
      [string]$secret.Value
    } else {
      [string]$secret
    }
    if ($value.Trim().Length -ge 4 -and $Text.Contains($value)) {
      throw "Secret value from live snake smoke appeared in serialized artifacts."
    }
  }
}

function Count-Words {
  param([string]$Text)
  if ([string]::IsNullOrWhiteSpace($Text)) {
    return 0
  }
  @($Text -split '\s+' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }).Count
}

function Format-StartWorkFailure {
  param(
    [object]$StartWork,
    [string]$StorePath
  )
  $status = if ($null -ne $StartWork -and $StartWork.PSObject.Properties.Name -contains "status") {
    [string]$StartWork.status
  } else {
    "unknown"
  }
  $runId = if ($null -ne $StartWork -and $StartWork.PSObject.Properties.Name -contains "run_id" -and -not [string]::IsNullOrWhiteSpace([string]$StartWork.run_id)) {
    [string]$StartWork.run_id
  } else {
    ""
  }
  $eventsPath = if (-not [string]::IsNullOrWhiteSpace($runId)) {
    Join-Path $StorePath "runs\$runId\events.jsonl"
  } else {
    ""
  }
  $suffix = if (-not [string]::IsNullOrWhiteSpace($eventsPath)) {
    " Inspect $eventsPath."
  } else {
    ""
  }
  "Start Work did not complete: status=$status run_id=$runId.$suffix"
}

if ($LoadLocalEnv) {
  $localEnvPath = Join-Path $repoRoot ".local-env.ps1"
  if (-not (Test-Path -LiteralPath $localEnvPath)) {
    Stop-Or-Skip -Reason "Local env file not found: $localEnvPath"
  }
  . $localEnvPath
}

$liveFlag = [Environment]::GetEnvironmentVariable("CODER_SNAKE_LIVE_SMOKE", "Process")
if ($liveFlag -ne "1" -and -not $Live) {
  Stop-Or-Skip -Reason "Set CODER_SNAKE_LIVE_SMOKE=1 or pass -Live to run the live Snake smoke."
}

$normalizedProvider = $Provider.Trim().ToLowerInvariant()
if ([string]::IsNullOrWhiteSpace($normalizedProvider)) {
  $normalizedProvider = "deepseek"
}

$apiKeyCandidates = @()
if (-not [string]::IsNullOrWhiteSpace($ApiKeyEnv)) {
  $apiKeyCandidates += $ApiKeyEnv
}
switch ($normalizedProvider) {
  "deepseek" { $apiKeyCandidates += @("DEEPSEEK_API_KEY", "LLM_API_KEY") }
  "openai" { $apiKeyCandidates += @("OPENAI_API_KEY", "LLM_API_KEY") }
  default { $apiKeyCandidates += @("LLM_API_KEY", "DEEPSEEK_API_KEY", "CODER_API_KEY") }
}
$apiKey = Get-FirstEnvValue -Names $apiKeyCandidates
if ($null -eq $apiKey) {
  Stop-Or-Skip -Reason "No live provider API key found in: $($apiKeyCandidates -join ', ')"
}

if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
  $BaseUrl = [Environment]::GetEnvironmentVariable("LLM_BASE_URL", "Process")
}
if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
  $BaseUrl = if ($normalizedProvider -eq "openai") { "https://api.openai.com/v1" } else { "https://api.deepseek.com" }
}

if ([string]::IsNullOrWhiteSpace($Model)) {
  $Model = [Environment]::GetEnvironmentVariable("LLM_MODEL", "Process")
}
if ([string]::IsNullOrWhiteSpace($Model)) {
  $Model = if ($normalizedProvider -eq "openai") { "gpt-5.5" } else { "deepseek-v4-flash" }
}

if ([string]::IsNullOrWhiteSpace($ProviderProxyUrl)) {
  $ProviderProxyUrl = [Environment]::GetEnvironmentVariable("HTTPS_PROXY", "Process")
}
if ([string]::IsNullOrWhiteSpace($ProviderProxyUrl)) {
  $ProviderProxyUrl = [Environment]::GetEnvironmentVariable("HTTP_PROXY", "Process")
}

$openHandsSessionKey = $null
if ($OpenHandsRuntimeMode -eq "external") {
  if ([string]::IsNullOrWhiteSpace($OpenHandsServerUrl)) {
    $OpenHandsServerUrl = [Environment]::GetEnvironmentVariable("OPENHANDS_AGENT_SERVER_URL", "Process")
  }
  if ([string]::IsNullOrWhiteSpace($OpenHandsServerUrl)) {
    Stop-Or-Skip -Reason "External OpenHands mode requires OPENHANDS_AGENT_SERVER_URL or -OpenHandsServerUrl."
  }
  $OpenHandsServerUrl = $OpenHandsServerUrl.TrimEnd('/')

  if (-not [string]::IsNullOrWhiteSpace($OpenHandsSessionApiKey)) {
    $openHandsSessionKey = [pscustomobject]@{
      Name = "OpenHandsSessionApiKey"
      Value = $OpenHandsSessionApiKey
    }
  } else {
    $sessionKeyCandidates = @()
    if (-not [string]::IsNullOrWhiteSpace($OpenHandsSessionApiKeyEnv)) {
      $sessionKeyCandidates += $OpenHandsSessionApiKeyEnv
    }
    $sessionKeyCandidates += @("OPENHANDS_SESSION_API_KEY", "SESSION_API_KEY")
    $openHandsSessionKey = Get-FirstEnvValue -Names ($sessionKeyCandidates | Select-Object -Unique)
  }
  if ($OpenHandsApiProfile -eq "legacy-sdk" -and $null -eq $openHandsSessionKey) {
    Stop-Or-Skip -Reason "External OpenHands legacy-sdk profile requires a developer session token."
  }
}

$target = Resolve-SnakeProjectPath -Root $WorkRoot -Name $ProjectName -UseForce ([bool]$Force)
$workRootPath = $target.WorkRoot
$projectPath = $target.ProjectPath
$storePath = Resolve-UnderRepo -PathValue $Store
$outLog = Join-Path $storePath "server.out.log"
$errLog = Join-Path $storePath "server.err.log"

if (Test-Path -LiteralPath $storePath) {
  Remove-Item -LiteralPath $storePath -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $storePath | Out-Null
New-Item -ItemType Directory -Force -Path $workRootPath | Out-Null

if (Test-Path -LiteralPath $projectPath) {
  if (-not $Force) {
    throw "Target project unexpectedly exists after timestamp resolution: $projectPath"
  }
  $resolvedProject = [System.IO.Path]::GetFullPath($projectPath)
  $resolvedWorkRoot = [System.IO.Path]::GetFullPath($workRootPath).TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
  if (-not $resolvedProject.StartsWith($resolvedWorkRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to remove target outside WorkRoot: $projectPath"
  }
  Remove-Item -LiteralPath $projectPath -Recurse -Force
}
Initialize-EmptyGitRepo -Path $projectPath

$processEnv = [Environment]::GetEnvironmentVariables("Process")
if ($processEnv.Contains("Path") -and $processEnv.Contains("PATH")) {
  [Environment]::SetEnvironmentVariable("PATH", $null, "Process")
}
[Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", (Join-Path $repoRoot ".tmp\cargo-target"), "Process")
[Environment]::SetEnvironmentVariable("LLM_BASE_URL", $BaseUrl, "Process")
[Environment]::SetEnvironmentVariable("LLM_MODEL", $Model, "Process")
[Environment]::SetEnvironmentVariable("LLM_API_KEY", $apiKey.Value, "Process")
if ($null -ne $openHandsSessionKey) {
  [Environment]::SetEnvironmentVariable("OPENHANDS_SESSION_API_KEY", $openHandsSessionKey.Value, "Process")
} else {
  [Environment]::SetEnvironmentVariable("OPENHANDS_SESSION_API_KEY", $null, "Process")
}
if ($OpenHandsRuntimeMode -eq "managed") {
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
  Assert-Smoke ($null -ne $health -and $health.status -eq "ok") "Rust v3 health check failed. See $errLog"

  $jsonHeaders = @{ "Content-Type" = "application/json" }

  $providerBaseUrls = @{}
  $providerBaseUrls[$normalizedProvider] = $BaseUrl
  $providerProxyUrls = @{}
  if (-not [string]::IsNullOrWhiteSpace($ProviderProxyUrl)) {
    $providerProxyUrls[$normalizedProvider] = $ProviderProxyUrl
  }
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
  Assert-Smoke ($providerSettings.status.default_status.credential_configured -eq $true) "Provider Settings did not detect configured credentials."

  $providerTest = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/providers/test" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
    provider = $normalizedProvider
    mock = $false
  })
  Assert-Smoke ($providerTest.test.ok -eq $true) "Live provider test failed: $($providerTest.test.message)"
  Assert-Smoke ($providerTest.test.mode -eq "live") "Provider test did not use live mode."

  if ($OpenHandsRuntimeMode -eq "external") {
    $openHandsSettingsBody = @{
      enabled = $true
      runtime_mode = "external"
      server_url = $OpenHandsServerUrl
      workspace_mode = "local"
      allow_native_fallback = $false
    }
    if ($null -ne $openHandsSessionKey) {
      $openHandsSettingsBody["session_api_key"] = $openHandsSessionKey.Value
    }
    $openHandsSettings = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/openhands/settings" -Headers $jsonHeaders -Body (ConvertTo-JsonBody $openHandsSettingsBody)
  } else {
    $openHandsSettings = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/openhands/settings" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      enabled = $true
      runtime_mode = "managed"
      workspace_mode = "local"
      allow_native_fallback = $false
    })
  }
  $openHandsStatus = $openHandsSettings.status
  $openHandsStatusAttempts = if ($OpenHandsRuntimeMode -eq "managed") { 420 } else { 60 }
  foreach ($attempt in 1..$openHandsStatusAttempts) {
    if ($openHandsStatus.status -eq "connected") {
      break
    }
    Start-Sleep -Seconds 1
    $openHandsStatus = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/openhands/status"
  }
  Assert-Smoke ($openHandsStatus.status -eq "connected") "OpenHands Settings test did not connect: $($openHandsStatus.detail)"
  Assert-Smoke ($openHandsStatus.runtime_mode -eq $OpenHandsRuntimeMode) "OpenHands runtime mode mismatch."

  $defaultWorkflow = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/workflows/default"
  $config = $defaultWorkflow.config
  $config.workflows."planner-led".max_rounds = 1
  $config.workflows."planner-led".edges = @(
    @{
      from = "planner"
      to = "executor"
      on = "ready"
    }
  )
  $config.harnesses."openhands-code-edit".openhands.prefer_websocket = $false
  $config.harnesses."openhands-code-edit".openhands.poll_interval_ms = 1000
  $config.harnesses."openhands-code-edit".openhands.max_event_poll_seconds = 420
  $config.harnesses."openhands-code-edit".openhands.max_events = 100
  $effectiveOpenHandsApiProfile = $OpenHandsApiProfile
  if ($effectiveOpenHandsApiProfile -eq "auto") {
    $effectiveOpenHandsApiProfile = if ($OpenHandsRuntimeMode -eq "external" -and $null -ne $openHandsSessionKey) { "legacy-sdk" } else { "default" }
  }
  if ($effectiveOpenHandsApiProfile -eq "legacy-sdk" -and $null -eq $openHandsSessionKey) {
    Stop-Or-Skip -Reason "OpenHands legacy-sdk profile requires a developer session token."
  }
  if ($effectiveOpenHandsApiProfile -eq "legacy-sdk") {
    $config.harnesses."openhands-code-edit".openhands.api_paths = @{
      api_prefix = "/api"
      events_search_path = "/conversations/{conversation_id}/events/search"
      run_endpoint_path = "/conversations/{conversation_id}/run"
      websocket_path_template = "/sockets/events/{conversation_id}"
      auth_header = "x_session_api_key"
    }
    $config.harnesses."openhands-code-edit".openhands.run_start_strategy = "post_run_endpoint"
  } else {
    $config.harnesses."openhands-code-edit".openhands.api_paths = @{}
    $config.harnesses."openhands-code-edit".openhands.run_start_strategy = "post_user_event_with_run_true"
  }

  $sessionResponse = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
    workflow_id = $defaultWorkflow.workflow_id
    planner_agent_id = "planner"
    config = $config
    mode = "discuss"
  })
  $sessionId = $sessionResponse.session.session_id
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($sessionId)) "Planner session creation did not return session_id."

  function Send-PlannerTurn {
    param(
      [string]$Message,
      [bool]$Confirmed,
      [string]$Mode = "discuss"
    )
    Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/turn" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
      message = $Message
      confirmed = $Confirmed
      mode = $Mode
      planner_agent_id = "planner"
      config = $config
    })
  }

  $task = "Create a minimal Snake game in $projectPath. Requirements: create exactly these project files: index.html, style.css, main.js, and README.md; use vanilla HTML/CSS/JS only; no external network dependencies; arrow keys or WASD movement; visible score; a visible restart button with id='restart-btn' and text 'Restart'; code is readable. Do not create package.json, package-lock.json, node_modules, tests, screenshots, or reports. Gameplay requirements: the snake must not self-collide immediately on page load, the game must either wait for the first valid direction key or start with a safe initial direction, opposite-direction reversal must be prevented, and Game Over must not appear immediately after load or after the first valid move. Expose a minimal read-only window.__snakeTestState snapshot or object with gameOver, score, head, direction, and tick fields so Coder can validate browser gameplay after Start Work. During Start Work, do not install Playwright and do not run browser automation; Coder's smoke script runs browser validation after OpenHands finishes. Executor acceptance before final summary: create the four files, include #restart-btn, and run node --check main.js. Do not commit, push, publish, or clean the working tree."
  $firstTurn = Send-PlannerTurn -Message $task -Confirmed $false
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($firstTurn.assistant_message)) "First Planner turn did not return assistant text."
  Assert-Smoke ((Count-Words $firstTurn.assistant_message) -le 600) "First Planner response was too long."
  Assert-Smoke ($firstTurn.should_start_workflow -eq $false) "First Planner turn unexpectedly requested execution."

  $secondTurn = Send-PlannerTurn -Message "Confirm the Snake task is ready for Start Work. Execute only after Start Work through OpenHands. Target folder: $projectPath. Start Work should create only index.html, style.css, main.js, and README.md; include a visible restart button with id='restart-btn'; use no external dependencies; do not create package.json, package-lock.json, node_modules, tests, screenshots, or reports; do not install Playwright or run browser automation inside the user project. Run node --check main.js if available, then stop with a short final summary. Coder's smoke script will perform browser gameplay validation after OpenHands finishes. Review Changes and Final Summary must appear; leave changes uncommitted." -Confirmed $true -Mode "work"
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($secondTurn.assistant_message)) "Second Planner turn did not return assistant text."
  Assert-Smoke ((Count-Words $secondTurn.assistant_message) -le 120) "Ready Planner response was too long."
  Assert-Smoke ($secondTurn.assistant_message.Contains("Click Start Work")) "Planner did not direct user to Start Work."
  Assert-Smoke ($secondTurn.assistant_message.Contains("OpenHands executor")) "Planner did not mention OpenHands executor."
  Assert-Smoke (-not $secondTurn.assistant_message.Contains("Discuss mode")) "Planner exposed Discuss mode."
  Assert-Smoke (-not $secondTurn.assistant_message.Contains("Work mode")) "Planner exposed Work mode."
  Assert-Smoke ($secondTurn.should_start_workflow -eq $false) "Second Planner turn unexpectedly requested execution."
  Assert-Smoke (@($secondTurn.session.turns).Count -ge 4) "Planner did not retain two user/assistant turns."
  Assert-Smoke ($secondTurn.ready -eq $true) "Planner did not mark the plan ready."

  $startWork = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/planner-chat/sessions/$sessionId/start-work" -Headers $jsonHeaders -Body (ConvertTo-JsonBody @{
    repo = $projectPath
    workflow_id = $defaultWorkflow.workflow_id
    planner_agent_id = "planner"
    config = $config
    scopes = @("index.html", "style.css", "main.js", "README.md")
  })
  Assert-Smoke (-not [string]::IsNullOrWhiteSpace($startWork.run_id)) (Format-StartWorkFailure -StartWork $startWork -StorePath $storePath)
  Assert-Smoke ($startWork.status -eq "completed") (Format-StartWorkFailure -StartWork $startWork -StorePath $storePath)

  $runId = $startWork.run_id
  $events = Invoke-RestJsonWithRetry -Method Get -Uri "$base$($startWork.events_url)"
  $timeline = Invoke-RestJsonWithRetry -Method Get -Uri "$base$($startWork.timeline_url)"
  $timelineItems = @($timeline.items)
  Assert-Smoke ($timelineItems.Count -ge 1) "Timeline was empty."
  Assert-Smoke ($timelineItems.Count -le 240) "Timeline was too noisy: $($timelineItems.Count) items."
  $backendItems = @($timelineItems | Where-Object { $_.title -eq "Executor backend: OpenHands" })
  Assert-Smoke ($backendItems.Count -ge 1) "Timeline did not show Executor backend: OpenHands."
  $reactItems = @($timelineItems | Where-Object {
    $_.type -in @("reasoning_summary", "tool_call", "command_execution", "file_change", "executor_step")
  })
  Assert-Smoke ($reactItems.Count -ge 1) "Timeline did not include public ReAct items."
  $finalItems = @($timelineItems | Where-Object { $_.type -eq "final_summary" })
  Assert-Smoke ($finalItems.Count -ge 1) "Timeline did not include final summary."
  $timelineJson = $timeline | ConvertTo-Json -Depth 100
  Assert-Smoke (-not $timelineJson.Contains('"raw"')) "Timeline exposed raw backend JSON."

  $report = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$runId/report/preview"
  Assert-Smoke ($report.report.status -eq "completed") "Final report preview did not complete."
  Assert-Smoke ((Count-Words $report.report.summary) -le 500) "Final summary exceeded 500 words."

  $indexPath = Join-Path $projectPath "index.html"
  $stylePath = Join-Path $projectPath "style.css"
  $mainPath = Join-Path $projectPath "main.js"
  $readmePath = Join-Path $projectPath "README.md"
  Assert-Smoke (Test-Path -LiteralPath $indexPath) "index.html was not created."
  Assert-Smoke (Test-Path -LiteralPath $stylePath) "style.css was not created."
  Assert-Smoke (Test-Path -LiteralPath $mainPath) "main.js was not created."
  Assert-Smoke (Test-Path -LiteralPath $readmePath) "README.md was not created."
  $indexHtml = Get-Content -LiteralPath $indexPath -Raw
  Assert-Smoke ($indexHtml.Contains('id="restart-btn"') -or $indexHtml.Contains("id='restart-btn'")) "index.html did not include #restart-btn."
  foreach ($forbiddenPath in @("package.json", "package-lock.json", "node_modules", "tests", "screenshots", "reports")) {
    $candidate = Join-Path $projectPath $forbiddenPath
    Assert-Smoke (-not (Test-Path -LiteralPath $candidate)) "Start Work created forbidden Snake project artifact: $forbiddenPath."
  }

  $node = (Get-Command node).Source
  $nodeCheck = Invoke-NativeCapture -FilePath $node -Arguments @("--check", $mainPath)
  Assert-Smoke ($nodeCheck.ExitCode -eq 0) "node --check failed: $($nodeCheck.Output -join "`n")"

  $browserValidation = Invoke-SnakeBrowserValidation -IndexPath $indexPath
  Assert-Smoke ($browserValidation.status -eq "ok") "Browser gameplay validation did not pass."
  Assert-Smoke ($browserValidation.checks.no_immediate_game_over -eq $true) "Browser validation did not confirm no immediate Game Over."
  Assert-Smoke ($browserValidation.checks.direction_key_moves_game -eq $true) "Browser validation did not confirm direction key movement."
  Assert-Smoke ($browserValidation.checks.second_direction_continues -eq $true) "Browser validation did not confirm continued movement."
  Assert-Smoke ($browserValidation.checks.restart_works -eq $true) "Browser validation did not confirm Restart."

  $changes = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$runId/changes"
  $changeSets = @($changes.changes)
  Assert-Smoke ($changeSets.Count -ge 1) "Review Changes returned no change set."
  $changeSetId = $changeSets[0].change_set_id
  $changedPaths = @($changeSets[0].changed_files | ForEach-Object { $_.path })
  foreach ($required in @("index.html", "style.css", "main.js", "README.md")) {
    Assert-Smoke ($changedPaths -contains $required) "Review Changes did not include $required."
  }
  $diff = Invoke-RestJsonWithRetry -Method Get -Uri "$base/api/v3/runs/$runId/changes/$changeSetId/diff"
  foreach ($required in @("index.html", "style.css", "main.js", "README.md")) {
    Assert-Smoke ($diff.diff.Contains($required)) "Review diff did not mention $required."
  }

  $eventItems = @($events.events)
  $openHandsBackendEvents = @($eventItems | Where-Object {
    $_.kind -eq "backend.selected" -and $_.payload.node_id -eq "executor" -and $_.payload.backend -eq "openhands"
  })
  Assert-Smoke ($openHandsBackendEvents.Count -ge 1) "Events did not select OpenHands backend."
  $nativeFallbackEvents = @($eventItems | Where-Object {
    $_.kind -eq "backend.selected" -and $_.payload.node_id -eq "executor" -and $_.payload.backend -ne "openhands"
  })
  Assert-Smoke ($nativeFallbackEvents.Count -eq 0) "Executor selected a non-OpenHands backend."

  $undo = $null
  if ($UndoAfterReview) {
    $undo = Invoke-RestJsonWithRetry -Method Post -Uri "$base/api/v3/runs/$runId/changes/$changeSetId/undo"
    Assert-Smoke ($undo.status -eq "undone" -or $undo.status -eq "unsupported" -or $undo.status -eq "conflict") "Undo returned unexpected status: $($undo.status)"
  }

  $git = (Get-Command git).Source
  $gitDiff = Invoke-NativeCapture -FilePath $git -Arguments @("-C", $projectPath, "diff", "--no-ext-diff", "--no-textconv", "--")
  $gitStatus = @(git -C $projectPath status --porcelain)

  $eventsPath = Join-Path $storePath "runs\$runId\events.jsonl"
  $reportPath = Join-Path $storePath "runs\$runId\artifacts\final-report.json"
  $serialized = Get-SerializedArtifactsText -Objects @($providerSettings, $providerTest, $openHandsSettings, $firstTurn, $secondTurn, $startWork, $events, $timeline, $report, $changes, $diff, $undo, $gitDiff.Output) -Files @($eventsPath, $reportPath, $indexPath, $stylePath, $mainPath, $readmePath)
  Assert-NoSecretLeak -Text $serialized -Secrets @($apiKey, $openHandsSessionKey)

  [pscustomobject]@{
    status = "ok"
    validation = "live_snake_game_full_path"
    provider = $normalizedProvider
    model = $Model
    provider_test = $providerTest.test.mode
    openhands_connected = $openHandsStatus.status -eq "connected"
    openhands_status = $openHandsStatus.status
    backend_selected = "openhands"
    session_id = $sessionId
    planner_turns = @($secondTurn.session.turns).Count
    run_id = $runId
    start_work_status = $startWork.status
    timeline_items = $timelineItems.Count
    timeline_backend_items = $backendItems.Count
    timeline_react_items = $reactItems.Count
    final_report_status = $report.report.status
    final_summary_words = Count-Words $report.report.summary
    index_exists = Test-Path -LiteralPath $indexPath
    style_exists = Test-Path -LiteralPath $stylePath
    main_js_exists = Test-Path -LiteralPath $mainPath
    readme_exists = Test-Path -LiteralPath $readmePath
    forbidden_project_artifacts_absent = $true
    node_check = "passed"
    browser_gameplay = "passed"
    browser_validation = $browserValidation
    review_changes = $changeSets.Count
    changed_files = $changedPaths
    secrets_check = "passed"
    target_folder = $projectPath
    git_status = $gitStatus
    store = $storePath
  } | ConvertTo-Json -Depth 20
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
}
