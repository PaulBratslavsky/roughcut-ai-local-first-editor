// Export button + dropdown of targets. Calls the `export` tool with a
// sensible Downloads path for the chosen format.

import { useEffect, useRef, useState } from "react";
import { revealPath } from "../ipc/api";
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
  const [status, setStatus] = useState<{ text: string; path?: string } | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const exportMutation = useExport();

  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [open]);

  useEffect(() => {
    if (!status) return;
    const t = setTimeout(() => setStatus(null), status.path ? 12000 : 4000);
    return () => clearTimeout(t);
  }, [status]);

  const slug = projectName.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "project";

  const run = (target: ExportTarget, ext: string, label: string) => {
    setOpen(false);
    setStatus({ text: `Exporting ${label}…` });
    exportMutation.mutate(
      {
        project_id: projectId,
        target,
        out_path: `~/Downloads/${slug}-export.${ext}`,
      },
      {
        onSuccess: (res) => {
          setStatus({ text: `Saved ${res.path.split("/").pop()}`, path: res.path });
          // Surface the result physically: open Finder with the file selected.
          void revealPath(res.path);
        },
        onError: (err) => setStatus({ text: `Export failed: ${String((err as Error)?.message ?? err)}` }),
      },
    );
  };

  return (
    <div className="export-menu" ref={rootRef}>
      {status && (
        <span className="export-status" title={status.path}>
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
