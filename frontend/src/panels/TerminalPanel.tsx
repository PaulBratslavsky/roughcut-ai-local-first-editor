// In-app terminal: a real login shell (xterm.js ↔ Rust PTY) for driving the
// MCP server — or anything else — without leaving the app. Local-first stays
// the default; this is an optional power-user surface.
//
// The shell is spawned with ROUGHCUT_MCP_URL/_TOKEN in its env (when the MCP
// server is up), so "Connect Claude Code" just types a command that references
// them — no secrets on screen.

import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { isTauri, onAppEvent, ptyKill, ptyResize, ptySpawn, ptyWrite } from "../ipc/api";
import type { TerminalExitEvent, TerminalOutputEvent } from "../ipc/types";

/** base64 (raw PTY bytes) → Uint8Array for term.write — survives multibyte
 *  characters split across read boundaries, which a string decode would drop. */
function decode(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

const CONNECT_CMD =
  'claude mcp add --transport http roughcut "$ROUGHCUT_MCP_URL" ' +
  '--header "Authorization: Bearer $ROUGHCUT_MCP_TOKEN"\n';

export function TerminalPanel() {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const idRef = useRef<string>("");
  const [exited, setExited] = useState<number | null | "running">("running");
  // Bumped by "Restart" to re-run the init effect with a fresh session.
  const [gen, setGen] = useState(0);

  useEffect(() => {
    if (!isTauri || !hostRef.current) return;
    const id = crypto.randomUUID();
    idRef.current = id;
    setExited("running");

    const term = new Terminal({
      cursorBlink: true,
      fontFamily: 'ui-monospace, "SF Mono", Menlo, monospace',
      fontSize: 13,
      theme: { background: "#0e0e0e", foreground: "#e6e6e6", cursor: "#e6e6e6" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(hostRef.current);
    // WebGL is a perf win but can fail (driver/context limits) — degrade to DOM.
    try {
      term.loadAddon(new WebglAddon());
    } catch {
      /* DOM renderer is fine */
    }
    fit.fit();
    termRef.current = term;

    term.onData((d) => void ptyWrite(id, d));

    const offOut = onAppEvent<TerminalOutputEvent>("terminal-output", (e) => {
      if (e.id === id) term.write(decode(e.data));
    });
    const offExit = onAppEvent<TerminalExitEvent>("terminal-exit", (e) => {
      if (e.id === id) setExited(e.code);
    });

    const ro = new ResizeObserver(() => {
      try {
        fit.fit();
        void ptyResize(id, term.cols, term.rows);
      } catch {
        /* host not measurable yet */
      }
    });
    ro.observe(hostRef.current);

    void ptySpawn(id, term.cols, term.rows).then(() => term.focus());

    return () => {
      offOut();
      offExit();
      ro.disconnect();
      void ptyKill(id);
      term.dispose();
      termRef.current = null;
    };
  }, [gen]);

  if (!isTauri) {
    return (
      <div className="terminal-panel">
        <div className="terminal-placeholder">The terminal is only available in the desktop app.</div>
      </div>
    );
  }

  const connect = () => {
    ptyWrite(idRef.current, CONNECT_CMD);
    termRef.current?.focus();
  };

  return (
    <div className="terminal-panel">
      <div className="terminal-toolbar">
        <button className="terminal-btn" onClick={connect} title="Register this app's MCP server with Claude Code">
          Connect Claude Code
        </button>
        <span className="terminal-hint">runs <code>claude mcp add</code> against the running server</span>
      </div>
      <div className="terminal-host" ref={hostRef} />
      {exited !== "running" && (
        <div className="terminal-exit-bar">
          <span>Shell exited{typeof exited === "number" ? ` (code ${exited})` : ""}.</span>
          <button className="terminal-btn" onClick={() => setGen((g) => g + 1)}>
            Restart
          </button>
        </div>
      )}
    </div>
  );
}
