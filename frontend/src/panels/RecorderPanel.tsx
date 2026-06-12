// The recorder screen (M1: camera + mic). Live preview runs on the webview's
// getUserMedia while ffmpeg owns the actual capture — the M1 spike is
// whether macOS lets both read the camera at once; if the preview track
// dies when recording starts, we say so instead of looking frozen.

import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  attachScreenMedia,
  combineRecordings,
  openPrivacySettings,
  requestScreenPermission,
  screenPermissionStatus,
  deleteRecording,
  isTauri,
  listRecordings,
  recordDevices,
  recordPause,
  recordResume,
  recordStart,
  recordStatus,
  recordStop,
  type CaptureDevices,
  type RecordingFile,
} from "../ipc/api";
import { ingestFile } from "../ingest";
import { setProjectId, setScreen } from "../state/viewStore";

type Phase = "setup" | "countdown" | "recording" | "paused" | "finishing";

/** Tauri rejections are {error:{code,message}} — unwrap before display
 *  (String(obj) renders the dreaded "[object Object]"). */
function errText(e: unknown): string {
  const o = e as { error?: { message?: string }; message?: string };
  return o?.error?.message ?? o?.message ?? String(e);
}

function fmtClock(s: number): string {
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

/** Live mic level: proof the right microphone hears you BEFORE you commit
 *  to a take. WebAudio analyser on the selected device (matched by label). */
function MicMeter({ micName }: { micName?: string }) {
  const [level, setLevel] = useState(0);
  useEffect(() => {
    if (!navigator.mediaDevices?.getUserMedia) return;
    let stream: MediaStream | null = null;
    let ctx: AudioContext | null = null;
    let raf = 0;
    let cancelled = false;
    void (async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        if (micName) {
          const all = await navigator.mediaDevices.enumerateDevices();
          const match = all.find((d) => d.kind === "audioinput" && d.label === micName);
          if (match && stream.getAudioTracks()[0]?.label !== micName) {
            stream.getTracks().forEach((t) => t.stop());
            stream = await navigator.mediaDevices.getUserMedia({
              audio: { deviceId: { exact: match.deviceId } },
            });
          }
        }
        if (cancelled) return;
        ctx = new AudioContext();
        const src = ctx.createMediaStreamSource(stream);
        const analyser = ctx.createAnalyser();
        analyser.fftSize = 512;
        src.connect(analyser);
        const buf = new Uint8Array(analyser.fftSize);
        const tick = () => {
          analyser.getByteTimeDomainData(buf);
          let sum = 0;
          for (const v of buf) {
            const d = (v - 128) / 128;
            sum += d * d;
          }
          setLevel(Math.min(1, Math.sqrt(sum / buf.length) * 4));
          raf = requestAnimationFrame(tick);
        };
        tick();
      } catch {
        /* no meter without permission — recording still works */
      }
    })();
    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
      stream?.getTracks().forEach((t) => t.stop());
      void ctx?.close().catch(() => {});
    };
  }, [micName]);
  return (
    <span className="mic-meter" title="mic level">
      <span className="mic-meter-fill" style={{ width: `${Math.round(level * 100)}%` }} />
    </span>
  );
}

