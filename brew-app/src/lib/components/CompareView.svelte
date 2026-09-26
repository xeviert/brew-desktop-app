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

  const AXIS = { stroke: "#9aa0a6", grid: { stroke: "#2a2e36" }, ticks: { stroke: "#333842" } };

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
          {},
          ...p.drawn.map((d): uPlot.Series =>
            d.role === "manual"
              ? {
                  label: `${d.code} manual`,
                  stroke: d.color,
                  paths: () => null,
                  points: { show: true, size: 8, fill: d.color, stroke: "#16181d", width: 2 },
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

  <fieldset class="picker">
    <legend>Batches <span class="hint">({ids.length} of up to {MAX_BATCHES})</span></legend>
    {#each batches as b (b.id)}
      <label class:off={b.pitched_at === null}>
        <input
          type="checkbox"
          checked={ids.includes(b.id)}
          disabled={!ids.includes(b.id) && ids.length >= MAX_BATCHES}
          onchange={() => toggle(b.id)}
        />
        {#if ids.includes(b.id)}<i style:background={colorOf(slots, b.id)}></i>{/if}
        {b.code}
        <span class="hint">
          {b.archived_at !== null ? "archived" : b.status}{b.pitched_at === null ? " · not pitched" : ""}
        </span>
      </label>
    {/each}
  </fieldset>

  <div class="toolbar">
    <div class="toggle" role="group" aria-label="Gravity as">
      <button class:on={yMode === "fraction"} onclick={() => (yMode = "fraction")}>Attenuation</button>
      <button class:on={yMode === "raw"} onclick={() => (yMode = "raw")}>{view?.gravity_unit ?? "SG"}</button>
    </div>
    <label><input type="checkbox" bind:checked={withTemp} /> Temperature</label>
    <span class="hint">Drag to zoom, double-click to reset.</span>
    <button disabled={!view} title="Save these series as CSV" onclick={exportCsv}>Export</button>
  </div>

  {#if ids.length < 2}
    <p class="empty">Pick at least {2 - ids.length} more batch{ids.length === 1 ? "" : "es"} to compare.</p>
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
    border: 1px solid #333842;
    margin: 0 0 0.75rem;
  }

  .picker label.off {
    color: #6f757d;
  }

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

  .hint,
  .empty {
    color: #6f757d;
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
    border-radius: 2px;
    vertical-align: -0.1rem;
  }

  .key a {
    color: #e6e6e6;
  }

  .panels :global(.u-select) {
    background: rgba(255, 255, 255, 0.08);
  }

  .panels :global(.u-legend) {
    color: #9aa0a6;
  }
</style>
