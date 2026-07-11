# Provider Setup

The normal user path is Provider Settings in the app. Environment variables are
developer/headless fallback only.

## App Settings

Provider Settings can configure:

- default provider
- default model
- base URL per provider
- API key per provider
- proxy mode per provider: `direct`, `explicit`, or `environment`
- explicit proxy URL per provider
- mock mode for local plumbing tests

Settings are kept in the local Coder store. API keys must never be committed to
scripts, docs, examples, or workflow specs.

## Provider Defaults

Default base URLs:

- `openai`: `https://api.openai.com/v1`
- `deepseek`: `https://api.deepseek.com`
- `moonshot` / `kimi`: `https://api.moonshot.cn/v1`
- `qwen` / `dashscope`: `https://dashscope.aliyuncs.com/compatible-mode/v1`
- `groq`: `https://api.groq.com/openai/v1`
- `openrouter`: `https://openrouter.ai/api/v1`
- `together`: `https://api.together.xyz/v1`
- `mistral`: `https://api.mistral.ai/v1`
- `perplexity`: `https://api.perplexity.ai`
- `xai`: `https://api.x.ai/v1`
- `gemini`: `https://generativelanguage.googleapis.com/v1beta/openai`
- `ollama`: `http://localhost:11434/v1`

DeepSeek and Ollama default to `direct` proxy mode. Other providers default to
`environment` proxy mode. Set an explicit provider proxy URL only when a
provider needs it.

## Environment Fallback

Credential lookup order:

1. Provider Settings secret.
2. Provider-specific environment variable.
3. `CODER_API_KEY`.
4. `LLM_API_KEY`.

Provider-specific keys include:

- `DEEPSEEK_API_KEY`
- `OPENAI_API_KEY`
- `MOONSHOT_API_KEY`
- `DASHSCOPE_API_KEY`
- `OPENROUTER_API_KEY`
- `GROQ_API_KEY`
- `TOGETHER_API_KEY`
- `MISTRAL_API_KEY`
- `PERPLEXITY_API_KEY`
- `XAI_API_KEY`
- `GEMINI_API_KEY`

Base URL fallback:

1. model-specific env field from config
2. `CODER_BASE_URL`
3. `LLM_BASE_URL`
4. provider default

DeepSeek developer fallback:

```powershell
$env:DEEPSEEK_API_KEY = Read-Host "DeepSeek API key"
$env:LLM_API_KEY=$env:DEEPSEEK_API_KEY
$env:LLM_BASE_URL="https://api.deepseek.com"
$env:LLM_MODEL="deepseek-chat"
```

## Proxy Isolation

Proxy modes:

- `direct`: do not use proxy env vars.
- `explicit`: use the provider's configured proxy URL.
- `environment`: use `CODER_{PROVIDER}_PROXY_URL`,
  `CODER_PROVIDER_PROXY_URL`, `HTTPS_PROXY`, or `HTTP_PROXY`, respecting
  `NO_PROXY`.

This keeps local providers and DeepSeek direct by default while still allowing
OpenAI-compatible providers to use a developer proxy when required.

## Live Tests

Provider-only live smoke:

```powershell
$env:CODER_LIVE_LLM_SMOKE="1"
powershell -ExecutionPolicy Bypass -File .\scripts\live-llm-smoke.ps1 -SkipIfMissingProvider
```

Native full-path live self-test:

```powershell
$env:CODER_SELFTEST_LIVE="1"
powershell -ExecutionPolicy Bypass -File .\scripts\live-coder-selftest-suite.ps1 -SkipIfMissingLiveConfig
```

Open-ended browser cases additionally preflight Coder's owned verifier runtime.
Developers can prepare the package without modifying a target repo:

```powershell
npm run browser-verifier:install
```

Live tests send the temporary task context to the configured provider. They
should use throwaway work roots and repo-local/F-drive cache paths when large
artifacts are expected. The smoke scripts use process-scoped environment
variables for keys; Coder does not write plaintext provider secrets to run
artifacts or print them in status output.

During Start Work, `native-code-edit` can use the configured provider through
`native-model-file-write`. The preferred path is an OpenAI-compatible tool-call
loop for repo/git/write/finish operations. If no tool calls are returned, Coder
uses the strict JSON file-plan fallback. Rust still owns the side-effect
boundary: it writes only repo-relative files through the native file tool,
records `file.written` events, stores repo evidence, and falls back to the
deterministic native backend when credentials are missing or mock mode is on.

## Secret Hygiene

- Do not put keys in committed scripts.
- Do not paste keys into docs or workflow JSON.
- Prefer app Settings for normal use.
- Prefer process-scoped environment variables for developer smokes.
- Error messages returned through Provider Settings are redacted.
