<script lang="ts">
  import { onMount, untrack } from "svelte";
  import uPlot from "uplot";
  import "uplot/dist/uPlot.min.css";
  import {
    batchSeries,
    batchStability,
    listBatchEvents,
    onSyncStatus,
    toApiError,
    type ApiError,
    type BatchEvent,
    type Config,
    type Curve,
    type SeriesRange,
    type StabilityView,
  } from "$lib/api";
  import { buildPlot, toAbs, toX, type Drawn, type Plot, type XMode } from "$lib/chart";
  import { formatTimestamp } from "$lib/units";

  // `revision` is bumped by every write from the batch screen — a manual reading or an
  // event raises no `sync://status` — and refetches at the same zoom.
  let {
    batchId,
    pitchedAt,
    ui,
    revision,
  }: { batchId: number; pitchedAt: number | null; ui: Config["ui"]; revision: number } = $props();

  /** Per sensor series. A zoom refetches at the same count: the bucket shrinks, the payload doesn't grow. */
  const MAX_POINTS = 1000;
  const HEIGHT = 360;

  let mode = $state<XMode>("abs");
  // `raw`: these are replaced whole, never mutated, and a deep proxy over tens of
  // thousands of numbers is cost for nothing.
  let curve = $state.raw<Curve | null>(null);
  let events = $state.raw<BatchEvent[]>([]);
  let stability = $state.raw<StabilityView | null>(null);
  let error = $state<ApiError | null>(null);
  let hover = $state<{ event: BatchEvent; left: number } | null>(null);

  let host: HTMLDivElement;
  let chart: uPlot | null = null;
  let shape = "";
  // The zoomed range in epoch seconds; `null` is the default, pitch to now. Not state:
  // a change is a fetch, and the chart reads it when that lands.
  let view: SeriesRange | null = null;

  const nothing = $derived(curve !== null && curve.range === null);
  const plot = $derived(curve ? buildPlot(curve, mode, pitchedAt, ui.temp_display) : null);

  // One fetch at a time. Anything asked for meanwhile runs once after, with whatever
  // `view` is by then — a burst of sync events is one refetch, not a queue of them.
  let busy = false;
  let again: boolean | null = null;

  async function refresh(all: boolean) {
    if (busy) {
      again = (again ?? false) || all;
      return;
    }
    busy = true;
    try {
      const [c, e, s] = await Promise.allSettled([
        batchSeries(batchId, view, MAX_POINTS),
        all ? listBatchEvents(batchId) : null,
        all ? batchStability(batchId) : null,
      ]);
      if (c.status === "fulfilled") {
        curve = c.value;
        error = null;
      } else {
        error = toApiError(c.reason);
      }
      if (e.status === "fulfilled" && e.value) events = e.value;
      if (s.status === "fulfilled" && s.value) stability = s.value;
    } finally {
      busy = false;
      if (again !== null) {
        const next = again;
        again = null;
        void refresh(next);
      }
    }
  }

  /** The range on screen: the zoom, else what the server resolved the default to. */
  function shown(): SeriesRange | null {
    return view ?? curve?.range ?? null;
  }

  function applyRange(u: uPlot, m: XMode) {
    const r = shown();
    if (r) u.setScale("x", { min: toX(r.start, m, pitchedAt), max: toX(r.end, m, pitchedAt) });
  }

  function seriesOpts(d: Drawn): uPlot.Series {
    const base = { label: `${d.label} ${d.metric}`, scale: d.axis };
    const faint = `${d.color}66`;
    switch (d.role) {
      case "line":
      case "smoothed":
        return { ...base, stroke: d.color, width: 1.5, points: { show: false } };
      case "raw":
        return {
          ...base,
          label: `${base.label} raw`,
          stroke: faint,
          paths: () => null,
          points: { show: true, size: 3, stroke: faint, fill: faint },
        };
      case "manual":
        return {
          ...base,
          stroke: d.color,
          width: 1,
          dash: [2, 4],
          points: { show: true, size: 7, fill: d.color },
        };
      case "hi":
      case "lo":
        return { ...base, stroke: "transparent", width: 0, points: { show: false } };
    }
  }

  const AXIS = { stroke: "#9aa0a6", grid: { stroke: "#2a2e36" }, ticks: { stroke: "#333842" } };

  function axesOpts(p: Plot, m: XMode, unit: string): uPlot.Axis[] {
    const axes: uPlot.Axis[] = [
      m === "abs"
        ? AXIS
        : { ...AXIS, label: "hours since pitch", values: (_u, splits) => splits.map((v) => `${v} h`) },
    ];
    if (p.axes.includes("grav")) {
      const digits = unit === "SG" ? 3 : 1;
      axes.push({
        ...AXIS,
        scale: "grav",
        label: unit,
        values: (_u, splits) => splits.map((v) => v.toFixed(digits)),
        size: 60,
      });
    }
    if (p.axes.includes("temp")) {
      axes.push({ ...AXIS, scale: "temp", side: 1, label: `°${ui.temp_display}`, grid: { show: false } });
    }
    if (p.axes.includes("ph")) {
      axes.push({ ...AXIS, scale: "ph", side: 1, label: "pH", grid: { show: false } });
    }
    return axes;
  }

  const kindLabel = (kind: string) => kind.replaceAll("_", " ");

  /** Stable and stalled spans, under the grid and the series. */
  function shadeSpans(u: uPlot, m: XMode) {
    const { ctx, bbox } = u;
    ctx.save();
    for (const span of stability?.spans ?? []) {
      const x0 = Math.max(u.valToPos(toX(span.start, m, pitchedAt), "x", true), bbox.left);
      const x1 = Math.min(u.valToPos(toX(span.end, m, pitchedAt), "x", true), bbox.left + bbox.width);
      if (x1 <= x0) continue;
      ctx.fillStyle = span.state === "stable" ? "#7fb0691f" : "#e0a35f1f";
      ctx.fillRect(x0, bbox.top, x1 - x0, bbox.height);
    }
    ctx.restore();
  }

  function drawEvents(u: uPlot, m: XMode) {
    const { ctx, bbox } = u;
    const px = uPlot.pxRatio;
    ctx.save();
    ctx.font = `${11 * px}px system-ui, sans-serif`;
    ctx.lineWidth = px;
    for (const e of events) {
      const x = Math.round(u.valToPos(toX(e.ts, m, pitchedAt), "x", true));
      if (x < bbox.left || x > bbox.left + bbox.width) continue;
      const pitch = e.kind === "pitch";
      ctx.strokeStyle = pitch ? "#e6e6e6" : "#9aa0a6aa";
      ctx.setLineDash(pitch ? [] : [4 * px, 4 * px]);
      ctx.beginPath();
      ctx.moveTo(x, bbox.top);
      ctx.lineTo(x, bbox.top + bbox.height);
      ctx.stroke();
      ctx.fillStyle = pitch ? "#e6e6e6" : "#9aa0a6";
      ctx.fillText(pitch ? "pitch · t=0" : kindLabel(e.kind), x + 4 * px, bbox.top + 12 * px);
    }
    ctx.restore();
  }

  /** The nearest event within a few pixels of the cursor, for its note. */
  function hoverEvent(u: uPlot, m: XMode) {
    const left = u.cursor.left;
    let best: { event: BatchEvent; left: number } | null = null;
    if (left !== undefined && left >= 0) {
      let nearest = 6;
      for (const e of events) {
        const x = u.valToPos(toX(e.ts, m, pitchedAt), "x");
        if (Math.abs(x - left) < nearest) {
          nearest = Math.abs(x - left);
          best = { event: e, left: u.over.offsetLeft + x };
        }
      }
    }
    if (best?.event.id !== hover?.event.id) hover = best;
  }

  /** A drag zooms at once on what is loaded, then refetches that range at full detail. */
  function zoom(u: uPlot, m: XMode) {
    const { left, width } = u.select;
    if (width < 2) return;
    const start = Math.floor(toAbs(u.posToVal(left, "x"), m, pitchedAt));
    const end = Math.ceil(toAbs(u.posToVal(left + width, "x"), m, pitchedAt));
    u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
    if (end <= start) return;
    view = { start, end };
    applyRange(u, m);
    void refresh(false);
  }

  function resetZoom() {
    if (view === null) return;
    view = null;
    void refresh(false);
  }

  function build(p: Plot, m: XMode, unit: string): uPlot {
    const u = new uPlot(
      {
        width: host.clientWidth,
        height: HEIGHT,
        scales: { x: { time: m === "abs" } },
        series: [{}, ...p.drawn.map(seriesOpts)],
        bands: p.bands.map(([hi, lo]) => ({
          series: [hi, lo] as [number, number],
          fill: `${p.drawn[hi - 1].color}26`,
        })),
        axes: axesOpts(p, m, unit),
        cursor: {
          drag: { x: true, y: false, setScale: false },
          // Ours instead: uPlot's would reset to the loaded data, which after a zoom
          // is only the zoomed range.
          bind: { dblclick: () => () => null },
        },
        hooks: {
          drawClear: [(u) => shadeSpans(u, m)],
          draw: [(u) => drawEvents(u, m)],
          setSelect: [(u) => zoom(u, m)],
          setCursor: [(u) => hoverEvent(u, m)],
        },
      },
      p.data as uPlot.AlignedData,
      host,
    );
    u.over.addEventListener("dblclick", resetZoom);
    // Band edges carry no information of their own in the legend.
    const rows = u.root.querySelectorAll<HTMLElement>(".u-legend .u-series");
    p.drawn.forEach((d, i) => {
      if (d.role === "hi" || d.role === "lo") rows[i + 1]?.style.setProperty("display", "none");
    });
    return u;
  }

  $effect(() => {
    const p = plot;
    const m = mode;
    const unit = curve?.gravity_unit ?? "SG";
    // Read so a new event list or new spans redraw.
    void events;
    void stability;
    untrack(() => {
      if (!p || p.drawn.length === 0) {
        chart?.destroy();
        chart = null;
        shape = "";
        return;
      }
      const key = [m, ui.temp_display, unit, p.axes.join(), p.drawn.map((d) => `${d.streamId}:${d.role}`).join()].join("|");
      if (chart && key === shape) {
        chart.setData(p.data as uPlot.AlignedData);
      } else {
        chart?.destroy();
        chart = build(p, m, unit);
        shape = key;
      }
      applyRange(chart, m);
    });
  });

  // Gravity is converted server-side, so a unit change is a refetch.
  $effect(() => {
    void ui.gravity_display;
    void revision;
    untrack(() => void refresh(true));
  });

  onMount(() => {
    // No timer: sync already is one. Refetched only when a poll added rows for a source
    // this curve draws — or when it draws nothing yet, since a first reading has no
    // series to match.
    const unlisten = onSyncStatus((s) => {
      if (s.rows_added === 0) return;
      const series = curve?.series ?? [];
      if (series.length === 0 || series.some((x) => x.source_id === s.source_id)) {
        void refresh(true);
      }
    });
    const resize = new ResizeObserver(() => chart?.setSize({ width: host.clientWidth, height: HEIGHT }));
    resize.observe(host);
    return () => {
      unlisten();
      resize.disconnect();
      chart?.destroy();
      chart = null;
    };
  });
