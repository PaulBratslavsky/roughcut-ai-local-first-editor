// Cut-time math, pure. Source time is the raw footage clock; EDITED time is
// what the exported cut will show (source minus everything excluded). All of
// the playback engine's skip/fade decisions and the UI's "Cut · 35:06"
// readouts reduce to these functions.

import type { Clip } from "../ipc/types";

export interface Range {
  start: number;
  end: number;
}

/** The excluded portions of the timeline, sorted, in source time. */
export function excludedRanges(clips: Clip[]): Range[] {
  return clips
    .filter((c) => !c.included)
    .map((c) => ({ start: c.source_in, end: c.source_out }))
    .sort((a, b) => a.start - b.start);
}

/** If `t` is inside an excluded range, the time to jump to instead. */
export function skipTarget(t: number, ranges: Range[]): number | null {
  for (const r of ranges) {
    if (t >= r.start && t < r.end - 1e-4) return r.end;
    if (r.start > t) break;
  }
  return null;
}

/** Total excluded duration in seconds. */
export function excludedTotal(ranges: Range[]): number {
  return ranges.reduce((acc, r) => acc + (r.end - r.start), 0);
}

/** Source time → edited time (what the viewer of the cut has watched). */
export function toEditedTime(t: number, ranges: Range[]): number {
  let cut = 0;
  for (const r of ranges) {
    if (t >= r.end) cut += r.end - r.start;
    else if (t > r.start) cut += t - r.start;
    else break;
  }
  return Math.max(0, t - cut);
}
