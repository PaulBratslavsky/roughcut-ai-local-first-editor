// Load timeline view assets (real waveform peaks + thumbnail filmstrip).
// The core hands us FILE PATHS; the binary flows over the asset protocol
// (fetch on a convertFileSrc URL), never JSON IPC.

import { mediaSrc } from "../ipc/api";
import type { MediaAssets } from "../ipc/types";

export interface LoadedThumb {
  time: number;
  img: HTMLImageElement;
}

export interface TimelineAssets {
  /** u8 peaks, `pps` per second of source time; null = use synthetic fallback. */
  peaks: Uint8Array | null;
  pps: number;
  thumbs: LoadedThumb[];
}

function loadImage(url: string): Promise<HTMLImageElement | null> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => resolve(null);
    img.src = url;
  });
}

export async function loadTimelineAssets(meta: MediaAssets): Promise<TimelineAssets> {
  let peaks: Uint8Array | null = null;
  const peaksUrl = meta.peaks_path ? mediaSrc(meta.peaks_path) : null;
  if (peaksUrl) {
    try {
      const resp = await fetch(peaksUrl);
      if (resp.ok) peaks = new Uint8Array(await resp.arrayBuffer());
    } catch {
      peaks = null;
    }
  }

  const thumbs: LoadedThumb[] = [];
  const loaded = await Promise.all(
    meta.thumbnails.map(async (t) => {
      const url = mediaSrc(t.path);
      if (!url) return null;
      const img = await loadImage(url);
      return img ? { time: t.time, img } : null;
    }),
  );
  for (const t of loaded) if (t) thumbs.push(t);
  thumbs.sort((a, b) => a.time - b.time);

  return { peaks, pps: meta.peaks_per_second, thumbs };
}
