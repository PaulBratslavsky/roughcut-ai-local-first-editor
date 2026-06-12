// Playback volume metering: PreviewPanel registers whichever element is
// AUDIBLE (the <video>, or the shifted <audio> follower when an A/V sync
// offset is active); the transport meter taps it through one WebAudio
// graph per element. createMediaElementSource is once-per-element-forever,
// hence the WeakMap of graphs.

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

/** Analyser for the current audible element (builds the graph on demand). */
export function audibleAnalyser(): AnalyserNode | null {
  if (!audible) return null;
  let graph = graphs.get(audible);
  if (!graph) {
    try {
      const ctx = new AudioContext();
      const source = ctx.createMediaElementSource(audible);
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 512;
      // Tap AND pass through — a bare analyser would mute playback.
      source.connect(analyser);
      analyser.connect(ctx.destination);
      graph = { ctx, analyser };
      graphs.set(audible, graph);
    } catch {
      return null; // element already claimed by another context
    }
  }
  if (graph.ctx.state === "suspended") void graph.ctx.resume().catch(() => {});
  return graph.analyser;
}
