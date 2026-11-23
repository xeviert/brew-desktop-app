<script lang="ts">
  import { onMount, untrack } from "svelte";
  import uPlot from "uplot";
  import "uplot/dist/uPlot.min.css";
  import {
    compareBatches,
    exportComparison,
    listBatches,
    onSyncStatus,
    toApiError,
    type ApiError,
    type Batch,
    type ComparedBatch,
    type CompareView,
    type Config,
    type Missing,
    type RelRange,
    type YMode,
  } from "$lib/api";
  import { assignSlots, buildCompare, colorOf, type ComparePanel, type Panel } from "$lib/compare";
  import { csvName, saveCsv } from "$lib/export";
  import { href } from "$lib/route";
  import { formatTimestamp } from "$lib/units";

  let { ids, ui }: { ids: number[]; ui: Config["ui"] } = $props();

  /** Per sensor series, as the single-batch curve. */
  const MAX_POINTS = 1000;
  const MAX_BATCHES = 8;
  const HEIGHT = 260;
  const HOUR = 3_600;
  const SYNC = "compare";

  let batches = $state<Batch[]>([]);
  let yMode = $state<YMode>("fraction");
  let withTemp = $state(false);
  let showArchived = $state(false);
  let view = $state.raw<CompareView | null>(null);
  let error = $state<ApiError | null>(null);

  // Not state: written from inside `$derived`, and only ever read back there.
  let prevSlots = new Map<number, number>();
  const slots = $derived((prevSlots = assignSlots(prevSlots, ids)));

  const panels = $derived(view ? buildCompare(view, slots, ui.temp_display) : []);

  const hosts: Partial<Record<Panel, HTMLDivElement>> = $state({});
  const charts = new Map<Panel, { u: uPlot; key: string }>();
  let wrap: HTMLDivElement;
  // The zoomed span; `null` is the server's default. Not state: a change is a fetch.
  let zoomed: RelRange | null = null;

  let busy = false;
  let again = false;

  async function refresh() {
    if (ids.length < 2) {
      view = null;
      error = null;
      return;
    }
    if (busy) {
      again = true;
      return;
    }
    busy = true;
    try {
      view = await compareBatches(ids, yMode, withTemp, zoomed, MAX_POINTS);
      error = null;
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
      if (again) {
        again = false;
        void refresh();
      }
    }
  }

  /** Same batches, mode and span as the chart, every point rather than `MAX_POINTS`. */
  async function exportCsv() {
    if (!view) return;
    const name = csvName(["compare", ...view.batches.map((b) => b.code)]);
    try {
      await saveCsv(name, (path) => exportComparison(ids, yMode, withTemp, zoomed, path));
    } catch (e) {
      error = toApiError(e);
    }
  }

  function toggle(id: number) {
    const next = ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id];
    location.hash = href({ name: "compare", ids: next });
  }

  const shown = () => zoomed ?? view?.span ?? null;

  function applyRange(u: uPlot) {
    const r = shown();
    if (r) u.setScale("x", { min: r.start / HOUR, max: r.end / HOUR });
  }

  /** Every synced panel sees the same drag, so the first one through does the zoom. */
  function zoom(u: uPlot) {
    const { left, width } = u.select;
    if (width < 2) return;
    const start = Math.floor(u.posToVal(left, "x") * HOUR);
    const end = Math.ceil(u.posToVal(left + width, "x") * HOUR);
    u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
    if (end <= start || (zoomed?.start === start && zoomed?.end === end)) return;
    zoomed = { start, end };
    for (const c of charts.values()) applyRange(c.u);
    void refresh();
  }

  function resetZoom() {
    if (zoomed === null) return;
    zoomed = null;
    void refresh();
  }

  const AXIS = {
    stroke: "#9aa1ab",
    grid: { stroke: "#262a33" },
    ticks: { stroke: "#313642" },
    font: "12px 'Inter Variable', system-ui, sans-serif",
    labelFont: "12px 'Inter Variable', system-ui, sans-serif",
  };

  function yAxis(panel: Panel, unit: string): uPlot.Axis {
    switch (panel) {
      case "frac":
        return {
          ...AXIS,
          label: "apparent attenuation",
          values: (_u, splits) => splits.map((v) => `${Math.round(v * 100)} %`),
          size: 60,
        };
      case "grav": {
        const digits = unit === "SG" ? 3 : 1;
        return { ...AXIS, label: unit, values: (_u, splits) => splits.map((v) => v.toFixed(digits)), size: 60 };
      }
      case "temp":
        return { ...AXIS, label: `°${ui.temp_display}`, size: 60 };
    }
  }

  function build(p: ComparePanel, host: HTMLDivElement, unit: string, last: boolean): uPlot {
    const u = new uPlot(
      {
        width: host.clientWidth,
        height: HEIGHT,
        scales: { x: { time: false } },
        series: [
          { label: "Since pitch", value: (_u, v) => (v == null ? "--" : `${v.toFixed(1)} h`) },
          ...p.drawn.map((d): uPlot.Series =>
            d.role === "manual"
              ? {
                  label: `${d.code} manual`,
                  stroke: d.color,
                  paths: () => null,
                  points: { show: true, size: 8, fill: d.color, stroke: "#0f1115", width: 2 },
                }
              : { label: d.code, stroke: d.color, width: 2, points: { show: false } },
          ),
        ],
        axes: [
          {
            ...AXIS,
            label: last ? "hours since pitch" : undefined,
            values: (_u, splits) => splits.map((v) => `${v} h`),
          },
          yAxis(p.panel, unit),
        ],
        cursor: {
          sync: { key: SYNC },
          drag: { x: true, y: false, setScale: false },
          bind: { dblclick: () => () => null },
        },
        hooks: { setSelect: [zoom] },
      },
      p.data as uPlot.AlignedData,
      host,
    );
    u.over.addEventListener("dblclick", resetZoom);
    return u;
  }

  $effect(() => {
    const ps = panels;
    const unit = view?.gravity_unit ?? "SG";
    // Hosts are bound after the panel list renders, and this re-runs when they are.
    const els = { ...hosts };
    untrack(() => {
      for (const [panel, c] of charts) {
        if (!ps.some((p) => p.panel === panel)) {
          c.u.destroy();
          charts.delete(panel);
        }
      }
      ps.forEach((p, i) => {
        const host = els[p.panel];
        if (!host) return;
        const last = i === ps.length - 1;
        const key = [unit, ui.temp_display, last, p.drawn.map((d) => `${d.batchId}:${d.role}`).join()].join("|");
        const c = charts.get(p.panel);
        if (c && c.key === key && c.u.root.parentElement === host) {
          c.u.setData(p.data as uPlot.AlignedData);
        } else {
          c?.u.destroy();
          charts.set(p.panel, { u: build(p, host, unit, last), key });
        }
        applyRange(charts.get(p.panel)!.u);
      });
    });
  });

  // Picks, toggles and the gravity unit (raw batches are converted server-side) all
  // refetch; a new pick also drops the zoom, whose span was chosen for the old set.
  $effect(() => {
    void ids.join();
    untrack(() => (zoomed = null));
  });
  $effect(() => {
    void ids.join();
    void yMode;
    void withTemp;
    void ui.gravity_display;
    untrack(() => void refresh());
  });

  function note(b: ComparedBatch): string | null {
    if (b.y.kind !== "raw" || b.y.reason === null) return null;
    return b.y.reason === "no_og" ? "raw — no OG" : "raw — OG implausible";
  }

  // Pitched first, newest first: only they can be lined up at t=0. A picked batch
  // always stays listed, so it can be unticked.
  const listed = $derived(
    batches
      .filter((b) => ids.includes(b.id) || showArchived || b.archived_at === null)
      .sort((a, b) => (b.pitched_at ?? -Infinity) - (a.pitched_at ?? -Infinity)),
  );
  const archivedCount = $derived(batches.filter((b) => b.archived_at !== null).length);

  function about(b: Batch): string {
    const parts = [b.beverage_type, b.archived_at !== null ? "archived" : b.status];
    parts.push(b.pitched_at === null ? "not pitched" : `pitched ${formatTimestamp(b.pitched_at, { dateStyle: "medium" })}`);
    return parts.join(" · ");
  }

  const REASONS: Partial<Record<Missing, string>> = { no_pitch: "not pitched" };
  const reason = (m: Missing) => REASONS[m] ?? m.replaceAll("_", " ");

  onMount(() => {
    void (async () => {
      try {
        batches = await listBatches(true);
      } catch (e) {
        error = toApiError(e);
      }
    })();
    const unlisten = onSyncStatus((s) => {
      if (s.rows_added === 0 || !view) return;
      const drawn = view.batches.flatMap((b) => [...b.gravity, ...(b.temp ? [b.temp] : [])]);
      if (drawn.length === 0 || drawn.some((x) => x.source_id === s.source_id)) void refresh();
    });
    const resize = new ResizeObserver(() => {
      for (const [panel, c] of charts) {
        const host = hosts[panel];
        if (host) c.u.setSize({ width: host.clientWidth, height: HEIGHT });
      }
    });
    resize.observe(wrap);
    return () => {
      unlisten();
      resize.disconnect();
      for (const c of charts.values()) c.u.destroy();
      charts.clear();
    };
  });
