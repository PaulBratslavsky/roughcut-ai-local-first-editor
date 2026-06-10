// Pure Canvas2D renderer + layout math for the timeline. No React in here.

import type { TimelineAssets } from "./assets";
import type { Clip } from "../ipc/types";

export const RULER_H = 26;
export const TRACK_TOP = RULER_H + 8;
export const EDGE_HIT_PX = 6;

// ---------------------------------------------------------------------------
// Time mapping: source seconds <-> display seconds.
// With showCuts on the mapping is identity (excluded clips stay visible);
// with showCuts off, excluded ranges collapse away.
// ---------------------------------------------------------------------------

export interface TimeMap {
  toDisplay(t: number): number;
  toSource(d: number): number;
  displayDuration: number;
}

interface MapSeg { srcIn: number; srcOut: number; offset: number }

export function buildTimeMap(clips: Clip[], showCuts: boolean, sourceDuration: number): TimeMap {
  if (showCuts) {
    return {
      toDisplay: (t) => t,
      toSource: (d) => d,
      displayDuration: sourceDuration,
    };
  }
  const included = clips
    .filter((c) => c.included)
    .slice()
    .sort((a, b) => a.source_in - b.source_in);

  const segs: MapSeg[] = [];
  let offset = 0;
  for (const c of included) {
    segs.push({ srcIn: c.source_in, srcOut: c.source_out, offset });
    offset += c.source_out - c.source_in;
  }
  const displayDuration = offset;

  return {
    toDisplay(t: number): number {
      for (const s of segs) {
        if (t < s.srcIn) return s.offset; // inside a collapsed gap -> snap forward
        if (t <= s.srcOut) return s.offset + (t - s.srcIn);
      }
      return displayDuration;
    },
    toSource(d: number): number {
      for (const s of segs) {
        const len = s.srcOut - s.srcIn;
        if (d <= s.offset + len) return s.srcIn + Math.max(0, d - s.offset);
      }
      return segs.length > 0 ? segs[segs.length - 1]!.srcOut : 0;
    },
    displayDuration,
  };
}

// ---------------------------------------------------------------------------
// Clip layout in screen space
// ---------------------------------------------------------------------------

export interface ClipRect {
  clip: Clip;
  x: number;
  w: number;
}

export function layoutClips(
  clips: Clip[],
  map: TimeMap,
  zoom: number,
  scrollX: number,
  showCuts: boolean,
): ClipRect[] {
  const rects: ClipRect[] = [];
  for (const clip of clips) {
    if (!showCuts && !clip.included) continue;
    const dIn = map.toDisplay(clip.source_in);
    const dOut = map.toDisplay(clip.source_out);
    const x = (dIn - scrollX) * zoom;
    const w = Math.max(1, (dOut - dIn) * zoom - 2);
    rects.push({ clip, x, w });
  }
  return rects;
}

export type EdgeHit = { clip: Clip; edge: "in" | "out" } | null;

export function hitTestClipEdge(rects: ClipRect[], x: number): EdgeHit {
  for (const r of rects) {
    if (Math.abs(x - r.x) <= EDGE_HIT_PX) return { clip: r.clip, edge: "in" };
    if (Math.abs(x - (r.x + r.w)) <= EDGE_HIT_PX) return { clip: r.clip, edge: "out" };
  }
  return null;
}

// ---------------------------------------------------------------------------
// Waveform peaks. Deterministic pseudo-random per clip id for now; this hook
// point is where real peaks from the Rust VideoEngine adapter will plug in
// later (binary over the asset/stream protocol, never JSON IPC).
// ---------------------------------------------------------------------------

const peakCache = new Map<string, number[]>();

