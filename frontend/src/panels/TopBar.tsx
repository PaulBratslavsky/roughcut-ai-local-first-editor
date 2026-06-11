// Top chrome: project switcher, new-video button, undo/redo, export menu.

import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ask } from "@tauri-apps/plugin-dialog";
import { callTool, isTauri } from "../ipc/api";
import { useProjects, useRedo, useRestoreProject, useUndo } from "../ipc/queries";
import { setProjectId, setScreen } from "../state/viewStore";
import { newProjectFromDialog } from "../newProject";
import { ExportMenu } from "./ExportMenu";
import { currentTheme, toggleTheme } from "../theme";
import { SetupPanel } from "./SetupPanel";

export function TopBar({
  projectId,
  projectName,
}: {
  projectId: string;
  projectName: string;
}) {
  const undo = useUndo();
  const redo = useRedo();
  const restoreProject = useRestoreProject();
  const projects = useProjects();
  const queryClient = useQueryClient();
  const [showSetup, setShowSetup] = useState(false);
  const [theme, setTheme] = useState(currentTheme());

  const [newOpen, setNewOpen] = useState(false);
  const onNewVideo = async () => {
    const id = await newProjectFromDialog();
    if (id) {
      setProjectId(id);
      await queryClient.invalidateQueries();
    }
  };

  const onDeleteProject = async () => {
    const question = `Move “${projectName}” to the trash? Restore it any time in the next 30 days from the project menu. The video file itself is never touched.`;
    const confirmed = isTauri
      ? await ask(question, { title: "Move to trash", kind: "warning" })
      : window.confirm(question);
    if (!confirmed) return;
    await callTool("delete_project", { project_id: projectId });
    setProjectId(null);
    await queryClient.invalidateQueries();
  };

  const list = projects.data?.projects ?? [];
  const trash = projects.data?.trash ?? [];

  const onSelectProject = (value: string) => {
    if (value.startsWith("trash:")) {
      const id = value.slice("trash:".length);
      restoreProject.mutate(
        { project_id: id },
        { onSuccess: () => setProjectId(id) },
      );
      return;
    }
    setProjectId(value);
  };

  return (
    <header className="topbar">
      <div className="topbar-left">
        <span className="app-mark">Fable</span>
        {list.length > 1 || trash.length > 0 ? (
          <select
            className="project-select"
            value={projectId}
            title="Switch project — trashed projects restore when selected"
            onChange={(e) => onSelectProject(e.target.value)}
          >
            {list.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
            {trash.length > 0 && (
              <optgroup label="trash — select to restore">
                {trash.map((p) => (
                  <option key={p.id} value={`trash:${p.id}`}>
                    ↩ {p.name}
                  </option>
                ))}
              </optgroup>
            )}
          </select>
        ) : (
          <span className="project-name" title={projectName}>
            {projectName}
          </span>
        )}
        <div className="export-menu new-video-menu">
          <button
            className="new-video-btn"
            title="Load a video file or record a new one"
            onClick={() => setNewOpen((v) => !v)}
          >
            + New video
          </button>
          {newOpen && (
            <div className="export-dropdown">
              <button
                className="export-item"
                onClick={() => {
                  setNewOpen(false);
                  void onNewVideo();
                }}
              >
                ▸ Open a video file…
              </button>
              {isTauri && (
                <button
                  className="export-item"
                  onClick={() => {
                    setNewOpen(false);
                    setScreen("recorder");
                  }}
                >
                  ● Record camera
                </button>
              )}
            </div>
          )}
        </div>
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
          title={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
          onClick={() => setTheme(toggleTheme())}
        >
          {theme === "dark" ? (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7">
              <circle cx="12" cy="12" r="4.2" />
              <path d="M12 2.5v2.4M12 19.1v2.4M2.5 12h2.4M19.1 12h2.4M4.9 4.9l1.7 1.7M17.4 17.4l1.7 1.7M19.1 4.9l-1.7 1.7M6.6 17.4l-1.7 1.7" />
            </svg>
          ) : (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7">
              <path d="M20.5 14.5A8.5 8.5 0 0 1 9.5 3.5a8.5 8.5 0 1 0 11 11z" />
            </svg>
          )}
        </button>
        <button className="icon-btn" title="Setup (models & toolchain)" onClick={() => setShowSetup(true)}>
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7">
            <circle cx="12" cy="12" r="3.2" />
            <path d="M19.4 13.5a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1.03 1.56V19.6a2 2 0 1 1-4 0v-.09A1.7 1.7 0 0 0 8.98 18a1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.7 1.7 0 0 0 .34-1.87 1.7 1.7 0 0 0-1.56-1.03H2.9a2 2 0 1 1 0-4h.09A1.7 1.7 0 0 0 4.55 7.6a1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.7 1.7 0 0 0 1.87.34h.0a1.7 1.7 0 0 0 1.03-1.56V1.6a2 2 0 1 1 4 0v.09c0 .68.4 1.3 1.03 1.56a1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.7 1.7 0 0 0-.34 1.87v.0c.26.63.88 1.03 1.56 1.03h.09a2 2 0 1 1 0 4h-.09c-.68 0-1.3.4-1.56 1.03z" />
          </svg>
        </button>
        {showSetup && <SetupPanel onClose={() => setShowSetup(false)} />}
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
