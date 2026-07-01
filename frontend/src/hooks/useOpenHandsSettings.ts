import { useCallback, useState } from "react";

import {
  getOpenHandsSettings,
  getOpenHandsStatus,
  saveOpenHandsSettings
} from "../api";
import type { OpenHandsFormState, OpenHandsSettings, OpenHandsStatus } from "../types";

export const defaultOpenHandsForm: OpenHandsFormState = {
  enabled: true,
  server_url: "http://127.0.0.1:8000",
  session_api_key: "",
  workspace_mode: "local",
  allow_native_fallback: false
};

export function useOpenHandsSettings(onStatus: (status: string) => void) {
  const [openHandsSettings, setOpenHandsSettings] = useState<OpenHandsSettings | null>(null);
  const [openHandsStatus, setOpenHandsStatus] = useState<OpenHandsStatus | null>(null);
  const [openHandsForm, setOpenHandsForm] = useState<OpenHandsFormState>(defaultOpenHandsForm);

  const refreshOpenHandsInfo = useCallback(() => {
    Promise.all([getOpenHandsSettings(), getOpenHandsStatus()])
      .then(([settings, status]) => {
        setOpenHandsSettings(settings);
        setOpenHandsStatus(status);
        setOpenHandsForm({
          enabled: settings.enabled,
          server_url: settings.server_url || defaultOpenHandsForm.server_url,
          session_api_key: "",
          workspace_mode: settings.workspace_mode || defaultOpenHandsForm.workspace_mode,
          allow_native_fallback: settings.allow_native_fallback
        });
      })
      .catch((error) => onStatus(`Failed to load OpenHands settings: ${error.message}`));
  }, [onStatus]);

  const updateOpenHandsForm = useCallback((patch: Partial<OpenHandsFormState>) => {
    setOpenHandsForm((current) => ({ ...current, ...patch }));
  }, []);

  const persistOpenHandsSettings = useCallback(async () => {
    const payload = buildOpenHandsSettingsPayload(openHandsForm);
    onStatus("Saving OpenHands settings...");
    try {
      const result = await saveOpenHandsSettings(payload);
      setOpenHandsSettings(result.settings);
      setOpenHandsStatus(result.status);
      setOpenHandsForm((current) => ({
        ...current,
        enabled: result.settings.enabled,
        server_url: result.settings.server_url,
        workspace_mode: result.settings.workspace_mode,
        allow_native_fallback: result.settings.allow_native_fallback,
        session_api_key: ""
      }));
      onStatus(`OpenHands settings saved: ${result.status.status}.`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }, [onStatus, openHandsForm]);

  const runOpenHandsTest = useCallback(async () => {
    const payload = buildOpenHandsSettingsPayload(openHandsForm);
    onStatus("Saving OpenHands settings before test...");
    try {
      const saved = await saveOpenHandsSettings(payload);
      setOpenHandsSettings(saved.settings);
      setOpenHandsForm((current) => ({
        ...current,
        enabled: saved.settings.enabled,
        server_url: saved.settings.server_url,
        workspace_mode: saved.settings.workspace_mode,
        allow_native_fallback: saved.settings.allow_native_fallback,
        session_api_key: ""
      }));
      onStatus("Checking OpenHands...");
      const status = await getOpenHandsStatus();
      setOpenHandsStatus(status);
      onStatus(`OpenHands ${status.status}: ${status.detail}`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }, [onStatus, openHandsForm]);

  const clearOpenHandsToken = useCallback(async () => {
    onStatus("Clearing OpenHands token...");
    try {
      const result = await saveOpenHandsSettings({ session_api_key: null });
      setOpenHandsSettings(result.settings);
      setOpenHandsStatus(result.status);
      setOpenHandsForm((current) => ({ ...current, session_api_key: "" }));
      onStatus("OpenHands token cleared.");
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }, [onStatus]);

  return {
    openHandsSettings,
    openHandsStatus,
    openHandsForm,
    updateOpenHandsForm,
    refreshOpenHandsInfo,
    persistOpenHandsSettings,
    runOpenHandsTest,
    clearOpenHandsToken
  };
}

function buildOpenHandsSettingsPayload(form: OpenHandsFormState): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    server_url: form.server_url.trim() || defaultOpenHandsForm.server_url,
    workspace_mode: form.workspace_mode.trim() || defaultOpenHandsForm.workspace_mode,
    allow_native_fallback: false
  };
  if (form.session_api_key.trim()) {
    payload.session_api_key = form.session_api_key.trim();
  }
  return payload;
}
