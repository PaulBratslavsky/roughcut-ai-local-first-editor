// The drawer under the player on the Shorts and Clips screens: ask for
// changes while you review, in the app's own chat (Local or Hosted, the same
// conversation as the editor's Chat tab) or the app's one shell for Claude
// Code (the same session as the Terminal tab). Whether it's open, and which
// tab, is shared by both screens (viewStore).

import { useStore } from "@tanstack/react-store";
import { modelLabel, useModelStatus } from "../state/modelStore";
import { showShortsDrawerTab, toggleShortsDrawer, viewStore, type ShortsDrawerTab } from "../state/viewStore";
import { ChatPanel } from "./ChatPanel";
import { TerminalPanel } from "./TerminalPanel";

const DRAWER_TABS: ShortsDrawerTab[] = ["chat", "terminal"];

export function PlayerDrawer({ projectId }: { projectId: string }) {
  const open = useStore(viewStore, (v) => v.shortsDrawerOpen);
  const tab = useStore(viewStore, (v) => v.shortsDrawerTab);
  const model = useModelStatus();
  return (
    <div className={`shorts-drawer${open ? " open" : ""}`}>
      <div className="shorts-drawer-bar">
        <button
          className="shorts-drawer-toggle"
          onClick={toggleShortsDrawer}
          aria-expanded={open}
          aria-label={open ? "Hide the chat and terminal" : "Show the chat and terminal"}
          title={open ? "Hide (the chat and the terminal keep running)" : "Show"}
        >
          {open ? "▾" : "▸"}
        </button>
        <div className="shorts-drawer-tabs" role="tablist" aria-label="Drawer">
          {DRAWER_TABS.map((t) => (
            <button
              key={t}
              role="tab"
              aria-selected={open && tab === t}
              className={`tab${open && tab === t ? " active" : ""}`}
              onClick={() => showShortsDrawerTab(t)}
            >
              {t}
            </button>
          ))}
        </div>
        <span className="shorts-drawer-hint">
          {tab === "chat" ? `the app's assistant · runs on ${modelLabel(model)}` : "Claude Code over MCP · needs internet"}
        </span>
      </div>
      {open && (tab === "chat" ? <ChatPanel projectId={projectId} /> : <TerminalPanel />)}
    </div>
  );
}
