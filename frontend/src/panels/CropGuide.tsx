// 9:16 framing over the preview: the window a vertical short exports, with
// everything outside it dimmed. Drag the window to move it; drag a corner to
// zoom in or out (it stays 9:16, never bigger than the full frame height).
// A change commits once, on release (one tool call, not one per pixel).
//
// Two things get framed: a saved 9:16 clip (Clips ▸ crop; "◎ face" asks the
// core to find the speaker) and, on the Shorts screen, a scan candidate
// before it's made (the made short keeps that framing instead of being
// framed on the speaker).

import { useEffect, useRef, useState, type ReactNode } from "react";
import { useStore } from "@tanstack/react-store";
import {
  useCenterClipOnFace,
  useClipCandidates,
  useClips,
  useFrameCandidate,
  useUpdateClip,
} from "../ipc/queries";
import type { ClipFraming } from "../ipc/types";
import { setCropCandidate, setCropClip, viewStore } from "../state/viewStore";
import { errText } from "./clipUtil";

/** The closest a crop zooms in (the core's ClipFraming::MAX_ZOOM). */
const MAX_ZOOM = 3;
const CENTRED: ClipFraming = { crop_x: 0.5, crop_y: 0.5, crop_zoom: 1 };
/** Under this many source rows the 1920-row short looks soft. */
const SOFT_ROWS = 720;

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const round3 = (v: number) => Math.round(v * 1000) / 1000;
const same = (a: ClipFraming, b: ClipFraming) =>
  Math.abs(a.crop_x - b.crop_x) < 1e-3 && Math.abs(a.crop_y - b.crop_y) < 1e-3 && Math.abs(a.crop_zoom - b.crop_zoom) < 1e-3;

type Corner = "nw" | "ne" | "sw" | "se";
type Drag =
  | { kind: "move"; px: number; py: number; from: ClipFraming }
  | { kind: "zoom"; corner: Corner; ax: number; ay: number };

/** The window's size as shares of the frame (width, height). */
function windowShare(frameW: number, frameH: number, zoom: number): [number, number] {
  const full = Math.min(1, (frameH * 9) / 16 / Math.max(1, frameW));
  return [full / zoom, 1 / zoom];
}

