import { useCallback, useState } from "react";

import { getAgentRoleCards, getCapabilities, getHealth, getLiveRuns, getRuns } from "../api";
import type { CapabilitySpec, HealthStatus, RoleCardSpec, RunSummaryItem } from "../types";

export function useRuntimeInfo(onStatus: (status: string) => void) {
  const [capabilities, setCapabilities] = useState<CapabilitySpec[]>([]);
  const [runHistory, setRunHistory] = useState<RunSummaryItem[]>([]);
  const [liveRuns, setLiveRuns] = useState<RunSummaryItem[]>([]);
  const [health, setHealth] = useState<HealthStatus | null>(null);
  const [roleCards, setRoleCards] = useState<RoleCardSpec[]>([]);

  const refreshRuntimeInfo = useCallback(() => {
    Promise.all([getRuns(), getLiveRuns(), getHealth(), getCapabilities(), getAgentRoleCards()])
      .then(([runs, live, nextHealth, nextCapabilities, nextRoleCards]) => {
        setRunHistory(runs);
        setLiveRuns(live);
        setHealth(nextHealth);
        setCapabilities(nextCapabilities);
        setRoleCards(nextRoleCards);
      })
      .catch((error) => onStatus(`Failed to load runtime info: ${error.message}`));
  }, [onStatus]);

  return {
    capabilities,
    runHistory,
    liveRuns,
    health,
    roleCards,
    refreshRuntimeInfo
  };
}
