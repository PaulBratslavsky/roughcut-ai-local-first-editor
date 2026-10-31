// Small helpers shared by the Clips tab, the Shorts screen and the clip rows.

import { useEffect, useState } from "react";
import type { ClipFormat } from "../ipc/types";

export const FORMAT_LABEL: Record<ClipFormat, string> = { landscape: "16:9", vertical: "9:16" };

export function clock(s: number): string {
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

export function errText(e: unknown): string {
  const o = e as { error?: { message?: string }; message?: string };
  return o?.error?.message ?? o?.message ?? String(e);
}

export function slugify(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 48) || "clip";
}

/** Whole seconds since `running` turned true (0 when idle). */
export function useElapsed(running: boolean): number {
  const [s, setS] = useState(0);
  useEffect(() => {
    if (!running) {
      setS(0);
      return;
    }
    const started = Date.now();
    const id = setInterval(() => setS(Math.floor((Date.now() - started) / 1000)), 1000);
    return () => clearInterval(id);
  }, [running]);
  return s;
}

/** Whole seconds since `since` (a Date.now() stamp), ticking; 0 when null.
 *  Unlike useElapsed it survives a remount mid-run (the stamp is stored). */
export function useSecondsSince(since: number | null): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (since === null) return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [since]);
  return since === null ? 0 : Math.max(0, Math.floor((now - since) / 1000));
}
