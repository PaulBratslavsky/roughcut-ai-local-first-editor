// 9:16 framing over the preview: the window a vertical clip exports, with
// everything outside it dimmed. Drag the window sideways to reframe;
// "◎ face" asks the core to find the speaker. A drag commits once, on
// release (one update_clip, not one per pixel).

import { useEffect, useRef, useState } from "react";
import { useStore } from "@tanstack/react-store";
import { useCenterClipOnFace, useClips, useUpdateClip } from "../ipc/queries";
import { setCropClip, viewStore } from "../state/viewStore";
import { errText } from "./ClipsPanel";

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));

export function CropGuide({
  projectId,
  frameW,
  frameH,
}: {
  projectId: string;
  /** The preview frame's pixel size: what the 9:16 crop is cut from. */
  frameW: number;
  frameH: number;
}) {
  const cropClipId = useStore(viewStore, (s) => s.cropClipId);
  const { data: clips } = useClips(projectId);
  const clip = clips?.find((c) => c.id === cropClipId && c.format === "vertical");
  const update = useUpdateClip();
  const face = useCenterClipOnFace();
  const [dragX, setDragX] = useState<number | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const guideRef = useRef<HTMLDivElement | null>(null);
  const drag = useRef<{ startPx: number; startX: number } | null>(null);

  // The clip was deleted or switched to 16:9: stop framing it.
  useEffect(() => {
    if (cropClipId && clips && !clip) setCropClip(null);
  }, [cropClipId, clips, clip]);
  useEffect(() => setNote(null), [cropClipId]);

  if (!clip) return null;
  // The 9:16 window's share of the frame width (full width for a source
  // that's already 9:16 or narrower: nothing to pan).
  const win = Math.min(1, (frameH * 9) / 16 / Math.max(1, frameW));
  const x = dragX ?? clip.crop_x;

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.stopPropagation();
    if (win >= 1) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { startPx: e.clientX, startX: x };
    setDragX(x);
    setNote(null);
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    const width = guideRef.current?.getBoundingClientRect().width ?? 0;
    const travel = (1 - win) * width;
    if (!d || travel <= 0) return;
    setDragX(clamp01(d.startX + (e.clientX - d.startPx) / travel));
  };
  const onPointerUp = () => {
    if (!drag.current) return;
    drag.current = null;
    const next = Math.round((dragX ?? clip.crop_x) * 1000) / 1000;
    if (Math.abs(next - clip.crop_x) < 1e-3) {
      setDragX(null);
      return;
    }
    update.mutate(
      { project_id: projectId, clip_id: clip.id, crop_x: next },
      {
        onSettled: () => setDragX(null),
        onError: (e) => setNote(`couldn't save the crop: ${errText(e)}`),
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
    <div className="crop-guide" ref={guideRef} onClick={(e) => e.stopPropagation()}>
      <div
        className={`crop-window${dragX !== null ? " dragging" : ""}`}
        style={{ left: `${x * (1 - win) * 100}%`, width: `${win * 100}%` }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        title="Drag to reframe the 9:16 short"
      >
        <span className="crop-label">9:16</span>
      </div>
      <div className="crop-bar" onPointerDown={(e) => e.stopPropagation()}>
        <span className="crop-title">{clip.title}</span>
        <span className="crop-note">
          {face.isPending ? "finding the speaker…" : (note ?? "drag the frame to reframe")}
        </span>
        <button className="crop-btn" onClick={findFace} disabled={face.isPending} title="Centre the crop on the speaker's face">
          ◎ face
        </button>
        <button className="crop-btn" onClick={() => setCropClip(null)}>
          done
        </button>
      </div>
    </div>
  );
}
