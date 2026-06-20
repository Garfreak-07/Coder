from __future__ import annotations

import json
import os
from pathlib import Path
from threading import Lock
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.config import DEFAULT_MODEL, DEFAULT_PROVIDER, PROVIDER_ENV_KEYS, default_base_url


class ProviderSettings(BaseModel):
    model_config = ConfigDict(extra="forbid")

    default_provider: str = DEFAULT_PROVIDER
    default_model: str = DEFAULT_MODEL
    base_urls: dict[str, str] = Field(default_factory=dict)
    api_keys: dict[str, str] = Field(default_factory=dict)
    mock_mode: bool = True


class ProviderSettingsStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.path = self.root / "settings.json"
        self._lock = Lock()
        self.root.mkdir(parents=True, exist_ok=True)

    def load(self) -> ProviderSettings:
        if not self.path.exists():
            return ProviderSettings()
        try:
            return ProviderSettings.model_validate(json.loads(self.path.read_text(encoding="utf-8")))
        except Exception:
            return ProviderSettings()

    def save(self, patch: dict[str, Any]) -> ProviderSettings:
        with self._lock:
            current = self.load()
            payload = current.model_dump(mode="json")

            if "default_provider" in patch:
                payload["default_provider"] = _normalize_provider(patch.get("default_provider")) or DEFAULT_PROVIDER
            if "default_model" in patch:
                payload["default_model"] = str(patch.get("default_model") or DEFAULT_MODEL).strip() or DEFAULT_MODEL
            if "mock_mode" in patch:
                payload["mock_mode"] = bool(patch.get("mock_mode"))
            if "base_urls" in patch and isinstance(patch["base_urls"], dict):
                payload["base_urls"] = _clean_string_map(patch["base_urls"])
            if "api_keys" in patch and isinstance(patch["api_keys"], dict):
                payload["api_keys"] = _merge_secret_map(current.api_keys, patch["api_keys"])

            next_settings = ProviderSettings.model_validate(payload)
            self.path.write_text(next_settings.model_dump_json(indent=2), encoding="utf-8")
            return next_settings

    def response(self) -> dict[str, Any]:
        return settings_response(self.load())


def settings_response(settings: ProviderSettings) -> dict[str, Any]:
    return {
        "default_provider": settings.default_provider,
        "default_model": settings.default_model,
        "base_urls": settings.base_urls,
        "api_keys": {
            provider: {
                "configured": bool(value),
                "source": "settings",
            }
            for provider, value in settings.api_keys.items()
            if value
        },
        "mock_mode": settings.mock_mode,
    }


def provider_status(settings: ProviderSettings, providers: list[str] | None = None) -> dict[str, Any]:
    selected = providers or sorted({settings.default_provider, *settings.api_keys.keys(), *PROVIDER_ENV_KEYS.keys()})
    statuses = [_provider_status(settings, _normalize_provider(provider) or DEFAULT_PROVIDER) for provider in selected]
    default_status = _provider_status(settings, _normalize_provider(settings.default_provider) or DEFAULT_PROVIDER)
    return {
        "default_provider": settings.default_provider,
        "default_model": settings.default_model,
        "mock_mode": settings.mock_mode,
        "default_status": default_status,
        "providers": statuses,
    }


def workflow_provider_status(settings: ProviderSettings, workflow: Any) -> dict[str, Any]:
    providers = {
        _normalize_provider(getattr(agent, "provider", None) or settings.default_provider) or DEFAULT_PROVIDER
        for agent in getattr(workflow, "agents", [])
    }
    if not providers:
        providers.add(settings.default_provider)
    return provider_status(settings, sorted(providers))


def resolve_settings_config(
    settings: ProviderSettings,
    provider_override: str | None,
    model_override: str | None,
) -> dict[str, str | None]:
    provider = _normalize_provider(provider_override) or _normalize_provider(os.getenv("CODER_PROVIDER")) or settings.default_provider
    provider = _normalize_provider(provider) or DEFAULT_PROVIDER
    model = model_override or os.getenv("CODER_MODEL") or settings.default_model
    api_key, _ = _credential_for_provider(settings, provider)
    base_url = os.getenv("CODER_BASE_URL") or settings.base_urls.get(provider) or default_base_url(provider)
    return {
        "provider": provider,
        "model": model,
        "api_key": api_key,
        "base_url": base_url,
    }


def _provider_status(settings: ProviderSettings, provider: str) -> dict[str, Any]:
    api_key, source = _credential_for_provider(settings, provider)
    configured = provider == "ollama" or bool(api_key) or settings.mock_mode
    return {
        "provider": provider,
        "configured": configured,
        "credential_configured": provider == "ollama" or bool(api_key),
        "credential_source": "ollama" if provider == "ollama" else source,
        "base_url": os.getenv("CODER_BASE_URL") or settings.base_urls.get(provider) or default_base_url(provider),
        "mode": "mock" if settings.mock_mode and not api_key and provider != "ollama" else "live",
    }


def _credential_for_provider(settings: ProviderSettings, provider: str) -> tuple[str | None, str]:
    env_name = PROVIDER_ENV_KEYS.get(provider, "CODER_API_KEY")
    value = os.getenv(env_name) or os.getenv("CODER_API_KEY")
    if value:
        return value, "environment"
    value = settings.api_keys.get(provider)
    if value:
        return value, "settings"
    return None, "missing"


def _normalize_provider(value: Any) -> str | None:
    if value is None:
        return None
    normalized = str(value).strip().lower()
    return normalized or None


def _clean_string_map(value: dict[str, Any]) -> dict[str, str]:
    cleaned: dict[str, str] = {}
    for key, item in value.items():
        provider = _normalize_provider(key)
        if not provider:
            continue
        text = str(item or "").strip()
        if text:
            cleaned[provider] = text
    return cleaned


def _merge_secret_map(current: dict[str, str], patch: dict[str, Any]) -> dict[str, str]:
    merged = dict(current)
    for key, item in patch.items():
        provider = _normalize_provider(key)
        if not provider:
            continue
        if item is None:
            merged.pop(provider, None)
            continue
        text = str(item).strip()
        if not text or set(text) == {"*"}:
            continue
        merged[provider] = text
    return merged
