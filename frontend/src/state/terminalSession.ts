// One shell for the whole app. The editor's Terminal tab and the Shorts
// screen's drawer show the SAME session, and switching tabs or screens never
// kills it: a `claude` running in it keeps going while you look elsewhere.
// Only "Restart" ends it (the Rust side also kills it when the app quits).
//
// The xterm instance lives here, in a detached element that panels move into
// their own host while they're mounted. Output keeps landing in its buffer
// while no panel shows it, so nothing is missed.

import { Store } from "@tanstack/react-store";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { isTauri, onAppEvent, ptyKill, ptyResize, ptySpawn, ptyWrite } from "../ipc/api";
import type { TerminalExitEvent, TerminalOutputEvent } from "../ipc/types";

/** "running", or the shell's exit code (null when it gave none). */
export type TerminalState = { exited: "running" | number | null };

export const terminalStore = new Store<TerminalState>({ exited: "running" });

interface Session {
  id: string;
  term: Terminal;
  fit: FitAddon;
  /** The element xterm renders into; moved between panel hosts. */
  el: HTMLDivElement;
  opened: boolean;
  spawned: boolean;
  off: () => void;
}

let session: Session | null = null;
/** The panel host currently showing the terminal (at most one at a time). */
let host: HTMLElement | null = null;
let observer: ResizeObserver | null = null;

/** base64 (raw PTY bytes) → Uint8Array for term.write — survives multibyte
 *  characters split across read boundaries, which a string decode would drop. */
function decode(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function create(): Session {
  const id = crypto.randomUUID();
  const term = new Terminal({
    cursorBlink: true,
    fontFamily: 'ui-monospace, "SF Mono", Menlo, monospace',
    fontSize: 13,
    theme: { background: "#0e0e0e", foreground: "#e6e6e6", cursor: "#e6e6e6" },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.onData((d) => void ptyWrite(id, d));
  const el = document.createElement("div");
  el.className = "terminal-surface";
  const offOut = onAppEvent<TerminalOutputEvent>("terminal-output", (e) => {
    if (e.id === id) term.write(decode(e.data));
  });
  const offExit = onAppEvent<TerminalExitEvent>("terminal-exit", (e) => {
    if (e.id === id) terminalStore.setState(() => ({ exited: e.code }));
  });
  terminalStore.setState(() => ({ exited: "running" }));
  return {
    id,
    term,
    fit,
    el,
    opened: false,
    spawned: false,
    off: () => {
      offOut();
      offExit();
    },
  };
}

/** Fit the grid to the host and tell the PTY. Skipped while the host has no
 *  size (collapsed, or not laid out yet). */
function fitNow(): void {
  const s = session;
  if (!s || !s.opened || !host || host.clientWidth === 0 || host.clientHeight === 0) return;
  try {
    s.fit.fit();
    if (s.spawned) void ptyResize(s.id, s.term.cols, s.term.rows);
  } catch {
    /* host not measurable yet */
  }
}

function show(container: HTMLElement): void {
  if (!session) session = create();
  const s = session;
  host = container;
  container.appendChild(s.el);
  if (!s.opened) {
    s.term.open(s.el);
    // WebGL is a perf win but can fail (driver/context limits) — degrade to DOM.
    try {
      s.term.loadAddon(new WebglAddon());
    } catch {
      /* DOM renderer is fine */
    }
    s.opened = true;
  }
  fitNow();
  // Repaint what arrived while the terminal was off screen.
  s.term.refresh(0, s.term.rows - 1);
  if (!s.spawned) {
    s.spawned = true;
    void ptySpawn(s.id, s.term.cols, s.term.rows).then(() => s.term.focus());
  }
  observer?.disconnect();
  observer = new ResizeObserver(fitNow);
  observer.observe(container);
  s.term.focus();
}

/** Show the app's terminal in `container`, starting the shell on first use.
 *  Returns the matching detach, which hides it again but keeps it running. */
export function attachTerminal(container: HTMLElement): () => void {
  if (!isTauri) return () => {};
  show(container);
  return () => {
    // A newer host may already have taken the terminal; leave that one alone.
    if (host !== container) return;
    observer?.disconnect();
    observer = null;
    host = null;
    if (session && session.el.parentElement === container) container.removeChild(session.el);
  };
}

/** End the shell and start a fresh one in the same host. */
export function restartTerminal(): void {
  const s = session;
  if (!s) return;
  const h = host;
  void ptyKill(s.id);
  s.off();
  s.term.dispose();
  s.el.remove();
  session = null;
  if (h) show(h);
}

/** Type into the shell (e.g. the "Connect Claude Code" command). */
export function writeToTerminal(data: string): void {
  if (!session) return;
  void ptyWrite(session.id, data);
  session.term.focus();
}
