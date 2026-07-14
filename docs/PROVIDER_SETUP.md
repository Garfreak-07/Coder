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
- provider network parameters: request retries, stream retries, stream idle
  timeout, WebSocket connect timeout, and provider WebSocket capability
- mock mode for local plumbing tests

Provider Settings survive server restarts. Coder stores API keys in the OS
credential store and writes only non-secret settings plus configured-provider
references to `settings/providers.json` in the Coder store. Environment
variables remain a developer/headless fallback. API keys must never be
committed to scripts, docs, examples, or task profile configuration.

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

1. Provider Settings secret loaded from the OS credential store.
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
  `NO_PROXY`. When none is set, preserve reqwest's platform/system proxy
  discovery instead of forcing direct access.

This keeps local providers and DeepSeek direct by default while still allowing
OpenAI-compatible providers to use a developer proxy when required.

All active external HTTP paths share one route resolver. Provider and webhook
traffic therefore use the same `NO_PROXY` host/port matching and credential-safe
route diagnostics. Loopback destinations always bypass an explicit proxy.

## Custom CA

For an enterprise TLS proxy or private gateway, set one PEM bundle:

1. `CODER_CA_CERTIFICATE`
2. `SSL_CERT_FILE` fallback

The selected bundle is added to system trust for provider, SSE, and webhook
clients. An unreadable, empty, or invalid bundle fails client construction with
the selecting environment variable and file path, without logging certificate
contents or provider credentials.

Provider network defaults match Codex:

- request retries: 4, capped at 100
- stream reconnect budget: 5, capped at 100
- stream idle timeout: 300,000 ms
- WebSocket connect timeout: 15,000 ms

Coder currently uses OpenAI-compatible Chat Completions over HTTP/SSE. A
provider's WebSocket capability is metadata until a supported wire protocol can
preserve turn state and fall back to HTTP without replaying completed output.

Provider keys belong to the host model transport. Model-generated foreground
and background commands do not inherit the provider key environment variables.
This prevents accidental key disclosure but is not OS-level network isolation;
commands can still open sockets until a platform sandbox and managed proxy are
implemented together.

## Live Provider Checks

Use Provider Settings in the app to test credentials and transport. Run an
ordinary Task against a throwaway repository to test the complete model/tool
path. Coder does not require a separate platform-specific smoke script.

Live checks send their Conversation or Task context to the configured
provider. Use throwaway work roots and an explicitly configured cache location
when large artifacts are expected. Coder does not write plaintext provider
secrets to run artifacts or print them in status output.

During a Task run, `native-code-edit` can use the configured provider through
`native-model-tool-loop`. It is an OpenAI-compatible tool-call loop for
repo/git/write/finish operations and a frozen snapshot of registered stdio MCP
tools. Rust owns the only side-effect
boundary: it writes only repo-relative files through native tools, records
`file.written` events, and stores repo evidence. Missing credentials return an
explicit blocked result. The deterministic backend runs only when mock mode is
explicitly enabled. Plain assistant text is summary-only and cannot write files.

## Secret Hygiene

- Do not put keys in committed scripts.
- Do not paste keys into docs or task profile JSON.
- Prefer app Settings for normal use.
- Use process-scoped environment variables only for developer/headless runs.
- Error messages returned through Provider Settings are redacted.