</script>

<section>
  <h2>Compare</h2>
  <p class="intro">
    Overlay the fermentation curves of two or more batches, each starting at its own pitch (hour 0), to see which
    fermented faster or further — a new yeast against the old one, say, or the same recipe in summer and winter.
  </p>

  <fieldset class="picker">
    <legend>
      Batches <span class="hint">({ids.length} of up to {MAX_BATCHES} ticked)</span>
      {#if archivedCount}
        <label class="archived"><input type="checkbox" bind:checked={showArchived} /> Show archived</label>
      {/if}
    </legend>
    {#if batches.length === 0}
      <p class="empty">No batches yet.</p>
    {/if}
    {#each listed as b (b.id)}
      <label class:off={b.pitched_at === null}>
        <input
          type="checkbox"
          checked={ids.includes(b.id)}
          disabled={!ids.includes(b.id) && (ids.length >= MAX_BATCHES || b.pitched_at === null)}
          onchange={() => toggle(b.id)}
        />
        {#if ids.includes(b.id)}<i style:background={colorOf(slots, b.id)}></i>{/if}
        {b.code}
        <span class="hint">{about(b)}</span>
      </label>
    {/each}
  </fieldset>

  {#if ids.length >= 2}
    <div class="toolbar">
      <span class="hint">Gravity as</span>
      <div class="toggle" role="group" aria-label="Gravity as">
        <button
          class:on={yMode === "fraction"}
          onclick={() => (yMode = "fraction")}
          title="How much of each batch's sugar has fermented, from its own OG">Attenuation %</button
        >
        <button class:on={yMode === "raw"} onclick={() => (yMode = "raw")} title="The gravity readings as taken"
          >{view?.gravity_unit ?? "SG"}</button
        >
      </div>
      <label><input type="checkbox" bind:checked={withTemp} /> Show temperature</label>
      <span class="spacer"></span>
      <button disabled={!view} title="Save these series as CSV" onclick={exportCsv}>Export CSV</button>
    </div>
    <p class="hint">
      {yMode === "fraction"
        ? "Attenuation puts batches with different starting gravities on one scale: 0 % at its OG, 100 % at 1.000 (a dry mead or wine goes past it)."
        : "Raw gravity: batches with different OGs start at different heights."}
      Drag across a chart to zoom, double-click to reset.
    </p>
  {/if}

  {#if ids.length < 2}
    <p class="empty">Tick {ids.length === 0 ? "two or more batches" : "one more batch"} above to see their curves.</p>
  {:else if error}
    <p role="alert">{error.message}</p>
  {:else if view}
    <ul class="key">
      {#each view.batches as b (b.batch_id)}
        <li>
          <i style:background={colorOf(slots, b.batch_id)}></i>
          <a href={href({ name: "batch", id: b.batch_id })}>{b.code}</a>
          {#if note(b)}<span class="hint">{note(b)}</span>{/if}
        </li>
      {/each}
    </ul>
    {#if view.excluded.length}
      <p class="hint">
        Left out:
        {#each view.excluded as x, i (x.batch_id)}{i ? ", " : ""}{x.code} ({reason(x.reason)}){/each}
      </p>
    {/if}
    {#if view.batches.length < 2}
      <p class="empty">Fewer than two pitched batches — nothing to line up yet.</p>
    {:else if panels.length === 0}
      <p class="empty">No readings in this range.</p>
    {/if}
  {/if}

  <div class="panels" bind:this={wrap}>
    {#each panels as p (p.panel)}
      <div bind:this={hosts[p.panel]}></div>
    {/each}
  </div>
</section>

<style>
  .picker {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 1rem;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    margin: 0 0 0.75rem;
  }

  .picker label.off {
    color: var(--text-faint);
  }

  .picker legend {
    display: flex;
    gap: 1rem;
    align-items: baseline;
  }

  .picker .archived {
    font-size: 0.9rem;
    color: var(--text-muted);
  }

  .intro {
    max-width: 44rem;
    color: var(--text-muted);
    line-height: 1.45;
  }

  .spacer {
    flex: 1;
  }

  .toolbar {
    display: flex;
    align-items: center;
    gap: 1rem;
    margin-bottom: 0.5rem;
  }

  .toggle button {
    background: var(--surface-2);
    color: var(--text-muted);
    border: 1px solid var(--border-strong);
    padding: 0.25rem 0.6rem;
  }

  .toggle button.on {
    background: var(--accent);
    color: var(--text);
  }

  .hint,
  .empty {
    color: var(--text-faint);
  }

  .key {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 1.25rem;
    list-style: none;
    padding: 0;
    margin: 0 0 0.5rem;
  }

  i {
    display: inline-block;
    width: 0.8rem;
    height: 0.8rem;
    margin-right: 0.3rem;
    border-radius: var(--radius-sm);
    vertical-align: -0.1rem;
  }

  .key a {
    color: var(--text);
  }

  .panels :global(.u-select) {
    background: rgba(255, 255, 255, 0.08);
  }

  .panels :global(.u-legend) {
    color: var(--text-muted);
  }
</style>
