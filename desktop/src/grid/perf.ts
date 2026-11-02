// Numbers for the grid's perf HUD (Ctrl/Cmd+Shift+P): time to first rows,
// block fetch latency, scroll frame rate, JS heap, rows cached. Collecting is
// cheap (a few numbers per block and per frame while scrolling), so it always
// runs; the HUD only reads it.

export interface PerfSnapshot {
  /** Milliseconds from opening a table (or starting a run) to its first rows on screen. */
  firstRowsMs: number | null;
  fetches: number;
  p50: number | null;
  p95: number | null;
  /** Frames per second over the last second of scrolling. */
  fps: number | null;
  /** The longest frame in that second, ms. */
  worstFrameMs: number | null;
  heapMB: number | null;
  rowsCached: number;
  blocksCached: number;
}

const latencies: number[] = [];
const LATENCY_SAMPLES = 200;
let fetches = 0;
let firstRowsMs: number | null = null;
let started: number | null = null;
let rowsCached = 0;
let blocksCached = 0;

/** Frame times (performance.now) of frames drawn while scrolling, last second. */
const frames: number[] = [];
let lastScroll = -Infinity;
let sampling = false;
let previousFrame = 0;

const listeners = new Set<() => void>();
let visible = false;

/** Something new starts loading (a table opens, a query runs). */
export function markStart() {
  started = performance.now();
}

/** The first rows of what started are on screen (call once they are painted). */
export function markFirstRows() {
  if (started === null) return;
  const begin = started;
  started = null;
  requestAnimationFrame(() => {
    firstRowsMs = performance.now() - begin;
    notify();
  });
}

export function recordFetch(ms: number) {
  fetches++;
  latencies.push(ms);
  if (latencies.length > LATENCY_SAMPLES) latencies.shift();
}

export function recordCache(rows: number, blocks: number) {
  rowsCached = rows;
  blocksCached = blocks;
}

/** The grid scrolled: sample frames for a moment. */
export function recordScroll() {
  lastScroll = performance.now();
  if (!sampling) {
    sampling = true;
    previousFrame = 0;
    requestAnimationFrame(frame);
  }
}

function frame(now: number) {
  if (previousFrame) frames.push(now);
  previousFrame = now;
  while (frames.length && frames[0] < now - 1000) frames.shift();
  if (now - lastScroll < 250) requestAnimationFrame(frame);
  else sampling = false;
}

function percentile(sorted: number[], p: number): number | null {
  if (sorted.length === 0) return null;
  return sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))];
}

export function snapshot(): PerfSnapshot {
  const sorted = [...latencies].sort((a, b) => a - b);
  let fps: number | null = null;
  let worst: number | null = null;
  if (frames.length >= 2) {
    const span = frames[frames.length - 1] - frames[0];
    fps = span > 0 ? ((frames.length - 1) * 1000) / span : null;
    worst = 0;
    for (let i = 1; i < frames.length; i++) worst = Math.max(worst, frames[i] - frames[i - 1]);
  }
  const memory = (performance as Performance & { memory?: { usedJSHeapSize: number } }).memory;
  return {
    firstRowsMs,
    fetches,
    p50: percentile(sorted, 50),
    p95: percentile(sorted, 95),
    fps,
    worstFrameMs: worst,
    heapMB: memory ? memory.usedJSHeapSize / 1048576 : null,
    rowsCached,
    blocksCached,
  };
}

/** Clears the fetch and frame samples (the HUD's Reset). */
export function resetPerf() {
  latencies.length = 0;
  frames.length = 0;
  fetches = 0;
  notify();
}

export function hudVisible(): boolean {
  return visible;
}

export function toggleHud() {
  visible = !visible;
  notify();
}

export function subscribeHud(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function notify() {
  for (const l of listeners) l();
}