export function waveformPeaks(clipId: string, count: number): number[] {
  const key = `${clipId}:${count}`;
  const cached = peakCache.get(key);
  if (cached) return cached;

  let seed = 2166136261;
  for (let i = 0; i < clipId.length; i++) {
    seed ^= clipId.charCodeAt(i);
    seed = Math.imul(seed, 16777619);
  }
  let a = seed >>> 0;
  const rand = () => {
    a |= 0; a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };

  const peaks: number[] = [];
  let env = 0.35 + rand() * 0.3;
  for (let i = 0; i < count; i++) {
    env += (rand() - 0.5) * 0.22;
    env = Math.min(0.95, Math.max(0.08, env));
    const spike = rand() < 0.06 ? rand() * 0.35 : 0;
    peaks.push(Math.min(1, env + spike));
  }
  peakCache.set(key, peaks);
  return peaks;
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

export interface DragPreview {
  clipId: string;
  edge: "in" | "out";
  time: number; // source seconds
}

export interface DrawState {
  width: number; // CSS px
  height: number;
  clips: Clip[];
  duration: number;
  showCuts: boolean;
  zoom: number;
  scrollX: number;
  playhead: number; // source seconds
  selectedClipId: string | null;
  hoverEdge: EdgeHit;
  drag: DragPreview | null;
  /** Real peaks/thumbnails when loaded; null = synthetic waveform fallback. */
  assets: TimelineAssets | null;
}

const DARK_COLORS = {
  bg: "#141416",
  ruler: "#1b1b1f",
  rulerText: "#8a8a92",
  tick: "#3a3a42",
  clip: "#2e3d52",
  clipTop: "#3a4d68",
  clipBorder: "#4a6285",
  clipExcluded: "#26262a",
  clipExcludedBorder: "#3a3a40",
  hatch: "#46464e",
  wave: "#9eb86e",
  waveDim: "#55555c",
  playhead: "#5b9bff",
  selected: "#7ab3ff",
  handle: "#cfe0f5",
};

const LIGHT_COLORS: typeof DARK_COLORS = {
  bg: "#e8e7e3",
  ruler: "#efeeea",
  rulerText: "#6f6f78",
  tick: "#c4c3bd",
  clip: "#c8d4e6",
  clipTop: "#dbe4f1",
  clipBorder: "#8aa6cc",
  clipExcluded: "#dddcd7",
  clipExcludedBorder: "#c2c1bb",
  hatch: "#c0bfb9",
  wave: "#6f8f44",
  waveDim: "#b3b2ac",
  playhead: "#2f6fe0",
  selected: "#2f6fe0",
  handle: "#3a4d68",
};

/** Active palette follows <html data-theme>. */
function theme() {
  return document.documentElement.dataset.theme === "light" ? LIGHT_COLORS : DARK_COLORS;
}

function pickTickStep(zoom: number): number {
  const steps = [0.25, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300];
  for (const s of steps) {
    if (s * zoom >= 70) return s;
  }
  return 600;
}

function fmtTime(t: number): string {
  const m = Math.floor(t / 60);
  const s = t - m * 60;
  const sStr = s < 10 ? `0${Number.isInteger(s) ? s : s.toFixed(1)}` : `${Number.isInteger(s) ? s : s.toFixed(1)}`;
  return `${String(m).padStart(2, "0")}:${sStr}`;
}

function roundRectPath(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number): void {
  const rr = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + rr, y);
  ctx.arcTo(x + w, y, x + w, y + h, rr);
  ctx.arcTo(x + w, y + h, x, y + h, rr);
  ctx.arcTo(x, y + h, x, y, rr);
  ctx.arcTo(x, y, x + w, y, rr);
  ctx.closePath();
}

