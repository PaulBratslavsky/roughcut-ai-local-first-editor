// Playback volume metering, NON-INVASIVE edition. createMediaElementSource
// looked right but REROUTES the element's audio through the graph — and
// WebKit hands the graph silence for asset-protocol media (CORS taint),
// which silenced playback entirely. captureStream() taps a COPY of the
// media without touching the element's own output; where it's unsupported
// the meter simply stays idle and audio is never at risk.

interface Graph {
  ctx: AudioContext;
  analyser: AnalyserNode;
}

const graphs = new WeakMap<HTMLMediaElement, Graph>();
const listeners = new Set<() => void>();
let audible: HTMLMediaElement | null = null;

export function registerAudibleElement(el: HTMLMediaElement | null): void {
  audible = el;
  listeners.forEach((l) => l());
}

export function onAudibleChange(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

type WithCapture = HTMLMediaElement & { captureStream?: () => MediaStream };

/** Analyser for the current audible element, or null when tapping isn't
 *  possible — playback itself is NEVER rerouted. */
export function audibleAnalyser(): AnalyserNode | null {
  if (!audible) return null;
  let graph = graphs.get(audible);
  if (!graph) {
    try {
      const capture = (audible as WithCapture).captureStream;
      if (typeof capture !== "function") return null;
      const stream = capture.call(audible);
      if (stream.getAudioTracks().length === 0) return null;
      const ctx = new AudioContext();
      const source = ctx.createMediaStreamSource(stream);
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 512;
      // Stream sources are tap-only: nothing connects to the destination,
      // so the element keeps playing its own audio untouched.
      source.connect(analyser);
      graph = { ctx, analyser };
      graphs.set(audible, graph);
    } catch {
      return null;
    }
  }
  if (graph.ctx.state === "suspended") void graph.ctx.resume().catch(() => {});
  return graph.analyser;
}
