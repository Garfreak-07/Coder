import { useCallback } from "react";

import { getHealth } from "../api";

export function useRuntimeInfo(onStatus: (status: string) => void) {
  const refreshRuntimeInfo = useCallback(() => {
    getHealth()
      .then(() => undefined)
      .catch((error) => onStatus(`Failed to load runtime info: ${error.message}`));
  }, [onStatus]);

  return { refreshRuntimeInfo };
}
