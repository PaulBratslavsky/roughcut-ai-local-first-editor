// Canvas timeline: ruler, clip blocks, fake waveform, playhead.
// Interactions: click/drag to seek, edge-drag to trim (-> trim_clip),
// wheel to zoom around the playhead / horizontal scroll.

import { useEffect, useMemo, useRef, useState } from "react";
import { useCutRange, useMediaAssets, useRestoreRange, useSplitClip, useTimeline, useTrimClip } from "../ipc/queries";
import type { Clip } from "../ipc/types";
import { loadTimelineAssets, type TimelineAssets } from "./assets";
import {
  fitTimelineToWidth,
  seekTo,
  setScrollX,
  setSelectedClipId,
  viewStore,
  zoomAroundPlayhead,
} from "../state/viewStore";
import {
  buildTimeMap,
  drawTimeline,
  hitTestClipEdge,
  layoutClips,
  type DragPreview,
  type EdgeHit,
} from "./renderer";

interface DragState {
  kind: "trim" | "scrub";
  preview: DragPreview | null;
}

export function Timeline({ projectId }: { projectId: string }) {
  const { data: timeline } = useTimeline(projectId);
  const { data: assetMeta, isLoading: assetsGenerating } = useMediaAssets(projectId);
  const trimClip = useTrimClip();
  const cutRange = useCutRange();
  const restoreRange = useRestoreRange();
  const splitClip = useSplitClip();

  // Right-click menu on a clip: Restore for cut clips, Cut/Split for kept.
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; clip: Clip } | null>(null);
  useEffect(() => {
    if (!ctxMenu) return;
    const close = () => setCtxMenu(null);
    const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && close();
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", close, true);
    };
  }, [ctxMenu]);

  const onContextMenu = (e: React.MouseEvent<HTMLCanvasElement>) => {
    e.preventDefault();
    const x = localX(e);
    const { rects } = currentLayout();
    const hit = rects.find((r) => x >= r.x && x <= r.x + r.w);
    if (!hit) return;
    setSelectedClipId(hit.clip.id);
    setCtxMenu({
      x: Math.min(e.clientX, window.innerWidth - 220),
      y: Math.min(e.clientY, window.innerHeight - 140),
      clip: hit.clip,
    });
  };

  // Auto-fit: when a project's duration first becomes known, zoom so the
  // whole video spans the lane — a 1:40 short fills the width instead of
  // huddling at default zoom. Once per project; manual zoom wins afterwards.
  const fittedRef = useRef<string | null>(null);
  useEffect(() => {
    const dur = timeline?.duration ?? 0;
    const w = wrapRef.current?.clientWidth ?? 0;
    if (!dur || !w || fittedRef.current === projectId) return;
    fittedRef.current = projectId;
    fitTimelineToWidth(dur, w);
  }, [projectId, timeline?.duration]);

  // Real waveform peaks + thumbnails: fetched over the asset protocol once
  // the core reports them; the renderer falls back to synthetic bars until then.
  const [assets, setAssets] = useState<TimelineAssets | null>(null);
  const assetsRef = useRef<TimelineAssets | null>(null);
  assetsRef.current = assets;
  useEffect(() => {
    let cancelled = false;
    setAssets(null);
    if (!assetMeta) return;
    void loadTimelineAssets(assetMeta).then((loaded) => {
      if (!cancelled) setAssets(loaded);
    });
    return () => { cancelled = true; };
  }, [assetMeta]);

  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const dragRef = useRef<DragState | null>(null);
  const hoverEdgeRef = useRef<EdgeHit>(null);
  const sizeRef = useRef({ w: 0, h: 0 });
  const rafRef = useRef(0);

  const clips = useMemo(() => timeline?.clips ?? [], [timeline]);
  const duration = timeline?.duration ?? 0;
  const clipsRef = useRef(clips);
  clipsRef.current = clips;
  const durationRef = useRef(duration);
  durationRef.current = duration;

  // --- draw loop: redraw via rAF whenever view state or data changes -------
  useEffect(() => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let dirty = true;
    const markDirty = () => { dirty = true; };

    const resize = () => {
      const rect = wrap.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      sizeRef.current = { w: rect.width, h: rect.height };
      canvas.width = Math.max(1, Math.round(rect.width * dpr));
      canvas.height = Math.max(1, Math.round(rect.height * dpr));
      canvas.style.width = `${rect.width}px`;
      canvas.style.height = `${rect.height}px`;
      markDirty();
    };
    const ro = new ResizeObserver(resize);
    ro.observe(wrap);
    resize();

    const unsub = viewStore.subscribe(markDirty);
    window.addEventListener("rc-theme-changed", markDirty);

    const loop = () => {
      rafRef.current = requestAnimationFrame(loop);
      if (!dirty) return;
      dirty = false;
      const dpr = window.devicePixelRatio || 1;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      const s = viewStore.state;
      drawTimeline(ctx, {
        width: sizeRef.current.w,
        height: sizeRef.current.h,
        clips: clipsRef.current,
        duration: durationRef.current,
        showCuts: s.showCuts,
        zoom: s.zoom,
        scrollX: s.scrollX,
        playhead: s.playhead,
        selectedClipId: s.selectedClipId,
        hoverEdge: hoverEdgeRef.current,
        drag: dragRef.current?.kind === "trim" ? dragRef.current.preview : null,
        assets: assetsRef.current,
      });
    };
    rafRef.current = requestAnimationFrame(loop);

    return () => {
      cancelAnimationFrame(rafRef.current);
      ro.disconnect();
      window.removeEventListener("rc-theme-changed", markDirty);
      unsub();
    };
  }, []);

  // Redraw when timeline data or loaded view assets change.
  useEffect(() => {
    // nudge the store so the rAF loop sees a change
    viewStore.setState((s) => ({ ...s }));
  }, [timeline, assets]);

  // --- helpers ---------------------------------------------------------------
  const currentLayout = () => {
    const s = viewStore.state;
    const map = buildTimeMap(clipsRef.current, s.showCuts, durationRef.current);
    const rects = layoutClips(clipsRef.current, map, s.zoom, s.scrollX, s.showCuts);
    return { s, map, rects };
  };

  const localX = (e: { clientX: number }) => {
    const rect = canvasRef.current!.getBoundingClientRect();
    return e.clientX - rect.left;
  };

  // --- pointer interactions ----------------------------------------------------
  const onPointerDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (!canvas || duration === 0) return;
    canvas.setPointerCapture(e.pointerId);
    const x = localX(e);
    const { s, map, rects } = currentLayout();
    const edge = hitTestClipEdge(rects, x);
    if (edge) {
      setSelectedClipId(edge.clip.id);
      dragRef.current = {
        kind: "trim",
        preview: {
          clipId: edge.clip.id,
          edge: edge.edge,
          time: edge.edge === "in" ? edge.clip.source_in : edge.clip.source_out,
        },
      };
    } else {
      // Plain click: select the clip under the pointer (NLE behavior),
      // clear selection on empty track, and scrub.
      const hit = rects.find((r) => x >= r.x && x <= r.x + r.w);
      setSelectedClipId(hit ? hit.clip.id : null);
      dragRef.current = { kind: "scrub", preview: null };
      seekTo(map.toSource(x / s.zoom + s.scrollX));
    }
  };

  const onPointerMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (!canvas || duration === 0) return;
    const x = localX(e);
    const drag = dragRef.current;
    const { s, map, rects } = currentLayout();

    if (drag?.kind === "trim" && drag.preview) {
      const t = map.toSource(x / s.zoom + s.scrollX);
      drag.preview = { ...drag.preview, time: Math.min(duration, Math.max(0, t)) };
      viewStore.setState((v) => ({ ...v })); // trigger redraw
      return;
    }
    if (drag?.kind === "scrub") {
      seekTo(map.toSource(x / s.zoom + s.scrollX));
      return;
    }
    const edge = hitTestClipEdge(rects, x);
    if ((edge !== null) !== (hoverEdgeRef.current !== null)) {
      canvas.style.cursor = edge ? "ew-resize" : "default";
    }
    hoverEdgeRef.current = edge;
    viewStore.setState((v) => ({ ...v }));
  };

  const onPointerUp = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (canvas) canvas.releasePointerCapture(e.pointerId);
    const drag = dragRef.current;
    dragRef.current = null;
    if (drag?.kind === "trim" && drag.preview) {
      const clip = clipsRef.current.find((c) => c.id === drag.preview!.clipId);
      if (clip) {
        const newIn = drag.preview.edge === "in" ? Math.min(drag.preview.time, clip.source_out - 0.05) : clip.source_in;
        const newOut = drag.preview.edge === "out" ? Math.max(drag.preview.time, clip.source_in + 0.05) : clip.source_out;
        if (Math.abs(newIn - clip.source_in) > 1e-3 || Math.abs(newOut - clip.source_out) > 1e-3) {
          trimClip.mutate({
            project_id: projectId,
            clip_id: clip.id,
            new_source_in: Number(newIn.toFixed(3)),
            new_source_out: Number(newOut.toFixed(3)),
          });
        }
      }
    }
    viewStore.setState((v) => ({ ...v }));
  };

  // --- wheel: zoom around playhead / horizontal scroll (non-passive) -----------
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const s = viewStore.state;
      if (Math.abs(e.deltaX) > Math.abs(e.deltaY) || e.shiftKey) {
        const d = (e.shiftKey ? e.deltaY : e.deltaX) / s.zoom;
        setScrollX(s.scrollX + d);
      } else {
        const map = buildTimeMap(clipsRef.current, s.showCuts, durationRef.current);
        zoomAroundPlayhead(Math.exp(-e.deltaY * 0.0018), map.toDisplay(s.playhead));
      }
    };
    canvas.addEventListener("wheel", onWheel, { passive: false });
    return () => canvas.removeEventListener("wheel", onWheel);
  }, []);

  return (
    <div className="timeline-wrap" ref={wrapRef}>
      <canvas
        ref={canvasRef}
        className="timeline-canvas"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onContextMenu={onContextMenu}
      />
      {(assetsGenerating || (assetMeta && !assets)) && (
        <div className="timeline-generating" role="status">
          <span className="spinner spinner-sm" aria-hidden />
          generating waveform &amp; thumbnails… the timeline works meanwhile
        </div>
      )}
      {ctxMenu && (
        <div
          className="context-menu"
          style={{ left: ctxMenu.x, top: ctxMenu.y, position: "fixed" }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div className="context-menu-label">
            {ctxMenu.clip.included ? "Clip" : "Cut"} · {(ctxMenu.clip.source_out - ctxMenu.clip.source_in).toFixed(1)}s
          </div>
          {ctxMenu.clip.included ? (
            <>
              <button
                onClick={() => {
                  cutRange.mutate({ project_id: projectId, start: ctxMenu.clip.source_in, end: ctxMenu.clip.source_out });
                  setCtxMenu(null);
                }}
              >
                Delete this clip
              </button>
              {viewStore.state.playhead > ctxMenu.clip.source_in &&
                viewStore.state.playhead < ctxMenu.clip.source_out && (
                  <button
                    onClick={() => {
                      splitClip.mutate({ project_id: projectId, clip_id: ctxMenu.clip.id, at_time: viewStore.state.playhead });
                      setCtxMenu(null);
                    }}
                  >
                    Split at playhead
                  </button>
                )}
            </>
          ) : (
            <button
              onClick={() => {
                restoreRange.mutate({ project_id: projectId, start: ctxMenu.clip.source_in, end: ctxMenu.clip.source_out });
                setCtxMenu(null);
              }}
            >
              Restore this cut
            </button>
          )}
        </div>
      )}
    </div>
  );
}

// Used by the transport's zoom buttons (anchors on the playhead).
export function zoomTimeline(factor: number): void {
  const s = viewStore.state;
  zoomAroundPlayhead(factor, s.playhead);
}
