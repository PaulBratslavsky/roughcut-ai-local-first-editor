// Edit history — "go back in time". A view over the journal (useHistory):
// every applied edit then every undone/future edit, newest at top. Click any
// row to jump there (jump_to undoes/redoes to land exactly on it). The
// current point is marked; future (undone) rows render dimmed.

import { useHistory, useJumpTo } from "../ipc/queries";

const SOURCE_ICON: Record<string, string> = {
  ui: "✋",
  local_ai: "💬",
  mcp_client: "🔌",
};

function relativeTime(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "";
  const s = Math.max(0, (Date.now() - then) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

export function HistoryPanel({ projectId }: { projectId: string }) {
  const { data: history } = useHistory(projectId);
  const jump = useJumpTo();

  const entries = history?.entries ?? [];
  const current = history?.current_index ?? -1;
  const jumpTo = (id: string | null) =>
    jump.mutate({ project_id: projectId, action_id: id });

  // Newest first for reading; remember each row's original chronological index
  // so we can compare against current_index.
  const rows = entries.map((e, i) => ({ e, i })).reverse();

  return (
    <div className="history-panel">
      <p className="history-note">
        Every edit, newest first. Click any point to jump back (or forward) in
        time — nothing is lost, you can always jump again.
      </p>
      <div className="history-list">
        {rows.map(({ e, i }) => {
          const isCurrent = i === current;
          const isFuture = !e.applied;
          return (
            <button
              key={e.id}
              className={`history-row${isCurrent ? " current" : ""}${isFuture ? " future" : ""}`}
              disabled={jump.isPending || isCurrent}
              onClick={() => jumpTo(e.id)}
              title={isFuture ? "Jump forward to here (redo to this point)" : "Jump back to here"}
            >
              <span className="history-src" title={e.source}>
                {SOURCE_ICON[e.source] ?? "•"}
              </span>
              <span className="history-desc">{e.description}</span>
              <span className="history-time">{relativeTime(e.timestamp)}</span>
              {isCurrent && <span className="history-now">now</span>}
            </button>
          );
        })}
        <button
          className={`history-row original${current === -1 ? " current" : ""}`}
          disabled={jump.isPending || current === -1}
          onClick={() => jumpTo(null)}
          title="Jump all the way back to the original, before any edit"
        >
          <span className="history-src">↩</span>
          <span className="history-desc">Original — before any edit</span>
          {current === -1 && <span className="history-now">now</span>}
        </button>
      </div>
    </div>
  );
}
