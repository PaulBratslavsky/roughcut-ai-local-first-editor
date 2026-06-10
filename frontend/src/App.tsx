import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import { onAppEvent } from "./ipc/api";
import type { ConfirmRequestEvent } from "./ipc/types";
import { useStore } from "@tanstack/react-store";
import { getDemoMode, isTauri } from "./ipc/api";
import { useCoreEventInvalidation, useProject, useProjects, useTranscript } from "./ipc/queries";
import { scrubBy, scrubTo, setActiveTab, setProjectId, togglePlaying, viewStore } from "./state/viewStore";
import { EmptyState } from "./EmptyState";
import { TopBar } from "./panels/TopBar";
import { TranscriptPanel } from "./panels/TranscriptPanel";
import { ChatPanel } from "./panels/ChatPanel";
import { PreviewPanel } from "./panels/PreviewPanel";
import { InspectorPanel } from "./panels/InspectorPanel";
import { MetadataPanel } from "./panels/MetadataPanel";
import { Timeline } from "./timeline/Timeline";
import { TransportBar } from "./timeline/TransportBar";

function Editor({ projectId }: { projectId: string }) {
  const { data: project } = useProject(projectId);
  const { data: transcript } = useTranscript(projectId);
  const activeTab = useStore(viewStore, (s) => s.activeTab);

  // Flat, time-ordered word list for ←/→ word navigation.
  const wordsRef = useRef<{ start: number; end: number }[]>([]);
  useEffect(() => {
    wordsRef.current = (transcript?.segments ?? [])
      .filter((seg) => !seg.is_silence)
      .flatMap((seg) => seg.words.map((w) => ({ start: w.start, end: w.end })))
      .sort((a, b) => a.start - b.start);
  }, [transcript]);

  // Space toggles playback (when not typing).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable) return;
      if (e.code === "Space") {
        e.preventDefault();
        togglePlaying();
      } else if (e.code === "ArrowLeft" || e.code === "ArrowRight") {
        e.preventDefault();
        const max = project?.timeline.duration ?? Number.MAX_SAFE_INTEGER;
        const back = e.code === "ArrowLeft";
        const words = wordsRef.current;
        if (e.shiftKey) {
          scrubBy(back ? -1 : 1, max); // coarse: 1 second
        } else if (e.altKey || words.length === 0) {
          const fps = project?.media?.frame_rate || 30;
          scrubBy((back ? -1 : 1) / fps, max); // fine: 1 frame (⌥)
        } else {
          // Default: walk the transcript word by word, auditioning the word.
          const t = viewStore.state.playhead;
          const target = back
            ? [...words].reverse().find((w) => w.start < t - 0.02)
            : words.find((w) => w.start > t + 0.02);
          if (target) {
            const ms = Math.min(700, Math.max(160, (target.end - target.start) * 1000));
            scrubTo(target.start, max, ms);
          }
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project?.media?.frame_rate, project?.timeline.duration]);

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
          <MetadataPanel projectId={projectId} />
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
