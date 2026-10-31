// In-app terminal: a real login shell (xterm.js ↔ Rust PTY) for driving the
// MCP server — or anything else — without leaving the app. Local-first stays
// the default; this is an optional power-user surface.
//
// The shell is spawned with ROUGHCUT_MCP_URL/_TOKEN in its env (when the MCP
// server is up), so "Connect Claude Code" just types a command that references
// them — no secrets on screen.
//
// The session itself lives in state/terminalSession.ts: this panel only shows
// it. The editor's Terminal tab and the Shorts screen's drawer are views of
// the same shell, and closing either leaves it running.

import { useEffect, useRef } from "react";
import { useStore } from "@tanstack/react-store";
import "@xterm/xterm/css/xterm.css";
import { isTauri } from "../ipc/api";
import { attachTerminal, restartTerminal, terminalStore, writeToTerminal } from "../state/terminalSession";

const CONNECT_CMD =
  'claude mcp add --transport http roughcut "$ROUGHCUT_MCP_URL" ' +
  '--header "Authorization: Bearer $ROUGHCUT_MCP_TOKEN"\n';

export function TerminalPanel() {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const exited = useStore(terminalStore, (s) => s.exited);

  useEffect(() => {
    if (!hostRef.current) return;
    return attachTerminal(hostRef.current);
  }, []);

  if (!isTauri) {
    return (
      <div className="terminal-panel">
        <div className="terminal-placeholder">The terminal is only available in the desktop app.</div>
      </div>
    );
  }

  return (
    <div className="terminal-panel">
      <div className="terminal-toolbar">
        <button
          className="terminal-btn"
          onClick={() => writeToTerminal(CONNECT_CMD)}
          title="Register this app's MCP server with Claude Code"
        >
          Connect Claude Code
        </button>
        <span className="terminal-hint">
          runs <code>claude mcp add</code> against the running server
        </span>
      </div>
      <div className="terminal-host" ref={hostRef} />
      {exited !== "running" && (
        <div className="terminal-exit-bar">
          <span>Shell exited{typeof exited === "number" ? ` (code ${exited})` : ""}.</span>
          <button className="terminal-btn" onClick={restartTerminal}>
            Restart
          </button>
        </div>
      )}
    </div>
  );
}
