# Provider Setup

Normal users configure model access in the app UI:

1. Open `Settings`.
2. Choose one provider:
   - provider: `deepseek`
   - provider: `openai-compatible`
   - provider: `custom`
3. For DeepSeek, use:
   - base URL: `https://api.deepseek.com`
   - model: `deepseek-v4-flash`
4. Paste the provider API key into `API Key`.
5. Click `Save`.
6. Click `Test Provider`.

`deepseek-v4-pro` is also supported when the account has access to that model.
The `DeepSeek preset` button fills the DeepSeek-compatible base URL and model;
users still provide their own API key.

Provider id `deepseek` uses the OpenAI-compatible chat completions API. The
separate `openai-compatible` and `custom` provider choices remain available for
other services with user-provided base URLs and models.

Planner Chat uses the configured provider in product mode. If provider
credentials are missing, the Planner returns a setup-required assistant message
instead of using a fake product response.

The `Test Provider` result shows success or failure, the model used, and the
sanitized chat completions endpoint. It never displays the API key.

Mock mode is for CI and developer debugging only. It is hidden from the normal
Settings path and is not product-mode Planner behavior.

API keys are accepted by the Rust server and kept in server memory for this MVP.
The settings response only returns whether a key is configured and where it came
from. Plaintext keys must not be written into repository files, run events,
timeline items, evidence blobs, reports, debug exports, or screenshots.

Use `Clear API Key` in Settings to remove the current provider key from the
server's in-memory settings. Leaving the API key field blank during `Save`
keeps the existing key.

TODO: replace the in-memory key store with an OS keychain or local secret store
before public desktop release.

## Troubleshooting

- `401` or `403`: the API key is missing, expired, copied incorrectly, or does
  not have access to the selected provider. Re-enter the key in Settings and run
  `Test Provider` again.
- `404`, `model not found`, or similar model errors: the model field does not
  match a model available to the account. For DeepSeek, start with
  `deepseek-v4-flash`, then switch only after confirming account access.
- Network, timeout, DNS, or proxy errors: verify the base URL, local proxy, VPN,
  firewall, and Windows proxy settings. The DeepSeek base URL should normally be
  `https://api.deepseek.com`.
- OpenHands unavailable vs Planner provider unavailable: OpenHands is the
  optional coding-agent executor backend. Planner provider errors mean the chat
  model itself is not configured or reachable. Fix Provider Settings first when
  Planner Chat cannot answer; fix OpenHands settings only when Start Work needs
  an OpenHands executor and that backend is unavailable.

## Developer Fallback

Environment variables remain for CI, smoke tests, and headless development.
They are fallback paths, not the normal user setup path:

```powershell
$env:LLM_BASE_URL="https://api.deepseek.com"
$env:LLM_API_KEY="..."
$env:LLM_MODEL="deepseek-v4-flash"
```

Provider-specific variables such as `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, or
`CODER_API_KEY` may also be used by developer tooling. They are fallback paths,
not the normal user setup path.

When a provider key is configured in Settings, that in-memory Settings key wins
over the environment fallback for the same provider. Clearing the Settings key
returns the provider to the developer/headless environment fallback if one is
present.

## Optional Live LLM Smoke

Mock tests prove CI-safe plumbing only. The live smoke proves the product
Planner path can call a real OpenAI-compatible provider without making CI
depend on paid credentials:

```powershell
$env:CODER_LIVE_LLM_SMOKE="1"
powershell -ExecutionPolicy Bypass -File .\scripts\live-llm-smoke.ps1 -SkipIfMissingProvider
```

The script starts a temporary Rust API v3 server, configures Provider Settings
in server memory, sends two Planner Chat turns, verifies chat turns do not
start execution, and calls Start Work. It returns `skipped` when no provider key
is available and `-SkipIfMissingProvider` is set. Without
`CODER_LIVE_LLM_SMOKE=1`, it also returns `skipped` with
`-SkipIfMissingProvider` so ordinary CI cannot accidentally call a paid
provider.

For DeepSeek, set one of these first:

```powershell
$env:DEEPSEEK_API_KEY="..."
# or
$env:LLM_API_KEY="..."
```

The key is passed only through the current process environment or in-memory
provider settings for the temporary server. The script does not write plaintext
keys to repository files or print them.

For OpenAI or another OpenAI-compatible service, pass the provider, base URL,
model, and key env name explicitly:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\live-llm-smoke.ps1 `
  -Provider openai `
  -BaseUrl https://api.openai.com/v1 `
  -Model gpt-5.5 `
  -ApiKeyEnv OPENAI_API_KEY
```
