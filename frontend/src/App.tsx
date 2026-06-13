import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import { onAppEvent } from "./ipc/api";
import type { ConfirmRequestEvent } from "./ipc/types";
import { useStore } from "@tanstack/react-store";
import { getDemoMode, isTauri } from "./ipc/api";
import { useCoreEventInvalidation, useProject, useProjects, useTranscript } from "./ipc/queries";
import { useEditorKeyboard } from "./hooks/useEditorKeyboard";
import { setActiveTab, setProjectId, setScreen, viewStore } from "./state/viewStore";
import { EmptyState } from "./EmptyState";
import { RecorderPanel } from "./panels/RecorderPanel";
import { TopBar } from "./panels/TopBar";
import { TranscriptPanel } from "./panels/TranscriptPanel";
import { ChatPanel } from "./panels/ChatPanel";
import { ClipsPanel } from "./panels/ClipsPanel";
import { HistoryPanel } from "./panels/HistoryPanel";
import { PreviewPanel } from "./panels/PreviewPanel";
import { InspectorPanel } from "./panels/InspectorPanel";
import { MetadataPanel } from "./panels/MetadataPanel";
import { Timeline } from "./timeline/Timeline";
import { TransportBar } from "./timeline/TransportBar";

/** `?` opens the keymap; Esc or `?` again closes. The app is keyboard-first
 *  but the bindings were only discoverable from the statusline. */
const SHORTCUTS: [string, string][] = [
  ["Space", "play / pause"],
  ["← / →", "step word by word (with audio cue)"],
  ["⌥ ← / →", "step one frame"],
  ["⇧ ← / →", "step one second"],
  ["click word", "seek there"],
  ["select text → right-click", "cut / restore the selection"],
  ["⌘-click paragraphs", "multi-select for one cut"],
  ["right-click a timeline clip", "restore / cut / split"],
  ["scroll on timeline", "zoom around the playhead"],
  ["R", "open the recorder"],
  ["?", "this overlay"],
];

function ShortcutsOverlay({ onClose }: { onClose: () => void }) {
  return (
    <div className="setup-backdrop" onClick={onClose}>
      <div className="setup-panel card shortcuts-panel" onClick={(e) => e.stopPropagation()}>
        <div className="setup-header">
          <h2>Keyboard</h2>
          <button className="icon-btn" title="Close" onClick={onClose}>✕</button>
        </div>
        <table className="shortcuts-table">
          <tbody>
            {SHORTCUTS.map(([key, what]) => (
              <tr key={key}>
                <td className="shortcut-key">{key}</td>
                <td>{what}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function Editor({ projectId }: { projectId: string }) {
  const { data: project } = useProject(projectId);
  const { data: transcript } = useTranscript(projectId);
  const activeTab = useStore(viewStore, (s) => s.activeTab);
  useEditorKeyboard(project, transcript);
  const [showKeys, setShowKeys] = useState(false);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable) return;
      if (e.key === "?") setShowKeys((v) => !v);
      else if (e.key === "Escape") setShowKeys(false);
      else if ((e.key === "r" || e.key === "R") && isTauri && !e.metaKey && !e.ctrlKey)
        setScreen("recorder");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app-shell">
      <TopBar projectId={projectId} projectName={project?.name ?? "…"} />
      <main className="main-row">
        <section className="left-panel card light">
          <TranscriptPanel projectId={projectId} />
        </section>
        <aside className="right-col">
          <PreviewPanel projectId={projectId} />
          <div className="right-tabs card">
            <div className="tab-switcher">
              <button
                className={`tab${activeTab === "tools" ? " active" : ""}`}
                onClick={() => setActiveTab("tools")}
              >
                Tools
              </button>
              <button
                className={`tab${activeTab === "chat" ? " active" : ""}`}
                onClick={() => setActiveTab("chat")}
              >
                Chat
              </button>
              <button
                className={`tab${activeTab === "clips" ? " active" : ""}`}
                onClick={() => setActiveTab("clips")}
              >
                Clips
              </button>
              <button
                className={`tab${activeTab === "history" ? " active" : ""}`}
                onClick={() => setActiveTab("history")}
              >
                History
              </button>
            </div>
            {activeTab === "chat" ? (
              <ChatPanel projectId={projectId} />
            ) : activeTab === "clips" ? (
              <ClipsPanel projectId={projectId} />
            ) : activeTab === "history" ? (
              <HistoryPanel projectId={projectId} />
            ) : (
              <div className="tools-scroll">
                <InspectorPanel projectId={projectId} />
                <MetadataPanel projectId={projectId} />
              </div>
            )}
          </div>
        </aside>
      </main>
      {showKeys && <ShortcutsOverlay onClose={() => setShowKeys(false)} />}
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

/** External MCP clients asking for destructive ops (export, delete) get a
 *  native approval dialog — the user always has the last word. */
/** The webview's default right-click menu (Reload / Inspect Element) has no
 *  place in the editor; our own context menus preventDefault at the source. */
function useSuppressDefaultContextMenu() {
  useEffect(() => {
    if (!isTauri) return;
    const handler = (e: MouseEvent) => {
      const t = e.target as HTMLElement;
      if (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable) return;
      e.preventDefault();
    };
    document.addEventListener("contextmenu", handler);
    return () => document.removeEventListener("contextmenu", handler);
  }, []);
}

function useExternalConfirmations() {
  useEffect(() => {
    if (!isTauri) return;
    return onAppEvent<ConfirmRequestEvent>("confirm-request", (req) => {
      void ask(req.summary, { title: "External request", kind: "warning" }).then((approved) =>
        invoke("confirm_action", { id: req.id, approved }),
      );
    });
  }, []);
}

export default function App() {
  useCoreEventInvalidation();
  useExternalConfirmations();
  useSuppressDefaultContextMenu();
  const projects = useProjects();
  const selected = useStore(viewStore, (s) => s.projectId);
  const screen = useStore(viewStore, (s) => s.screen);
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

  if (screen === "recorder") {
    return (
      <>
        <DemoBanner />
        <RecorderPanel />
      </>
    );
  }

  return (
    <>
      <DemoBanner />
      {projectId ? <Editor projectId={projectId} /> : <EmptyState />}
    </>
  );
}
