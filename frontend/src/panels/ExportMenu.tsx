// Export button + dropdown of targets. Calls the `export` tool with a
// sensible Downloads path for the chosen format.

import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { getResolveStatus, isTauri, onAppEvent, revealPath, sendToResolve } from "../ipc/api";
import type { ProgressEvent } from "../ipc/types";
import { useExport } from "../ipc/queries";
import type { ExportTarget } from "../ipc/types";

const TARGETS: { target: ExportTarget; label: string; ext: string }[] = [
  { target: "premiere_xml", label: "Premiere XML", ext: "xml" },
  { target: "fcp_xml", label: "Final Cut XML", ext: "fcpxml" },
  { target: "resolve_xml", label: "Resolve XML", ext: "xml" },
  { target: "edl", label: "EDL", ext: "edl" },
  { target: "otio", label: "OTIO", ext: "otio" },
  { target: "mp4", label: "MP4", ext: "mp4" },
  { target: "srt", label: "SRT captions", ext: "srt" },
];

export function ExportMenu({
  projectId,
  projectName,
}: {
  projectId: string;
  projectName: string;
}) {
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<{
    text: string;
    path?: string;
    fraction?: number;
    busy?: boolean;
  } | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const exportMutation = useExport();
  // Filesystem-only check (no probes); shows the Resolve item when it can work.
  const { data: resolve } = useQuery({
    queryKey: ["resolve-status"],
    queryFn: getResolveStatus,
    enabled: isTauri,
    staleTime: 60_000,
  });

  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [open]);

  // Only terminal states auto-clear — an in-flight export stays visible
  // for as long as it actually runs.
  useEffect(() => {
    if (!status || status.busy) return;
    const t = setTimeout(() => setStatus(null), status.path ? 15000 : 6000);
    return () => clearTimeout(t);
  }, [status]);

  // Live render progress from the core (ffmpeg percentage).
  useEffect(() => {
    return onAppEvent<ProgressEvent>("progress", (p) => {
      if (p.task !== "export") return;
      setStatus((cur) =>
        cur?.busy || p.fraction < 1
          ? { text: p.message, fraction: p.fraction, busy: p.fraction < 1, path: cur?.path }
          : cur,
      );
    });
  }, []);

  const slug = projectName.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "project";

  const run = (target: ExportTarget, ext: string, label: string) => {
    setOpen(false);
    setStatus({ text: `Exporting ${label}…`, busy: true, fraction: 0 });
    exportMutation.mutate(
      {
        project_id: projectId,
        target,
        out_path: `~/Downloads/${slug}-export.${ext}`,
      },
      {
        onSuccess: (res) => {
          setStatus({ text: `Saved ${res.path.split("/").pop()}`, path: res.path, busy: false });
          // Surface the result physically: open Finder with the file selected.
          void revealPath(res.path);
        },
        onError: (err) =>
          setStatus({ text: `Export failed: ${String((err as Error)?.message ?? err)}`, busy: false }),
      },
    );
  };

  // One click: export the current cut as Resolve XML and push it into the
  // RUNNING Resolve. Failure names the precondition and keeps the XML
  // revealable for a manual drag into the media pool.
  const runSendToResolve = async () => {
    setOpen(false);
    setStatus({ text: "Sending the cut to DaVinci Resolve…", busy: true, fraction: 0 });
    try {
      const res = await sendToResolve(projectId);
      if (res.ok) {
        setStatus({ text: `In Resolve: “${res.timeline}” — check the media pool`, busy: false });
      } else {
        setStatus({
          text: `Resolve: ${res.error} — XML exported for a manual drag`,
          path: res.xml_path,
          busy: false,
        });
        void revealPath(res.xml_path);
      }
    } catch (err) {
      setStatus({ text: `Send failed: ${String((err as Error)?.message ?? err)}`, busy: false });
    }
  };

  return (
    <div className="export-menu" ref={rootRef}>
      {status && (
        <span className="export-status" title={status.path}>
          {status.busy && (
            <span className="export-bar">
              <span
                className="export-bar-fill"
                style={{ width: `${Math.round((status.fraction ?? 0) * 100)}%` }}
              />
            </span>
          )}
          {status.text}
          {status.path && (
            <button className="reveal-btn" onClick={() => void revealPath(status.path!)}>
              Reveal in Finder
            </button>
          )}
        </span>
      )}
      <button className="export-btn" onClick={() => setOpen((v) => !v)}>
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
          <path d="M8 10V2M5 5l3-3 3 3M3 10v3h10v-3" />
        </svg>
        Export
      </button>
      {open && (
        <div className="export-dropdown">
          {resolve?.app_installed && (
            <button
              className="export-item export-item-send"
              onClick={() => void runSendToResolve()}
              disabled={exportMutation.isPending}
              title="Export the current cut and import it into the running Resolve"
            >
              → Send to DaVinci Resolve
            </button>
          )}
          {TARGETS.map((t) => (
            <button
              key={t.target}
              className="export-item"
              onClick={() => run(t.target, t.ext, t.label)}
              disabled={exportMutation.isPending}
            >
              {t.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