</script>

{#if !nothing}
  <div class="toolbar">
    <div class="toggle" role="group" aria-label="Time axis">
      <button class:on={mode === "abs"} onclick={() => (mode = "abs")}>Time</button>
      <button
        class:on={mode === "rel"}
        onclick={() => (mode = "rel")}
        disabled={pitchedAt === null}
        title={pitchedAt === null ? "Not pitched: no t=0 to count from" : undefined}
      >
        Hours since pitch
      </button>
    </div>
    <span class="hint">Drag to zoom, double-click to reset.</span>
    {#if stability?.spans.length}
      <span class="key"><i class="stable"></i>stable <i class="stalled"></i>stalled</span>
    {/if}
  </div>
{/if}

<div class="wrap">
  <div bind:this={host}></div>
  {#if hover}
    <div class="tip" style:left="{hover.left}px">
      <strong>{kindLabel(hover.event.kind)}</strong>
      <span>{formatTimestamp(hover.event.ts)}</span>
      {#if hover.event.note}<p>{hover.event.note}</p>{/if}
    </div>
  {/if}
</div>

{#if error}
  <p role="alert">{error.message}</p>
{:else if nothing}
  <p class="empty">
    {pitchedAt === null ? "Not pitched yet: the curve starts at the pitch." : "No streams bound — nothing to chart yet."}
  </p>
{:else if plot && plot.drawn.length === 0}
  <p class="empty">No readings in this range.</p>
{/if}

<style>
  .toolbar {
    display: flex;
    align-items: center;
    gap: 1rem;
    margin-bottom: 0.5rem;
  }

  .toggle button {
    background: #1e2127;
    color: #9aa0a6;
    border: 1px solid #333842;
    padding: 0.25rem 0.6rem;
  }

  .toggle button.on {
    background: #2d5d8a;
    color: #e6e6e6;
  }

  .toggle button:disabled {
    opacity: 0.5;
  }

  .hint,
  .key,
  .empty {
    color: #6f757d;
  }

  .key i {
    display: inline-block;
    width: 0.8rem;
    height: 0.8rem;
    margin: 0 0.3rem 0 0.5rem;
    vertical-align: -0.1rem;
  }

  .key .stable {
    background: #7fb06955;
  }

  .key .stalled {
    background: #e0a35f55;
  }

  .wrap {
    position: relative;
  }

  .tip {
    position: absolute;
    top: 1.5rem;
    transform: translateX(-50%);
    max-width: 18rem;
    padding: 0.4rem 0.6rem;
    background: #1e2127;
    border: 1px solid #4a4f57;
    pointer-events: none;
    z-index: 10;
  }

  .tip span {
    margin-left: 0.5rem;
    color: #9aa0a6;
  }

  .tip p {
    margin: 0.25rem 0 0;
  }

  /* uPlot's defaults assume a light page. */
  .wrap :global(.u-select) {
    background: rgba(255, 255, 255, 0.08);
  }

  .wrap :global(.u-legend) {
    color: #9aa0a6;
  }
</style>
