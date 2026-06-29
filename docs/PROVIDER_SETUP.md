# Provider Setup

Normal users configure model access in the app UI:

1. Open `Settings`.
2. Click `DeepSeek preset`, or choose:
   - provider: `openai-compatible`
   - base URL: `https://api.deepseek.com`
   - model: `deepseek-v4-flash`
3. Paste the provider API key into `API Key`.
4. Click `Save`.
5. Click `Test Provider`.

`deepseek-v4-pro` is also supported when the account has access to that model.

API keys are accepted by the Rust server and kept in server memory for this MVP.
The settings response only returns whether a key is configured and where it came
from. Plaintext keys must not be written into repository files, run events,
timeline items, evidence blobs, reports, debug exports, or screenshots.

TODO: replace the in-memory key store with an OS keychain or local secret store
before public desktop release.

## Developer Fallback

Environment variables remain for CI, smoke tests, and headless development:

```powershell
$env:LLM_BASE_URL="https://api.deepseek.com"
$env:LLM_API_KEY="..."
$env:LLM_MODEL="deepseek-v4-flash"
```

Provider-specific variables such as `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, or
`CODER_API_KEY` may also be used by developer tooling. They are fallback paths,
not the normal user setup path.
