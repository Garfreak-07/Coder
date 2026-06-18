from __future__ import annotations

import os
from dataclasses import dataclass


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


def load_runtime_config() -> RuntimeConfig:
    provider = os.getenv("CODER_PROVIDER", "openai").strip().lower()
    return RuntimeConfig(
        provider=provider,
        model=os.getenv("CODER_MODEL", "gpt-4.1-mini"),
        api_key=_api_key_for_provider(provider),
        base_url=_base_url_for_provider(provider),
    )


def _api_key_for_provider(provider: str) -> str | None:
    env_names = {
        "openai": "OPENAI_API_KEY",
        "openai-compatible": "CODER_API_KEY",
        "deepseek": "DEEPSEEK_API_KEY",
        "moonshot": "MOONSHOT_API_KEY",
        "kimi": "MOONSHOT_API_KEY",
        "qwen": "DASHSCOPE_API_KEY",
        "dashscope": "DASHSCOPE_API_KEY",
        "ollama": "OLLAMA_API_KEY",
    }
    return os.getenv(env_names.get(provider, "CODER_API_KEY")) or os.getenv("CODER_API_KEY")


def _base_url_for_provider(provider: str) -> str | None:
    if os.getenv("CODER_BASE_URL"):
        return os.getenv("CODER_BASE_URL")

    defaults = {
        "deepseek": "https://api.deepseek.com",
        "moonshot": "https://api.moonshot.cn/v1",
        "kimi": "https://api.moonshot.cn/v1",
        "qwen": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "dashscope": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "ollama": os.getenv("OLLAMA_BASE_URL", "http://localhost:11434/v1"),
    }
    return defaults.get(provider)
