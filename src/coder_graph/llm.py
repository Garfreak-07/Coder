from __future__ import annotations

from langchain_openai import ChatOpenAI

from .config import RuntimeConfig


def create_chat_model(config: RuntimeConfig) -> ChatOpenAI:
    """Create the chat model used by planner/reviewer nodes.

    Keep this intentionally small: many model vendors expose an
    OpenAI-compatible API, so one adapter covers a lot of ground without
    adding provider-specific dependencies.
    """

    kwargs: dict[str, object] = {
        "model": config.model,
        "temperature": 0,
    }

    if config.base_url:
        kwargs["base_url"] = config.base_url

    if config.provider == "ollama":
        # Ollama's OpenAI-compatible endpoint accepts any non-empty key.
        kwargs["api_key"] = config.api_key or "ollama"
    elif config.api_key:
        kwargs["api_key"] = config.api_key

    return ChatOpenAI(**kwargs)
