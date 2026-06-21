import { useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";

import {
  autoUpdateSkills,
  disableSkill,
  discoverSkills,
  enableSkill,
  getInstalledSkills,
  getSkillUpdates,
  importDeveloperSkill,
  installSkill,
  pinSkill,
  removeSkill,
  rollbackSkill,
  setSkillUpdatePolicy,
  unpinSkill,
  updateSkill
} from "../../api";
import type { DiscoverSkillsPayload, InstalledSkillsPayload, SkillSummary, SkillUpdateInfo } from "../../types";

type SkillTab = "discover" | "installed" | "updates" | "trusted" | "developer";

const skillTabs: Array<{ id: SkillTab; label: string }> = [
  { id: "discover", label: "Discover" },
  { id: "installed", label: "Installed" },
  { id: "updates", label: "Updates" },
  { id: "trusted", label: "Trusted Sources" },
  { id: "developer", label: "Developer Import" }
];

interface SkillsPanelProps {
  onStatus: (status: string) => void;
}

export function SkillsPanel({ onStatus }: SkillsPanelProps) {
  const [tab, setTab] = useState<SkillTab>("discover");
  const [registryUrl, setRegistryUrl] = useState("");
  const [developerPath, setDeveloperPath] = useState("");
  const [installed, setInstalled] = useState<InstalledSkillsPayload | null>(null);
  const [discover, setDiscover] = useState<DiscoverSkillsPayload | null>(null);
  const [updates, setUpdates] = useState<SkillUpdateInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const installedById = useMemo(
    () => new Map((installed?.skills ?? []).map((skill) => [skill.id, skill])),
    [installed]
  );
  const updateById = useMemo(() => new Map(updates.map((item) => [item.skill_id, item])), [updates]);

  useEffect(() => {
    void refreshInstalled();
  }, []);

  async function refreshInstalled() {
    try {
      setInstalled(await getInstalledSkills());
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function refreshDiscover() {
    if (!registryUrl.trim()) {
      onStatus("Registry URL is required.");
      return;
    }
    setLoading(true);
    try {
      const payload = await discoverSkills(registryUrl.trim());
      setDiscover(payload);
      await refreshInstalled();
      onStatus(`Loaded ${payload.skills.length} skill(s) from registry.`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }

  async function refreshUpdates() {
    if (!registryUrl.trim()) {
      onStatus("Registry URL is required.");
      return;
    }
    setLoading(true);
    try {
      const payload = await getSkillUpdates(registryUrl.trim());
      setUpdates(payload.updates);
      onStatus(`Loaded update status for ${payload.updates.length} installed skill(s).`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }

  async function runSkillAction(label: string, action: () => Promise<unknown>) {
    setLoading(true);
    try {
      await action();
      await refreshInstalled();
      if (registryUrl.trim()) {
        const updatePayload = await getSkillUpdates(registryUrl.trim()).catch(() => ({ updates: [] }));
        setUpdates(updatePayload.updates);
      }
      onStatus(label);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }

  return (
    <section className="panel skills-panel">
      <div className="panel-title">Skills</div>
      <div className="skill-tabs">
        {skillTabs.map((item) => (
          <button className={item.id === tab ? "selected" : ""} key={item.id} onClick={() => setTab(item.id)}>
            {item.label}
          </button>
        ))}
      </div>
      <div className="skill-registry-row">
        <label>
          Registry URL
          <input value={registryUrl} onChange={(event) => setRegistryUrl(event.target.value)} />
        </label>
        <button disabled={loading} onClick={refreshDiscover}>Discover</button>
        <button disabled={loading} onClick={refreshUpdates}>Check Updates</button>
      </div>

      {tab === "discover" && (
        <div className="skill-grid">
          {(discover?.skills ?? []).map((skill) => (
            <SkillCard
              key={skill.id}
              skill={skill}
              installed={installedById.get(skill.id)}
              update={updateById.get(skill.id)}
              actions={
                <div className="button-row">
                  {skill.installed ? (
                    <button disabled={loading} onClick={() => runSkillAction(`Updated ${skill.id}.`, () => updateSkill(skill.id, registryUrl.trim()))}>
                      Update
                    </button>
                  ) : (
                    <button disabled={loading} onClick={() => runSkillAction(`Installed ${skill.id}.`, () => installSkill(skill.id, registryUrl.trim()))}>
                      Install
                    </button>
                  )}
                </div>
              }
            />
          ))}
          {!discover && <div className="muted">No registry loaded.</div>}
          {discover && discover.skills.length === 0 && <div className="muted">No skills found.</div>}
        </div>
      )}

      {tab === "installed" && (
        <div className="skill-grid">
          {(installed?.skills ?? []).map((skill) => (
            <SkillCard
              key={skill.id}
              skill={skill}
              update={updateById.get(skill.id)}
              actions={
                <div className="button-row">
                  <button disabled={loading} onClick={() => runSkillAction(`${skill.enabled ? "Disabled" : "Enabled"} ${skill.id}.`, () => skill.enabled ? disableSkill(skill.id) : enableSkill(skill.id))}>
                    {skill.enabled ? "Disable" : "Enable"}
                  </button>
                  <button disabled={loading || !registryUrl.trim()} onClick={() => runSkillAction(`Updated ${skill.id}.`, () => updateSkill(skill.id, registryUrl.trim()))}>
                    Update
                  </button>
                  <button disabled={loading} onClick={() => runSkillAction(`Pinned ${skill.id}.`, () => pinSkill(skill.id))}>Pin</button>
                  <button disabled={loading} onClick={() => runSkillAction(`Unpinned ${skill.id}.`, () => unpinSkill(skill.id))}>Unpin</button>
                  <button disabled={loading} onClick={() => runSkillAction(`Set manual updates for ${skill.id}.`, () => setSkillUpdatePolicy(skill.id, "manual"))}>
                    Manual
                  </button>
                  <button disabled={loading} onClick={() => runSkillAction(`Set auto-update for ${skill.id}.`, () => setSkillUpdatePolicy(skill.id, "auto_official_low_risk"))}>
                    Auto
                  </button>
                  <button disabled={loading} onClick={() => runSkillAction(`Rolled back ${skill.id}.`, () => rollbackSkill(skill.id))}>Rollback</button>
                  <button disabled={loading} onClick={() => runSkillAction(`Removed ${skill.id}.`, () => removeSkill(skill.id))}>Remove</button>
                </div>
              }
            />
          ))}
          {(!installed || installed.skills.length === 0) && <div className="muted">No installed skills.</div>}
        </div>
      )}

      {tab === "updates" && (
        <div className="skill-grid">
          <div className="skill-actions-strip">
            <button disabled={loading || !registryUrl.trim()} onClick={() => runSkillAction("Auto-update completed.", () => autoUpdateSkills(registryUrl.trim()))}>
              Auto-update Eligible
            </button>
          </div>
          {updates.map((item) => (
            <article className="skill-card" key={item.skill_id}>
              <div className="skill-card-heading">
                <strong>{item.skill_id}</strong>
                <span className={`status-pill ${item.update_available ? "warn" : "good"}`}>
                  {item.update_available ? "update" : "current"}
                </span>
              </div>
              <div className="summary-grid">
                <span>{item.installed_version}</span>
                <span>{item.available_version ?? "not listed"}</span>
                <span>{item.update_policy}</span>
                <span>{item.pinned_version ? `pinned ${item.pinned_version}` : "not pinned"}</span>
                <span>{item.auto_update_eligible ? "auto eligible" : "manual"}</span>
                <span>{item.reason ?? item.trust_level ?? "registry"}</span>
              </div>
              <div className="button-row">
                <button disabled={loading || !item.update_available || !registryUrl.trim()} onClick={() => runSkillAction(`Updated ${item.skill_id}.`, () => updateSkill(item.skill_id, registryUrl.trim()))}>
                  Update
                </button>
              </div>
            </article>
          ))}
          {updates.length === 0 && <div className="muted">No update status loaded.</div>}
        </div>
      )}

      {tab === "trusted" && (
        <div className="skill-grid">
          <article className="skill-card">
            <div className="skill-card-heading">
              <strong>{registryUrl.trim() || "Registry"}</strong>
              <span className="status-pill">configured</span>
            </div>
            <div className="summary-grid">
              <span>{discover?.registry.registry_version ?? "unknown version"}</span>
              <span>{discover?.registry.generated_at ?? "not loaded"}</span>
              <span>{(discover?.skills ?? []).filter((skill) => skill.trust_level === "official").length} official</span>
              <span>{(discover?.skills ?? []).filter((skill) => skill.trust_level === "verified").length} verified</span>
            </div>
          </article>
        </div>
      )}

      {tab === "developer" && (
        <div className="developer-import">
          <label>
            Local Skill Path
            <input value={developerPath} onChange={(event) => setDeveloperPath(event.target.value)} />
          </label>
          <button disabled={loading || !developerPath.trim()} onClick={() => runSkillAction(`Imported ${developerPath}.`, () => importDeveloperSkill(developerPath.trim()))}>
            Import
          </button>
        </div>
      )}
    </section>
  );
}

function SkillCard({
  skill,
  installed,
  update,
  actions
}: {
  skill: SkillSummary | (DiscoverSkillsPayload["skills"][number]);
  installed?: SkillSummary;
  update?: SkillUpdateInfo;
  actions: ReactNode;
}) {
  const connectors = "requires_connectors" in skill ? skill.requires_connectors : skill.connectors;
  const connectorOperations = skill.connector_operations ?? [];
  return (
    <article className="skill-card">
      <div className="skill-card-heading">
        <strong>{skill.name}</strong>
        <span className={`status-pill ${skill.risk_level === "high" ? "bad" : skill.risk_level === "medium" ? "warn" : "good"}`}>
          {skill.risk_level}
        </span>
      </div>
      <p>{skill.description}</p>
      <div className="summary-grid">
        <span>{skill.category}</span>
        <span>{skill.version}</span>
        <span>{skill.publisher}</span>
        <span>{skill.trust_level}</span>
        <span>{connectors.length > 0 ? connectors.join(", ") : "no connectors"}</span>
        <span>{connectorOperations.length > 0 ? `${connectorOperations.length} connector ops` : "no connector ops"}</span>
        <span>{skill.external_effect ? "external effects" : "no external effects"}</span>
        {installed && <span>{installed.enabled ? "enabled" : "disabled"}</span>}
        {update?.pinned_version && <span>pinned {update.pinned_version}</span>}
      </div>
      {actions}
      <details className="json-details">
        <summary>Details</summary>
        <pre>{JSON.stringify({ skill, installed, update }, null, 2)}</pre>
      </details>
    </article>
  );
}