/** The draggable, zoomable 9:16 window. `bar` is the strip across the top. */
function CropFrame({
  frameW,
  frameH,
  framing,
  onCommit,
  bar,
}: {
  /** The preview frame's pixel size: what the 9:16 crop is cut from. */
  frameW: number;
  frameH: number;
  framing: ClipFraming;
  /** Save a new framing; call `revert` if that fails (the window goes back). */
  onCommit: (f: ClipFraming, revert: () => void) => void;
  bar: ReactNode;
}) {
  const [draft, setDraft] = useState<ClipFraming | null>(null);
  const guideRef = useRef<HTMLDivElement | null>(null);
  const drag = useRef<Drag | null>(null);
  // A new framing from the core (another clip, a face pass) replaces a
  // finished drag's draft.
  useEffect(() => setDraft(null), [framing.crop_x, framing.crop_y, framing.crop_zoom]);

  const f = draft ?? framing;
  const [ww, wh] = windowShare(frameW, frameH, f.crop_zoom);
  const box = () => guideRef.current?.getBoundingClientRect();

  const begin = (e: React.PointerEvent<HTMLElement>, d: Drag) => {
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = d;
    setDraft(f);
  };

  const onMoveDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if ((e.target as HTMLElement).dataset.corner) return;
    begin(e, { kind: "move", px: e.clientX, py: e.clientY, from: f });
  };

  const onCornerDown = (corner: Corner) => (e: React.PointerEvent<HTMLSpanElement>) => {
    const r = box();
    if (!r) return;
    // The opposite corner stays put, in pixels from the frame's top-left.
    const left = f.crop_x * (1 - ww) * r.width;
    const top = f.crop_y * (1 - wh) * r.height;
    const right = left + ww * r.width;
    const bottom = top + wh * r.height;
    const ax = corner.includes("w") ? right : left;
    const ay = corner.includes("n") ? bottom : top;
    begin(e, { kind: "zoom", corner, ax, ay });
  };

  const onPointerMove = (e: React.PointerEvent<HTMLElement>) => {
    const d = drag.current;
    const r = box();
    if (!d || !r) return;
    if (d.kind === "move") {
      const [w0, h0] = windowShare(frameW, frameH, d.from.crop_zoom);
      const travelX = (1 - w0) * r.width;
      const travelY = (1 - h0) * r.height;
      setDraft({
        ...d.from,
        crop_x: travelX > 0 ? clamp(d.from.crop_x + (e.clientX - d.px) / travelX, 0, 1) : d.from.crop_x,
        crop_y: travelY > 0 ? clamp(d.from.crop_y + (e.clientY - d.py) / travelY, 0, 1) : d.from.crop_y,
      });
      return;
    }
    // Zoom: the window grows from the fixed corner toward the pointer,
    // keeping 9:16, inside the frame, between full height and a third of it.
    const [full] = windowShare(frameW, frameH, 1);
    const ratio = (full * r.width) / r.height; // window width / height, in px
    const px = e.clientX - r.left;
    const py = e.clientY - r.top;
    const right = d.corner.includes("e");
    const down = d.corner.includes("s");
    const roomX = right ? r.width - d.ax : d.ax;
    const roomY = down ? r.height - d.ay : d.ay;
    let h = Math.max(Math.abs(py - d.ay), Math.abs(px - d.ax) / ratio);
    h = clamp(h, r.height / MAX_ZOOM, Math.min(r.height, roomY, roomX / ratio));
    const w = h * ratio;
    const left = right ? d.ax : d.ax - w;
    const top = down ? d.ay : d.ay - h;
    setDraft({
      crop_zoom: clamp(r.height / h, 1, MAX_ZOOM),
      crop_x: r.width - w > 0.5 ? clamp(left / (r.width - w), 0, 1) : 0.5,
      crop_y: r.height - h > 0.5 ? clamp(top / (r.height - h), 0, 1) : 0.5,
    });
  };

  const onPointerUp = () => {
    if (!drag.current) return;
    drag.current = null;
    const next = draft ?? framing;
    const rounded = { crop_x: round3(next.crop_x), crop_y: round3(next.crop_y), crop_zoom: round3(next.crop_zoom) };
    if (same(rounded, framing)) {
      setDraft(null);
      return;
    }
    onCommit(rounded, () => setDraft(null));
  };

  const corners: Corner[] = ["nw", "ne", "sw", "se"];
  return (
    <div className="crop-guide" ref={guideRef} onClick={(e) => e.stopPropagation()}>
      <div
        className={`crop-window${draft !== null ? " dragging" : ""}`}
        style={{
          left: `${f.crop_x * (1 - ww) * 100}%`,
          top: `${f.crop_y * (1 - wh) * 100}%`,
          width: `${ww * 100}%`,
          height: `${wh * 100}%`,
        }}
        onPointerDown={onMoveDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        title="Drag to move the 9:16 frame · drag a corner to zoom"
      >
        <span className="crop-label">9:16{f.crop_zoom > 1.005 && ` · ${f.crop_zoom.toFixed(1)}×`}</span>
        {corners.map((c) => (
          <span
            key={c}
            data-corner={c}
            className={`crop-corner ${c}`}
            onPointerDown={onCornerDown(c)}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={onPointerUp}
            aria-hidden
          />
        ))}
      </div>
      <div className="crop-bar" onPointerDown={(e) => e.stopPropagation()}>
        {bar}
      </div>
    </div>
  );
}

/** "zoom 1.8× · soft": how close it is, and when the short will look soft. */
function zoomNote(frameH: number, f: ClipFraming): string | null {
  if (f.crop_zoom <= 1.005) return null;
  const soft = frameH / f.crop_zoom < SOFT_ROWS;
  return `zoom ${f.crop_zoom.toFixed(1)}×${soft ? " · may look soft (few source pixels)" : ""}`;
}

