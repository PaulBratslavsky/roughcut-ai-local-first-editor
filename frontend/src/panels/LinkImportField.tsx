// "Import from a link": paste a link, and the core downloads the video with
// yt-dlp into the download folder and creates a project around it
// (import_url, ADR-0009), then transcription starts as after a file import.
// Used by the empty screen and the top bar.

import { useEffect, useRef, useState } from "react";
import { onAppEvent } from "../ipc/api";
import type { ProgressEvent } from "../ipc/types";
import { ingestUrl } from "../ingest";
import { errText } from "./clipUtil";

export function LinkImportField({
  onCreated,
  onOpenSetup,
  autoFocus = false,
}: {
  /** The project exists (transcription carries on in the background). */
  onCreated: (projectId: string) => void;
  /** Link import is off: open Setup to turn it on. */
  onOpenSetup?: () => void;
  autoFocus?: boolean;
}) {
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [fraction, setFraction] = useState(0);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [off, setOff] = useState(false);

  useEffect(
    () =>
      onAppEvent<ProgressEvent>("progress", (p) => {
        if (p.task === "url_import" && busyRef.current) {
          setFraction(p.fraction);
          setMessage(p.message);
        }
      }),
    [],
  );

  const start = async () => {
    const link = url.trim();
    if (!link || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    setOff(false);
    setFraction(0);
    setMessage("Reading the link…");
    try {
      // Resolve once the project exists; a later transcription error is kept
      // on the project (ingestUrl), as for a file import.
      await new Promise<void>((resolve, reject) => {
        let created = false;
        ingestUrl(link, {
          onPhase: setMessage,
          onCreated: (id) => {
            created = true;
            onCreated(id);
            resolve();
          },
        }).catch((err: unknown) => {
          if (!created) reject(err);
        });
      });
      setUrl("");
    } catch (err) {
      const text = errText(err);
      setError(text);
      setOff(text.includes("link import is off"));
    } finally {
      busyRef.current = false;
      setBusy(false);
      setMessage(null);
    }
  };

  return (
    <div className="link-import">
      <form
        className="link-import-row"
        onSubmit={(e) => {
          e.preventDefault();
          void start();
        }}
      >
        <input
          className="link-import-input"
          type="url"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://www.youtube.com/watch?v=…"
          aria-label="Import from a link"
          disabled={busy}
          autoFocus={autoFocus}
        />
        <button className="ghost-btn" type="submit" disabled={busy || !url.trim()}>
          {busy ? "importing…" : "import"}
        </button>
      </form>
      {busy && (
        <div className="link-import-progress">
          <div className="link-import-bar">
            <span style={{ width: `${Math.round(fraction * 100)}%` }} />
          </div>
          <span className="link-import-message">{message}</span>
        </div>
      )}
      {error && (
        <p className="link-import-error">
          {error}
          {off && onOpenSetup && (
            <button className="ghost-btn" type="button" onClick={onOpenSetup}>
              open setup
            </button>
          )}
        </p>
      )}
      <p className="link-import-note">One video per link. Only import videos you have the right to use.</p>
    </div>
  );
}
