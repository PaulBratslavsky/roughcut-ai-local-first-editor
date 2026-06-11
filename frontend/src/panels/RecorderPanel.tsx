// The recorder screen (M1: camera + mic). Live preview runs on the webview's
// getUserMedia while ffmpeg owns the actual capture — the M1 spike is
// whether macOS lets both read the camera at once; if the preview track
// dies when recording starts, we say so instead of looking frozen.

import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import {
  isTauri,
  recordDevices,
  recordStart,
  recordStatus,
  recordStop,
  type CaptureDevices,
} from "../ipc/api";
import { ingestFile } from "../ingest";
import { setProjectId, setScreen } from "../state/viewStore";

type Phase = "setup" | "countdown" | "recording" | "finishing";

function fmtClock(s: number): string {
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

export function RecorderPanel() {
  const [devices, setDevices] = useState<CaptureDevices | null>(null);
  const [camera, setCamera] = useState<number | null>(null);
  const [mic, setMic] = useState<number | null>(null);
  const [phase, setPhase] = useState<Phase>("setup");
  const [count, setCount] = useState(3);
  const [elapsed, setElapsed] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [previewNote, setPreviewNote] = useState<string | null>(null);
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const phaseRef = useRef<Phase>("setup");
  phaseRef.current = phase;
  const queryClient = useQueryClient();

  // Device list (ffmpeg's avfoundation indexes — the source of truth).
  useEffect(() => {
    void recordDevices()
      .then((d) => {
        setDevices(d);
        const cam =
          d.cameras.find((c) => /macbook|facetime/i.test(c.name)) ?? d.cameras[0];
        const m =
          d.microphones.find((c) => /macbook/i.test(c.name)) ?? d.microphones[0];
        setCamera(cam?.index ?? null);
        setMic(m?.index ?? null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  // Live preview: match the selected ffmpeg device to a webview device by
  // LABEL (separate namespaces; names align in practice). The spike
  // instrumentation: if the track dies while recording, report it.
  const cameraName = devices?.cameras.find((c) => c.index === camera)?.name;
  useEffect(() => {
    let cancelled = false;
    setPreviewNote(null);
    async function open() {
      streamRef.current?.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
      if (!navigator.mediaDevices?.getUserMedia) return;
      try {
        // First ask generically (also primes labels for enumerateDevices).
        let stream = await navigator.mediaDevices.getUserMedia({ video: true });
        if (cameraName) {
          const all = await navigator.mediaDevices.enumerateDevices();
          const match = all.find(
            (d) => d.kind === "videoinput" && d.label === cameraName,
          );
          if (match && stream.getVideoTracks()[0]?.label !== cameraName) {
            stream.getTracks().forEach((t) => t.stop());
            stream = await navigator.mediaDevices.getUserMedia({
              video: { deviceId: { exact: match.deviceId } },
            });
          }
        }
        if (cancelled) {
          stream.getTracks().forEach((t) => t.stop());
          return;
        }
        streamRef.current = stream;
        const track = stream.getVideoTracks()[0];
        if (track) {
          track.onended = () => {
            if (phaseRef.current === "recording") {
              setPreviewNote(
                "preview paused — macOS handed the camera to the recorder (the recording is unaffected)",
              );
            }
          };
          track.onmute = track.onended;
        }
        if (videoRef.current) {
          videoRef.current.srcObject = stream;
          void videoRef.current.play().catch(() => {});
        }
      } catch {
        setPreviewNote("no live preview (camera permission or device busy) — recording still works");
      }
    }
    void open();
    return () => {
      cancelled = true;
      streamRef.current?.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    };
  }, [cameraName]);

  // Recording clock: local tick, honesty-checked against the backend.
  useEffect(() => {
    if (phase !== "recording") return;
    const started = Date.now();
    const tick = setInterval(() => setElapsed((Date.now() - started) / 1000), 500);
    const verify = setInterval(() => {
      void recordStatus().then((s) => {
        if (!s.recording && phaseRef.current === "recording") {
          setError("the capture process exited unexpectedly");
          setPhase("setup");
        }
      });
    }, 3000);
    return () => {
      clearInterval(tick);
      clearInterval(verify);
    };
  }, [phase]);

  const begin = useCallback(() => {
    if (camera == null || mic == null) return;
    setError(null);
    setPhase("countdown");
    setCount(3);
    let n = 3;
    const t = setInterval(() => {
      n -= 1;
      if (n > 0) {
        setCount(n);
        return;
      }
      clearInterval(t);
      // Start before the "0" frame: ffmpeg's camera warm-up (~2s) overlaps
      // the tail of the countdown instead of eating the take's first words.
      recordStart(camera, mic)
        .then(() => {
          setElapsed(0);
          setPhase("recording");
        })
        .catch((e) => {
          setError(String((e as { message?: string })?.message ?? e));
          setPhase("setup");
        });
    }, 800);
  }, [camera, mic]);

  const finish = useCallback(async () => {
    setPhase("finishing");
    try {
      const path = await recordStop();
      const stamp = new Date();
      const name = `recording ${stamp.getFullYear()}-${String(stamp.getMonth() + 1).padStart(2, "0")}-${String(stamp.getDate()).padStart(2, "0")} ${String(stamp.getHours()).padStart(2, "0")}.${String(stamp.getMinutes()).padStart(2, "0")}`;
      let createdId: string | null = null;
      const done = ingestFile(name, path, { onCreated: (id) => (createdId = id) });
      // Switch to the editor as soon as the project exists; transcription
      // streams in via events like any other import.
      const poll = setInterval(() => {
        if (createdId) {
          clearInterval(poll);
          void queryClient.invalidateQueries();
          setProjectId(createdId);
          setScreen("editor");
        }
      }, 100);
      done
        .catch((e) => setError(String((e as { message?: string })?.message ?? e)))
        .finally(() => {
          clearInterval(poll);
          void queryClient.invalidateQueries();
        });
    } catch (e) {
      setError(String((e as { message?: string })?.message ?? e));
      setPhase("setup");
    }
  }, [queryClient]);

  if (!isTauri) {
    return (
      <div className="recorder">
        <div className="recorder-head">
          <span className="recorder-title">record</span>
          <button className="ghost-btn" onClick={() => setScreen("editor")}>✕ close</button>
        </div>
        <p className="recorder-note">Recording needs the desktop app — the browser build has no camera capture.</p>
      </div>
    );
  }

  const recording = phase === "recording";
  return (
    <div className="recorder">
      <div className="recorder-head">
        <span className="recorder-title">
          {recording ? <span className="rec-dot" aria-hidden /> : null}
          {recording ? `recording ${fmtClock(elapsed)}` : "record"}
        </span>
        <button
          className="ghost-btn"
          onClick={() => setScreen("editor")}
          disabled={recording || phase === "finishing"}
        >
          ✕ close
        </button>
      </div>

      <div className="recorder-stage">
        <video ref={videoRef} muted playsInline className="recorder-preview" />
        {phase === "countdown" && <div className="recorder-countdown">{count}</div>}
        {previewNote && <div className="recorder-preview-note">{previewNote}</div>}
      </div>

      <div className="recorder-controls">
        {phase === "setup" && devices && (
          <>
            <label className="recorder-field">
              camera
              <select
                value={camera ?? ""}
                onChange={(e) => setCamera(Number(e.target.value))}
              >
                {devices.cameras.map((d) => (
                  <option key={d.index} value={d.index}>{d.name}</option>
                ))}
              </select>
            </label>
            <label className="recorder-field">
              mic
              <select value={mic ?? ""} onChange={(e) => setMic(Number(e.target.value))}>
                {devices.microphones.map((d) => (
                  <option key={d.index} value={d.index}>{d.name}</option>
                ))}
              </select>
            </label>
            <button
              className="primary-btn recorder-go"
              disabled={camera == null || mic == null}
              onClick={begin}
            >
              ● record
            </button>
          </>
        )}
        {phase === "countdown" && <span className="recorder-note">starting…</span>}
        {recording && (
          <button className="primary-btn recorder-stop" onClick={() => void finish()}>
            ■ stop
          </button>
        )}
        {phase === "finishing" && <span className="recorder-note">finalizing &amp; importing…</span>}
      </div>
      {error && <p className="empty-error">{error}</p>}
      <p className="recorder-hint">
        First recording asks for camera + microphone permission. Files land in
        ~/Movies/RoughCut and open as a project automatically.
      </p>
    </div>
  );
}
