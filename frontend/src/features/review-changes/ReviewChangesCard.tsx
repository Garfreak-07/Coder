import type { ChangeSet } from "./changeSetTypes";

interface ReviewChangesCardProps {
  changeSets?: ChangeSet[] | null;
  diffByChangeSetId: Record<string, string>;
  loadingChangeSetId: string | null;
  onAccept: (changeSetId: string) => void;
  onLoadDiff: (changeSetId: string) => void;
  onUndo: (changeSetId: string) => void;
}

export function ReviewChangesCard({
  changeSets,
  diffByChangeSetId,
  loadingChangeSetId,
  onAccept,
  onLoadDiff,
  onUndo
}: ReviewChangesCardProps) {
  const safeChangeSets = Array.isArray(changeSets) ? changeSets.filter(isChangeSet) : [];
  if (safeChangeSets.length === 0) return null;
  return (
    <section className="review-changes-card" aria-label="Review changes">
      <div className="review-header">
        <div>
          <span>Review changes</span>
          <strong>{safeChangeSets.length} change set{safeChangeSets.length === 1 ? "" : "s"}</strong>
        </div>
      </div>
      {safeChangeSets.map((changeSet) => {
        const diff = diffByChangeSetId[changeSet.change_set_id];
        const loading = loadingChangeSetId === changeSet.change_set_id;
        const canAccept = changeSet.status === "pending_review";
        const canUndo =
          Boolean(changeSet.reverse_patch_ref) &&
          (changeSet.status === "pending_review" || changeSet.status === "accepted");
        const undoConflict = changeSet.status === "failed_to_undo";
        return (
          <article
            className={`change-set-card change-set-${changeSet.status}`}
            key={changeSet.change_set_id}
            aria-busy={loading}
          >
            <div className="change-set-title">
              <strong>{changeSet.change_set_id}</strong>
              <code>{changeSet.status}</code>
            </div>
            <div className="timeline-meta">
              <span>{changeSet.repo_root}</span>
              {changeSet.reverse_patch_ref && <span>undo available</span>}
            </div>
            {safeArray(changeSet.changed_files).length > 0 && (
              <div className="change-file-list">
                {safeArray(changeSet.changed_files).map((file) => (
                  <span key={file.path}>
                    {file.path}
                    <small>{file.change_type}</small>
                  </span>
                ))}
              </div>
            )}
            {safeArray(changeSet.command_checks).length > 0 && (
              <div className="change-check-list">
                {safeArray(changeSet.command_checks).map((check) => (
                  <code key={check.command}>{check.command}</code>
                ))}
              </div>
            )}
            {loading ? (
              <div className="review-diff-state" role="status">Loading diff...</div>
            ) : diff ? (
              <pre className="review-diff">{diff}</pre>
            ) : (
              <div className="review-diff-state">Diff is not loaded yet.</div>
            )}
            {undoConflict && (
              <p className="review-conflict">
                {changeSet.undo_conflict ??
                  "Undo blocked because the working tree changed after this review was recorded."}
              </p>
            )}
            <div className="review-actions">
              <button onClick={() => onLoadDiff(changeSet.change_set_id)} disabled={loading}>
                {loading ? "Loading diff..." : diff ? "Refresh diff" : "View diff"}
              </button>
              <button onClick={() => onAccept(changeSet.change_set_id)} disabled={!canAccept || loading}>
                Accept
              </button>
              <button onClick={() => onUndo(changeSet.change_set_id)} disabled={!canUndo || loading}>
                Undo
              </button>
            </div>
          </article>
        );
      })}
    </section>
  );
}

function safeArray<T>(values: T[] | null | undefined): T[] {
  return Array.isArray(values) ? values : [];
}

function isChangeSet(changeSet: unknown): changeSet is ChangeSet {
  return Boolean(
    changeSet &&
      typeof changeSet === "object" &&
      typeof (changeSet as { change_set_id?: unknown }).change_set_id === "string"
  );
}
