import type { PluginReadResponse } from "../../types";

interface SkillDetailsPanelProps {
  detail: PluginReadResponse | null;
}

export function SkillDetailsPanel({ detail }: SkillDetailsPanelProps) {
  if (!detail) {
    return (
      <section className="plugin-section">
        <div className="panel-title">Skill details</div>
        <div className="muted">Select an installed plugin to inspect its local skill surface.</div>
      </section>
    );
  }
  return (
    <section className="plugin-section">
      <div className="panel-title">Skill details</div>
      <div className="plugin-detail-heading">
        <strong>{detail.plugin.name}</strong>
        <code>{detail.plugin.id}</code>
      </div>
      <div className="plugin-card-grid">
        {detail.skills.map((skill) => (
          <article className="plugin-card" key={skill.id}>
            <div>
              <strong>{skill.name}</strong>
              <code>{skill.version}</code>
            </div>
            <p>{skill.description}</p>
            <div className="plugin-meta">
              <span>{skill.publisher}</span>
              <span>{skill.risk_level}</span>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
