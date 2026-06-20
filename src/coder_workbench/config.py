from __future__ import annotations

import os
from dataclasses import dataclass

DEFAULT_PROVIDER = "openai"
DEFAULT_MODEL = "gpt-4.1-mini"

PROVIDER_ENV_KEYS = {
    "openai": "OPENAI_API_KEY",
    "openai-compatible": "CODER_API_KEY",
    "deepseek": "DEEPSEEK_API_KEY",
    "moonshot": "MOONSHOT_API_KEY",
    "kimi": "MOONSHOT_API_KEY",
    "qwen": "DASHSCOPE_API_KEY",
    "dashscope": "DASHSCOPE_API_KEY",
    "groq": "GROQ_API_KEY",
    "openrouter": "OPENROUTER_API_KEY",
    "together": "TOGETHER_API_KEY",
    "mistral": "MISTRAL_API_KEY",
    "perplexity": "PERPLEXITY_API_KEY",
    "xai": "XAI_API_KEY",
    "gemini": "GEMINI_API_KEY",
    "ollama": "OLLAMA_API_KEY",
}


@dataclass(frozen=True)
class RuntimeConfig:
    provider: str
    model: str
    api_key: str | None
    base_url: str | None

    @property
    def has_llm_credentials(self) -> bool:
        if self.provider == "ollama":
            return True
        return bool(self.api_key)


def load_runtime_config(provider_override: str | None = None, model_override: str | None = None) -> RuntimeConfig:
    provider = (provider_override or os.getenv("CODER_PROVIDER", DEFAULT_PROVIDER)).strip().lower()
    return RuntimeConfig(
        provider=provider,
        model=model_override or os.getenv("CODER_MODEL", DEFAULT_MODEL),
        api_key=_api_key_for_provider(provider),
        base_url=base_url_for_provider(provider),
    )


def _api_key_for_provider(provider: str) -> str | None:
    return os.getenv(PROVIDER_ENV_KEYS.get(provider, "CODER_API_KEY")) or os.getenv("CODER_API_KEY")


def base_url_for_provider(provider: str) -> str | None:
    if os.getenv("CODER_BASE_URL"):
        return os.getenv("CODER_BASE_URL")
    return default_base_url(provider)


def default_base_url(provider: str) -> str | None:
    defaults = {
        "deepseek": "https://api.deepseek.com",
        "moonshot": "https://api.moonshot.cn/v1",
        "kimi": "https://api.moonshot.cn/v1",
        "qwen": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "dashscope": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "groq": "https://api.groq.com/openai/v1",
        "openrouter": "https://openrouter.ai/api/v1",
        "together": "https://api.together.xyz/v1",
        "mistral": "https://api.mistral.ai/v1",
        "perplexity": "https://api.perplexity.ai",
        "xai": "https://api.x.ai/v1",
        "gemini": "https://generativelanguage.googleapis.com/v1beta/openai",
        "ollama": os.getenv("OLLAMA_BASE_URL", "http://localhost:11434/v1"),
    }
    return defaults.get(provider)