/** A saved 9:16 clip's crop (Clips ▸ crop). */
export function CropGuide({ projectId, frameW, frameH }: { projectId: string; frameW: number; frameH: number }) {
  const cropClipId = useStore(viewStore, (s) => s.cropClipId);
  const { data: clips } = useClips(projectId);
  const clip = clips?.find((c) => c.id === cropClipId && c.format === "vertical");
  const update = useUpdateClip();
  const face = useCenterClipOnFace();
  const [note, setNote] = useState<string | null>(null);

  // The clip was deleted or switched to 16:9: stop framing it.
  useEffect(() => {
    if (cropClipId && clips && !clip) setCropClip(null);
  }, [cropClipId, clips, clip]);
  useEffect(() => setNote(null), [cropClipId]);

  if (!clip) return null;
  const framing: ClipFraming = { crop_x: clip.crop_x, crop_y: clip.crop_y, crop_zoom: clip.crop_zoom };

  const commit = (f: ClipFraming, revert?: () => void) => {
    setNote(null);
    update.mutate(
      { project_id: projectId, clip_id: clip.id, ...f },
      {
        onError: (e) => {
          revert?.();
          setNote(`couldn't save the crop: ${errText(e)}`);
        },
      },
    );
  };
  const findFace = () => {
    setNote(null);
    face.mutate(
      { project_id: projectId, clip_id: clip.id },
      {
        onSuccess: (r) =>
          setNote(
            r.crop_x === null
              ? `no clear face in ${r.frames_sampled} frames · drag to frame it`
              : `framed on the speaker · face in ${r.frames_with_face}/${r.frames_sampled} frames`,
          ),
        onError: (e) => setNote(errText(e)),
      },
    );
  };

  return (
    <CropFrame
      frameW={frameW}
      frameH={frameH}
      framing={framing}
      onCommit={commit}
      bar={
        <>
          <span className="crop-title">{clip.title}</span>
          <span className="crop-note">
            {face.isPending
              ? "finding the speaker…"
              : (note ?? zoomNote(frameH, framing) ?? "drag to move · drag a corner to zoom")}
          </span>
          {framing.crop_zoom > 1.005 && (
            <button className="crop-btn" onClick={() => commit({ ...framing, crop_zoom: 1, crop_y: 0.5 })} title="Back to the full frame height">
              full height
            </button>
          )}
          <button className="crop-btn" onClick={findFace} disabled={face.isPending} title="Centre the crop on the speaker's face">
            ◎ face
          </button>
          <button className="crop-btn" onClick={() => setCropClip(null)}>
            done
          </button>
        </>
      }
    />
  );
}

/** A scan candidate's crop on the Shorts screen, before it's made. Without a
 *  framing it shows centred; making it then frames it on the speaker. */
export function CandidateCropGuide({
  projectId,
  frameW,
  frameH,
}: {
  projectId: string;
  frameW: number;
  frameH: number;
}) {
  const candidateId = useStore(viewStore, (s) => s.cropCandidateId);
  const framingClip = useStore(viewStore, (s) => s.cropClipId !== null);
  const { data: candidates } = useClipCandidates(projectId);
  const candidate = candidates?.find((c) => c.id === candidateId);
  const frame = useFrameCandidate();
  const [error, setError] = useState<string | null>(null);

  // Made into a short, or replaced by a new scan: stop framing it.
  useEffect(() => {
    if (candidateId && candidates && !candidate) setCropCandidate(null);
  }, [candidateId, candidates, candidate]);
  useEffect(() => setError(null), [candidateId]);

  if (!candidate || framingClip) return null;
  const framing = candidate.framing ?? CENTRED;
  const set = (args: Partial<ClipFraming> & { reset?: boolean }, revert?: () => void) => {
    setError(null);
    frame.mutate(
      { project_id: projectId, candidate_id: candidate.id, ...args },
      {
        onError: (e) => {
          revert?.();
          setError(`couldn't save the framing: ${errText(e)}`);
        },
      },
    );
  };

  const note =
    error ??
    (candidate.framing
      ? [zoomNote(frameH, framing), "the short keeps this framing"].filter(Boolean).join(" · ")
      : "drag to frame it yourself · drag a corner to zoom · untouched, it's framed on the speaker when made");

  return (
    <CropFrame
      frameW={frameW}
      frameH={frameH}
      framing={framing}
      onCommit={(f, revert) => set(f, revert)}
      bar={
        <>
          <span className="crop-title">{candidate.title}</span>
          <span className="crop-note" title={note}>
            {note}
          </span>
          {candidate.framing && (
            <button
              className="crop-btn"
              onClick={() => set({ reset: true })}
              disabled={frame.isPending}
              title="Forget this framing: frame the short on the speaker when it's made"
            >
              ◎ auto
            </button>
          )}
          <button className="crop-btn" onClick={() => setCropCandidate(null)}>
            done
          </button>
        </>
      }
    />
  );
}
