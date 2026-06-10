// Top chrome: project switcher, new-video button, undo/redo, export menu.

import { useQueryClient } from "@tanstack/react-query";
import { ask } from "@tauri-apps/plugin-dialog";
import { callTool, isTauri } from "../ipc/api";
import { useProjects, useRedo, useUndo } from "../ipc/queries";
import { setProjectId } from "../state/viewStore";
import { newProjectFromDialog } from "../newProject";
import { ExportMenu } from "./ExportMenu";

export function TopBar({
  projectId,
  projectName,
}: {
  projectId: string;
  projectName: string;
}) {
  const undo = useUndo();
  const redo = useRedo();
  const projects = useProjects();
  const queryClient = useQueryClient();

  const onNewVideo = async () => {
    const id = await newProjectFromDialog();
    if (id) {
      setProjectId(id);
      await queryClient.invalidateQueries();
    }
  };

  const onDeleteProject = async () => {
    const question = `Delete project “${projectName}”? Cuts, transcript and undo history are removed. The video file itself is never touched.`;
    const confirmed = isTauri
      ? await ask(question, { title: "Delete project", kind: "warning" })
      : window.confirm(question);
    if (!confirmed) return;
    await callTool("delete_project", { project_id: projectId });
    setProjectId(null);
    await queryClient.invalidateQueries();
  };

  const list = projects.data?.projects ?? [];

  return (
    <header className="topbar">
      <div className="topbar-left">
        <span className="app-mark">Fable</span>
        {list.length > 1 ? (
          <select
            className="project-select"
            value={projectId}
            title="Switch project"
            onChange={(e) => setProjectId(e.target.value)}
          >
            {list.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        ) : (
          <span className="project-name" title={projectName}>
            {projectName}
          </span>
        )}
        <button className="new-video-btn" title="Import another video" onClick={() => void onNewVideo()}>
          + New video
        </button>
        <button
          className="icon-btn delete-project-btn"
          title="Delete this project (keeps the video file)"
          onClick={() => void onDeleteProject()}
        >
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
            <path d="M2.5 4h11M6.5 4V2.8a.8.8 0 0 1 .8-.8h1.4a.8.8 0 0 1 .8.8V4M4 4l.7 9.2a1 1 0 0 0 1 .8h4.6a1 1 0 0 0 1-.8L12 4" />
            <path d="M6.5 7v4.5M9.5 7v4.5" />
          </svg>
        </button>
      </div>
      <div className="topbar-right">
        <button
          className="icon-btn"
          title="Undo"
          onClick={() => undo.mutate({ project_id: projectId })}
          disabled={undo.isPending}
        >
          <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
            <path d="M6 3 2.5 6.5 6 10" />
            <path d="M2.5 6.5H10a3.5 3.5 0 0 1 0 7H7" />
          </svg>
        </button>
        <button
          className="icon-btn"
          title="Redo"
          onClick={() => redo.mutate({ project_id: projectId })}
          disabled={redo.isPending}
        >
          <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
            <path d="M10 3l3.5 3.5L10 10" />
            <path d="M13.5 6.5H6a3.5 3.5 0 0 0 0 7h3" />
          </svg>
        </button>
        <ExportMenu projectId={projectId} projectName={projectName} />
      </div>
    </header>
  );
}