function fmtDur(s: number): string {
  if (s <= 0) return "";
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

/** The recordings library (~/Movies/RoughCut): import one as a project, or
 *  select several takes and stitch them into one — Camtasia-bin style, but
 *  the result stays a single source the transcript editor can edit. */
function RecordingsLibrary({ onIngest }: { onIngest: (name: string, path: string) => void }) {
  const [files, setFiles] = useState<RecordingFile[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => void listRecordings().then(setFiles).catch(() => setFiles([]));
  useEffect(refresh, []);

  const remove = async (f: RecordingFile) => {
    const sure = await ask(`Delete "${f.name}" permanently? This is the video file itself.`, {
      title: "Delete recording",
      kind: "warning",
    });
    if (!sure) return;
    try {
      await deleteRecording(f.path);
      setPicked((old) => {
        const next = new Set(old);
        next.delete(f.path);
        return next;
      });
      refresh();
    } catch (e) {
      setError(errText(e));
    }
  };

  if (!files || files.length === 0) return null;

  const toggle = (path: string) => {
    setPicked((old) => {
      const next = new Set(old);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const combine = async () => {
    setBusy(true);
    setError(null);
    try {
      // Stitch in recording order (oldest first), not click order.
      const ordered = [...files].reverse().filter((f) => picked.has(f.path)).map((f) => f.path);
      const path = await combineRecordings(ordered);
      onIngest("combined recording", path);
    } catch (e) {
      setError(errText(e));
      setBusy(false);
    }
  };

  return (
    <aside className="rec-library">
      <div className="rec-library-head">
        <span className="rec-library-title">previous recordings</span>
        {picked.size >= 2 && (
          <button className="primary-btn" disabled={busy} onClick={() => void combine()}>
            {busy ? "stitching…" : `stitch ${picked.size} & import`}
          </button>
        )}
      </div>
      <div className="rec-library-list">
        {files.map((f) => (
          <div key={f.path} className="rec-library-row">
            <label className="rec-library-pick">
              <input
                type="checkbox"
                checked={picked.has(f.path)}
                onChange={() => toggle(f.path)}
              />
              <span className="rec-library-name">{f.name}</span>
            </label>
            <span className="rec-library-meta">
              {fmtDur(f.duration_s)} · {f.size_mb.toFixed(0)} MB
            </span>
            <button className="ghost-btn" disabled={busy} onClick={() => onIngest(f.name, f.path)}>
              import
            </button>
            <button
              className="icon-btn rec-delete"
              title="Delete this recording file"
              disabled={busy}
              onClick={() => void remove(f)}
            >
              ✕
            </button>
          </div>
        ))}
      </div>
      {error && <p className="empty-error">{error}</p>}
    </aside>
  );
}

export function RecorderPanel() {
  const [devices, setDevices] = useState<CaptureDevices | null>(null);
  const [source, setSource] = useState<"camera" | "screen" | "both">("camera");
  const [camera, setCamera] = useState<number | null>(null);
  const [screen, setScreenDev] = useState<number | null>(null);
  const [mic, setMic] = useState<number | null>(null);
  const [phase, setPhase] = useState<Phase>("setup");
  const [count, setCount] = useState(3);
  const [elapsed, setElapsed] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [take, setTake] = useState(1);
  const [doneTakesS, setDoneTakesS] = useState(0);
  const [previewNote, setPreviewNote] = useState<string | null>(null);
  const [screenPerm, setScreenPerm] = useState<boolean | null>(null);
  const [permHint, setPermHint] = useState(false);
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
        setScreenDev(d.screens[0]?.index ?? null);
        setMic(m?.index ?? null);
      })
      .catch((e) => setError(errText(e)));
  }, []);

  // Live preview: match the selected ffmpeg device to a webview device by
  // LABEL (separate namespaces; names align in practice). The spike
  // instrumentation: if the track dies while recording, report it.
  const cameraName =
    source !== "screen" ? devices?.cameras.find((c) => c.index === camera)?.name : undefined;
  useEffect(() => {
    let cancelled = false;
    setPreviewNote(null);
    async function open() {
      streamRef.current?.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
      if (!navigator.mediaDevices?.getUserMedia) return;
      try {
        // First ask generically (also primes labels for enumerateDevices).
        const hd = { width: { ideal: 1920 }, height: { ideal: 1080 } };
        let stream = await navigator.mediaDevices.getUserMedia({ video: hd });
        if (cameraName) {
          const all = await navigator.mediaDevices.enumerateDevices();
          const match = all.find(
            (d) => d.kind === "videoinput" && d.label === cameraName,
          );
          if (match && stream.getVideoTracks()[0]?.label !== cameraName) {
            stream.getTracks().forEach((t) => t.stop());
            stream = await navigator.mediaDevices.getUserMedia({
              video: { deviceId: { exact: match.deviceId }, ...hd },
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

  // Screen Recording TCC: checked whenever a screen source is selected, so
  // the grant flow happens BEFORE a take fails into a timeout.
  useEffect(() => {
    if (source === "camera") return;
    void screenPermissionStatus().then(setScreenPerm).catch(() => setScreenPerm(null));
  }, [source]);

  const grantScreen = async () => {
    // macOS shows the system prompt exactly once, ever. After that the only
    // path is System Settings — and the grant applies on app relaunch.
    const granted = await requestScreenPermission().catch(() => false);
    setScreenPerm(granted);
    if (!granted) {
      setPermHint(true);
      void openPrivacySettings("screen").catch(() => {});
    }
  };

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
    const video = source === "screen" ? screen : camera;
    if (video == null || mic == null) return;
    if (source === "both" && screen == null) return;
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
      recordStart(video, mic, source === "screen", source === "both" ? (screen ?? undefined) : undefined)
        .then(() => {
          setElapsed(0);
          setTake(1);
          setDoneTakesS(0);
          setPhase("recording");
        })
        .catch((e) => {
          setError(errText(e));
          setPhase("setup");
        });
    }, 800);
  }, [camera, screen, mic, source]);

  const doPause = useCallback(async () => {
    try {
      const st = await recordPause();
      setDoneTakesS(st.total_s);
      setTake(st.take);
      setPhase("paused");
    } catch (e) {
      setError(errText(e));
    }
  }, []);

  const doResume = useCallback(async () => {
    try {
      const st = await recordResume();
      setTake(st.take);
      setElapsed(0);
      setPhase("recording");
    } catch (e) {
      setError(errText(e));
    }
  }, []);

  const ingestRecording = useCallback(
    (name: string, path: string, screenPath?: string) => {
      setPhase("finishing");
      let createdId: string | null = null;
      const done = ingestFile(name, path, {
        onCreated: (id) => {
          createdId = id;
          if (screenPath) {
            void attachScreenMedia(id, screenPath).catch(() => {});
          }
        },
      });
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
        .catch((e) => {
          setError(errText(e));
          setPhase("setup");
        })
        .finally(() => {
          clearInterval(poll);
          void queryClient.invalidateQueries();
        });
    },
    [queryClient],
  );

  const finish = useCallback(async () => {
    setPhase("finishing");
    try {
      const out = await recordStop();
      const stamp = new Date();
      const kind = out.screen_path ? "presentation" : "recording";
      const name = `${kind} ${stamp.getFullYear()}-${String(stamp.getMonth() + 1).padStart(2, "0")}-${String(stamp.getDate()).padStart(2, "0")} ${String(stamp.getHours()).padStart(2, "0")}.${String(stamp.getMinutes()).padStart(2, "0")}`;
      ingestRecording(name, out.path, out.screen_path ?? undefined);
    } catch (e) {
      setError(errText(e));
      setPhase("setup");
    }
  }, [ingestRecording]);

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
          {recording
            ? `recording · take ${take} · ${fmtClock(doneTakesS + elapsed)}`
            : phase === "paused"
              ? `paused · ${take} take${take === 1 ? "" : "s"} · ${fmtClock(doneTakesS)}`
              : "record"}
        </span>
        <button
          className="ghost-btn"
          onClick={() => setScreen("editor")}
          disabled={recording || phase === "paused" || phase === "finishing"}
        >
          ✕ close
        </button>
      </div>

      <div className="recorder-body">
      <div className="recorder-main">
      <div className="recorder-stage">
        {source === "screen" && phase !== "recording" && phase !== "paused" ? (
          <div className="recorder-screen-ph">
            <span>display {screen ?? "?"} will be captured (cursor included)</span>
            <span className="recorder-screen-sub">
              no live preview for screens — the first screen take asks for the
              Screen Recording permission (System Settings ▸ Privacy &amp; Security)
            </span>
          </div>
        ) : (
          <video ref={videoRef} muted playsInline className="recorder-preview" />
        )}
        {phase === "countdown" && <div className="recorder-countdown">{count}</div>}
        {previewNote && <div className="recorder-preview-note">{previewNote}</div>}
      </div>

      {source !== "camera" && screenPerm === false && (
        <div className="perm-banner">
          <span>
            macOS hasn't granted RoughCut <strong>Screen Recording</strong> —
            screen takes would capture nothing.
          </span>
          <button className="primary-btn" onClick={() => void grantScreen()}>
            grant permission…
          </button>
          {permHint && (
            <span className="perm-hint">
              In the Settings window that just opened: enable <strong>RoughCut</strong>
              {" "}under Screen &amp; System Audio Recording, then <strong>relaunch the app</strong>.
            </span>
          )}
        </div>
      )}

      <div className="recorder-controls">
        {phase === "setup" && devices && (
          <>
            <div className="recorder-source">
              <button
                className={`tab${source === "camera" ? " active" : ""}`}
                onClick={() => setSource("camera")}
              >
                camera
              </button>
              <button
                className={`tab${source === "screen" ? " active" : ""}`}
                onClick={() => setSource("screen")}
                disabled={devices.screens.length === 0}
              >
                screen
              </button>
              <button
                className={`tab${source === "both" ? " active" : ""}`}
                onClick={() => setSource("both")}
                disabled={devices.screens.length === 0 || devices.cameras.length === 0}
                title="presentation mode: camera AND screen, two synced files"
              >
                screen + camera
              </button>
            </div>
            {source !== "screen" && (
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
            )}
            {source !== "camera" && (
              <label className="recorder-field">
                display
                <select
                  value={screen ?? ""}
                  onChange={(e) => setScreenDev(Number(e.target.value))}
                >
                  {devices.screens.map((d) => (
                    <option key={d.index} value={d.index}>{d.name}</option>
                  ))}
                </select>
              </label>
            )}
            <label className="recorder-field">
              mic
              <select value={mic ?? ""} onChange={(e) => setMic(Number(e.target.value))}>
                {devices.microphones.map((d) => (
                  <option key={d.index} value={d.index}>{d.name}</option>
                ))}
              </select>
              <MicMeter micName={devices.microphones.find((d) => d.index === mic)?.name} />
            </label>
            <button
              className="primary-btn recorder-go"
              disabled={(source === "screen" ? screen : camera) == null || mic == null || (source === "both" && screen == null)}
              onClick={begin}
            >
              ● record
            </button>
          </>
        )}
        {phase === "countdown" && <span className="recorder-note">starting…</span>}
        {recording && (
          <>
            <button className="ghost-btn" onClick={() => void doPause()}>⏸ pause</button>
            <button className="primary-btn recorder-stop" onClick={() => void finish()}>
              ■ finish
            </button>
          </>
        )}
        {phase === "paused" && (
          <>
            <span className="recorder-note">break time — takes stitch together when you finish</span>
            <button className="primary-btn recorder-go" onClick={() => void doResume()}>
              ● resume
            </button>
            <button className="ghost-btn recorder-stop-ghost" onClick={() => void finish()}>
              ■ finish
            </button>
          </>
        )}
        {phase === "finishing" && <span className="recorder-note">finalizing &amp; importing…</span>}
      </div>
      {error && <p className="empty-error">{error}</p>}
      <p className="recorder-hint">
        First recording asks for camera + microphone permission. Files land in
        ~/Movies/RoughCut and open as a project automatically.
      </p>
      </div>
      {phase === "setup" && <RecordingsLibrary onIngest={ingestRecording} />}
      </div>
    </div>
  );
}