export function drawTimeline(ctx: CanvasRenderingContext2D, s: DrawState): void {
  const { width, height } = s;
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = theme().bg;
  ctx.fillRect(0, 0, width, height);

  // Apply trim-drag preview by patching the dragged clip.
  let clips = s.clips;
  if (s.drag) {
    clips = clips.map((c) => {
      if (c.id !== s.drag!.clipId) return c;
      return s.drag!.edge === "in"
        ? { ...c, source_in: Math.min(s.drag!.time, c.source_out - 0.05) }
        : { ...c, source_out: Math.max(s.drag!.time, c.source_in + 0.05) };
    });
  }

  const map = buildTimeMap(clips, s.showCuts, s.duration);
  const rects = layoutClips(clips, map, s.zoom, s.scrollX, s.showCuts);

  // --- ruler -----------------------------------------------------------------
  ctx.fillStyle = theme().ruler;
  ctx.fillRect(0, 0, width, RULER_H);
  const step = pickTickStep(s.zoom);
  const firstTick = Math.floor(s.scrollX / step) * step;
  const lastTick = s.scrollX + width / s.zoom;
  ctx.font = "10px ui-sans-serif, system-ui, sans-serif";
  ctx.textBaseline = "middle";
  for (let t = firstTick; t <= lastTick; t += step) {
    const x = (t - s.scrollX) * s.zoom;
    ctx.strokeStyle = theme().tick;
    ctx.beginPath();
    ctx.moveTo(x + 0.5, RULER_H - 7);
    ctx.lineTo(x + 0.5, RULER_H);
    ctx.stroke();
    ctx.fillStyle = theme().rulerText;
    ctx.fillText(fmtTime(Math.max(0, t)), x + 4, RULER_H / 2);
    // minor ticks
    const minor = step / 5;
    for (let mt = t + minor; mt < t + step; mt += minor) {
      const mx = (mt - s.scrollX) * s.zoom;
      ctx.beginPath();
      ctx.moveTo(mx + 0.5, RULER_H - 3);
      ctx.lineTo(mx + 0.5, RULER_H);
      ctx.stroke();
    }
  }

  // --- clips -------------------------------------------------------------------
  const trackH = height - TRACK_TOP - 10;
  for (const r of rects) {
    if (r.x + r.w < 0 || r.x > width) continue;
    const excluded = !r.clip.included;
    const isSelected = s.selectedClipId === r.clip.id;

    roundRectPath(ctx, r.x, TRACK_TOP, r.w, trackH, 6);
    if (excluded) {
      ctx.fillStyle = theme().clipExcluded;
      ctx.fill();
      // hatch
      ctx.save();
      ctx.clip();
      ctx.strokeStyle = theme().hatch;
      ctx.lineWidth = 1;
      const spacing = 8;
      for (let hx = r.x - trackH; hx < r.x + r.w; hx += spacing) {
        ctx.beginPath();
        ctx.moveTo(hx, TRACK_TOP + trackH);
        ctx.lineTo(hx + trackH, TRACK_TOP);
        ctx.stroke();
      }
      ctx.restore();
      ctx.strokeStyle = isSelected ? theme().selected : theme().clipExcludedBorder;
      ctx.lineWidth = isSelected ? 2 : 1;
      roundRectPath(ctx, r.x, TRACK_TOP, r.w, trackH, 6);
      ctx.stroke();
    } else {
      const grad = ctx.createLinearGradient(0, TRACK_TOP, 0, TRACK_TOP + trackH);
      grad.addColorStop(0, theme().clipTop);
      grad.addColorStop(1, theme().clip);
      ctx.fillStyle = grad;
      ctx.fill();
      ctx.strokeStyle = isSelected ? theme().selected : theme().clipBorder;
      ctx.lineWidth = isSelected ? 2 : 1;
      roundRectPath(ctx, r.x, TRACK_TOP, r.w, trackH, 6);
      ctx.stroke();
    }

    // clip body: thumbnail filmstrip (top) + waveform (bottom)
    const hasThumbs = !!s.assets && s.assets.thumbs.length > 0;
    const hasPeaks = !!s.assets?.peaks && s.assets.peaks.length > 0;
    const clipDur = r.clip.source_out - r.clip.source_in;

    ctx.save();
    roundRectPath(ctx, r.x, TRACK_TOP, r.w, trackH, 6);
    ctx.clip();

    if (hasThumbs) {
      const thumbs = s.assets!.thumbs;
      const stripH = trackH * 0.56;
      const aspect = thumbs[0]!.img.width / Math.max(1, thumbs[0]!.img.height);
      const thumbW = stripH * aspect;
      // Tile frames across the clip; each tile shows the frame nearest its
      // own SOURCE time, so cuts stay visually truthful.
      const firstTile = Math.floor((r.x < 0 ? -r.x : 0) / thumbW);
      for (let k = firstTile; k * thumbW < r.w; k++) {
        const tileX = r.x + k * thumbW;
        if (tileX > width) break;
        const srcT = r.clip.source_in + ((k + 0.5) * thumbW) / Math.max(1e-6, r.w) * clipDur;
        let lo = 0, hi = thumbs.length - 1;
        while (lo < hi) {
          const mid = (lo + hi) >> 1;
          if (thumbs[mid]!.time < srcT) lo = mid + 1; else hi = mid;
        }
        const best = lo > 0 && srcT - thumbs[lo - 1]!.time < thumbs[lo]!.time - srcT ? lo - 1 : lo;
        ctx.globalAlpha = excluded ? 0.3 : 1;
        ctx.drawImage(thumbs[best]!.img, tileX, TRACK_TOP, thumbW, stripH);
        ctx.globalAlpha = 1;
      }
    }

    // waveform: real peaks mapped by source time, synthetic fallback otherwise
    const barW = 2;
    const gap = 1;
    const n = Math.max(4, Math.floor(r.w / (barW + gap)));
    const waveH = hasThumbs ? trackH * 0.36 : trackH * 0.62;
    const baseY = TRACK_TOP + trackH - 4;
    ctx.fillStyle = excluded ? theme().waveDim : theme().wave;
    const fallback = hasPeaks ? null : waveformPeaks(r.clip.id, 96);
    for (let i = 0; i < n; i++) {
      let p: number;
      if (hasPeaks) {
        const srcT = r.clip.source_in + ((i + 0.5) / n) * clipDur;
        const idx = Math.min(
          s.assets!.peaks!.length - 1,
          Math.max(0, Math.floor(srcT * s.assets!.pps)),
        );
        p = (s.assets!.peaks![idx] ?? 0) / 255;
      } else {
        p = fallback![Math.floor((i / n) * fallback!.length)] ?? 0.3;
      }
      const bh = Math.max(2, p * waveH);
      ctx.fillRect(r.x + 2 + i * (barW + gap), baseY - bh, barW, bh);
    }
    ctx.restore();

    // edge handles (hover / drag / selection)
    const showHandles =
      isSelected ||
      (s.hoverEdge && s.hoverEdge.clip.id === r.clip.id) ||
      (s.drag && s.drag.clipId === r.clip.id);
    if (showHandles && r.w > 14) {
      ctx.fillStyle = theme().handle;
      roundRectPath(ctx, r.x + 2, TRACK_TOP + trackH / 2 - 9, 3, 18, 2);
      ctx.fill();
      roundRectPath(ctx, r.x + r.w - 5, TRACK_TOP + trackH / 2 - 9, 3, 18, 2);
      ctx.fill();
    }
  }

  // --- playhead -----------------------------------------------------------------
  const px = (map.toDisplay(s.playhead) - s.scrollX) * s.zoom;
  if (px >= -2 && px <= width + 2) {
    ctx.strokeStyle = theme().playhead;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(px, 4);
    ctx.lineTo(px, height - 2);
    ctx.stroke();
    ctx.fillStyle = theme().playhead;
    ctx.beginPath();
    ctx.moveTo(px - 5, 2);
    ctx.lineTo(px + 5, 2);
    ctx.lineTo(px, 11);
    ctx.closePath();
    ctx.fill();
  }
}
