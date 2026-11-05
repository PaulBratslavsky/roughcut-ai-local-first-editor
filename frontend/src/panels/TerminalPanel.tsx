// In-app terminal: a real login shell (xterm.js ↔ Rust PTY) for driving the
// MCP server — or anything else — without leaving the app. Local-first stays
// the default; this is an optional power-user surface.
//
// The shell is spawned with ROUGHCUT_MCP_URL/_TOKEN in its env (when the MCP
// server is up), so "Connect Claude Code" just types a command that references
// them — no secrets on screen. It types only at the shell prompt: while a
// program such as `claude` runs, it says so instead.
//
// The session itself lives in state/terminalSession.ts: this panel only shows
// it. The editor's Terminal tab and the Shorts screen's drawer are views of
// the same shell, and closing either leaves it running.

import { useEffect, useRef, useState } from "react";
import { useStore } from "@tanstack/react-store";
import "@xterm/xterm/css/xterm.css";
import { isTauri } from "../ipc/api";
import { attachTerminal, restartTerminal, terminalStore, typeAtPrompt } from "../state/terminalSession";

// Safe to click again: drop this folder's earlier registration (it may hold
// an old port or token), add the current one, and print one line instead of
// the add command's own output (which could echo the token). Errors still
// show. Only typed at the shell prompt (see typeAtPrompt).
const CONNECT_CMD =
  "claude mcp remove roughcut -s local >/dev/null 2>&1; " +
  'claude mcp add --transport http roughcut "$ROUGHCUT_MCP_URL" ' +
  '--header "Authorization: Bearer $ROUGHCUT_MCP_TOKEN" >/dev/null && ' +
  'echo "RoughCut is connected. Now run: claude"\n';

export function TerminalPanel() {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const exited = useStore(terminalStore, (s) => s.exited);
  // Shown in place of the hint after a click while a program was running.
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!busy) return;
    const t = setTimeout(() => setBusy(false), 8000);
    return () => clearTimeout(t);
  }, [busy]);

  const connect = async () => {
    setBusy(!(await typeAtPrompt(CONNECT_CMD)));
  };

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
          onClick={() => void connect()}
          disabled={exited !== "running"}
          title="Register this app with Claude Code (refreshes it if already registered)"
        >
          Connect Claude Code
        </button>
        {busy ? (
          <span className="terminal-hint" role="status">
            a program is running in the terminal — quit it (for <code>claude</code>, <code>/exit</code>), then click again
          </span>
        ) : (
          <span className="terminal-hint">
            at a shell prompt, then run <code>claude</code>
          </span>
        )}
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
