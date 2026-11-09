// The bar across the top while a saved clip is open in the Editor
// (`begin_clip_edit`): which clip, how long it plays now, Save and Cancel.
// The core swaps the clip's timeline in, so the transcript, timeline, tools,
// chat and undo all edit the clip; this bar only closes the session. It also
// drives the view: cut material hidden and the playhead at the clip while it
// is open, the view as it was once it closes, however the session started
// (the "edit" button, an MCP client, or a restart with a clip still open).

import { useEffect, useState } from "react";
import { ask } from "@tauri-apps/plugin-dialog";
import { isTauri } from "../ipc/api";
import { useClips, useEndClipEdit, useHistory, useProject, useTimeline } from "../ipc/queries";
import { enterClipEditView, leaveClipEditView, setScreen } from "../state/viewStore";
import { clock, errText } from "./clipUtil";

export function ClipEditBar({ projectId }: { projectId: string }) {
  const { data: project } = useProject(projectId);
  const { data: timeline } = useTimeline(projectId);
  const { data: clips } = useClips(projectId);
  const { data: history } = useHistory(projectId);
  const end = useEndClipEdit();
  const [error, setError] = useState<string | null>(null);

  const session = project?.clip_session ?? null;
  const clip = session ? clips?.find((c) => c.id === session.clip_id) : undefined;
  const clipStart = clip?.start ?? null;

  useEffect(() => {
    if (session) enterClipEditView(clipStart);
    else if (project) leaveClipEditView();
  }, [session?.clip_id, clipStart, project === undefined]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => setError(null), [session?.clip_id]);

  if (!session) return null;
  const plays = (timeline?.clips ?? []).filter((c) => c.included).reduce((a, c) => a + c.source_out - c.source_in, 0);
  const was = clip?.duration ?? null;
  const changed = (history?.entries.length ?? 0) > 0;

  const close = (save: boolean) => {
    setError(null);
    end.mutate(
      { project_id: projectId, save },
      {
        onSuccess: () => setScreen("clips"),
        onError: (e) => setError(errText(e)),
      },
    );
  };
  const cancel = async () => {
    if (changed) {
      const question = `Drop your changes to “${session.title}”? The clip stays as it was saved.`;
      const ok = isTauri ? await ask(question, { title: "Cancel editing", kind: "warning" }) : window.confirm(question);
      if (!ok) return;
    }
    close(false);
  };

  return (
    <div className="clip-edit-bar" role="region" aria-label="Editing a saved clip">
      <span className="clip-edit-tag">editing clip</span>
      <span className="clip-edit-title" title={session.title}>
        {session.title}
      </span>
      <span className="clip-edit-len">
        {clock(plays)}
        {was !== null && Math.abs(was - plays) > 0.05 && ` · was ${clock(was)}`}
      </span>
      <span className="clip-edit-note">
        edits change only this clip · restore words just outside it to extend it · your main edit is set aside
      </span>
      <span className="clip-edit-spacer" />
      {error && (
        <span className="clip-edit-error" role="alert">
          {error}
        </span>
      )}
      <button className="ghost-btn" onClick={() => void cancel()} disabled={end.isPending}>
        Cancel
      </button>
      <button className="primary-btn" onClick={() => close(true)} disabled={end.isPending}>
        {end.isPending ? "Saving…" : "Save clip"}
      </button>
    </div>
  );
}
