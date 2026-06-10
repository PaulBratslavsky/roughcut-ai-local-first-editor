import { useEffect, useState } from "react";
import { useStore } from "@tanstack/react-store";
import { getDemoMode, isTauri } from "./ipc/api";
import { useCoreEventInvalidation, useProject, useProjects } from "./ipc/queries";
import { setActiveTab, setProjectId, togglePlaying, viewStore } from "./state/viewStore";
import { EmptyState } from "./EmptyState";
import { TopBar } from "./panels/TopBar";
import { TranscriptPanel } from "./panels/TranscriptPanel";
import { ChatPanel } from "./panels/ChatPanel";
import { PreviewPanel } from "./panels/PreviewPanel";
import { InspectorPanel } from "./panels/InspectorPanel";
import { Timeline } from "./timeline/Timeline";
import { TransportBar } from "./timeline/TransportBar";

function Editor({ projectId }: { projectId: string }) {
  const { data: project } = useProject(projectId);
  const activeTab = useStore(viewStore, (s) => s.activeTab);

  // Space toggles playback (when not typing).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable) return;
      if (e.code === "Space") {
        e.preventDefault();
        togglePlaying();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app-shell">
      <TopBar projectId={projectId} projectName={project?.name ?? "…"} />
      <main className="main-row">
        <section className="left-panel card light">
          <div className="tab-switcher">
            <button
              className={`tab${activeTab === "script" ? " active" : ""}`}
              onClick={() => setActiveTab("script")}
            >
              Script
            </button>
            <button
              className={`tab${activeTab === "chat" ? " active" : ""}`}
              onClick={() => setActiveTab("chat")}
            >
              Chat
            </button>
          </div>
          {activeTab === "script" ? (
            <TranscriptPanel projectId={projectId} />
          ) : (
            <ChatPanel projectId={projectId} />
          )}
        </section>
        <aside className="right-col">
          <PreviewPanel projectId={projectId} />
          <InspectorPanel projectId={projectId} />
        </aside>
      </main>
      <footer className="bottom-area">
        <TransportBar projectId={projectId} />
        <Timeline projectId={projectId} />
      </footer>
    </div>
  );
}

/** Honest labelling: when the backend is on fixture adapters, say so —
 *  otherwise demo footage reads as "the app is broken". */
function DemoBanner() {
  const [demo, setDemo] = useState(false);
  useEffect(() => {
    void getDemoMode().then(setDemo).catch(() => setDemo(false));
  }, []);
  if (!demo) return null;
  return (
    <div className="demo-banner">
      {isTauri
        ? "Demo mode — editing fixture footage. Finish setup (ffmpeg + speech model) to use your own video."
        : "Browser demo — running on fixture data. The desktop app edits real footage."}
    </div>
  );
}

export default function App() {
  useCoreEventInvalidation();
  const projects = useProjects();
  const selected = useStore(viewStore, (s) => s.projectId);
  const list = projects.data?.projects ?? [];
  // The user's pick wins (if it still exists); otherwise the most recent project.
  const projectId =
    (selected && list.some((p) => p.id === selected) ? selected : list[0]?.id) ?? null;

  useEffect(() => {
    setProjectId(projectId);
  }, [projectId]);

  if (projects.isLoading) {
    return (
      <div className="app-loading">
        <span>Loading…</span>
      </div>
    );
  }

  return (
    <>
      <DemoBanner />
      {projectId ? <Editor projectId={projectId} /> : <EmptyState />}
    </>
  );
}
