<script lang="ts">
  import uPlot from "uplot";
  import {
    batchActuals,
    batchDashboard,
    batchPackaging,
    batchSeries,
    batchTimeline,
    getBatch,
    toApiError,
    vesselCleanings,
    type ApiError,
    type Config,
    type Curve,
  } from "$lib/api";
  import { buildPlot, type Drawn } from "$lib/chart";
  import { buildReport, reportText, type Report } from "$lib/report";
  import { href } from "$lib/route";

  // 7e: one batch's record on a white sheet, printed as it is shown. Everything here is
  // what the batch screen already fetches; `report.ts` puts it together.
  let { batchId, ui }: { batchId: number; ui: Config["ui"] } = $props();

  // Rendered at twice the sheet's width, so the printed curve stays sharp.
  const CHART_WIDTH = 1400;
  const CHART_HEIGHT = 520;

  let report = $state.raw<Report | null>(null);
  let chart = $state<string | null>(null);
  let error = $state<ApiError | null>(null);
  let copied = $state(false);
  let host: HTMLDivElement;

  async function load() {
    try {
      const batch = await getBatch(batchId);
      const [dashboard, timeline, actuals, packaging, curve, cleaning] = await Promise.all([
        batchDashboard(batchId),
        batchTimeline(batchId),
        batchActuals(batchId),
        batchPackaging(batchId),
        batchSeries(batchId, null, 600),
        batch.vessel_id !== null ? vesselCleanings(batch.vessel_id).then((l) => l.entries) : null,
      ]);
      report = buildReport({
        batch,
        dashboard,
        timeline,
        actuals,
        packaging,
        curve,
        cleaning,
        temp: ui.temp_display,
        now: Date.now() / 1000,
      });
      chart = await draw(curve, batch.pitched_at);
    } catch (e) {
      error = toApiError(e);
    }
  }

  $effect(() => {
    void load();
  });

  /** The curve as an image: a canvas does not print reliably, an `<img>` does. uPlot draws
   * on a microtask after construction, so the image is taken in its `draw` hook. */
  async function draw(curve: Curve, pitchedAt: number | null): Promise<string | null> {
    if (curve.range === null || curve.series.length === 0) return null;
    const plot = buildPlot(curve, pitchedAt !== null ? "rel" : "abs", pitchedAt, ui.temp_display);
    // Hand readings and the smoothed line; raw points and bucket bands are screen detail.
    const keep = (d: Drawn) => d.role === "line" || d.role === "smoothed" || d.role === "manual";
    const axis = { stroke: "#333", grid: { stroke: "#e2e2e2" }, ticks: { stroke: "#bbb" }, font: "22px system-ui" };
    const axes: uPlot.Axis[] = [
      pitchedAt !== null
        ? { ...axis, label: "hours since pitch", labelFont: "22px system-ui", values: (_u, s) => s.map((v) => `${v} h`) }
        : axis,
    ];
    if (plot.axes.includes("grav"))
      axes.push({ ...axis, scale: "grav", label: curve.gravity_unit, labelFont: "22px system-ui", size: 110 });
    if (plot.axes.includes("temp"))
      axes.push({
        ...axis,
        scale: "temp",
        side: 1,
        label: `°${ui.temp_display}`,
        labelFont: "22px system-ui",
        grid: { show: false },
        size: 80,
      });
    let drawn: (url: string) => void = () => {};
    const url = new Promise<string>((resolve) => (drawn = resolve));
    const u = new uPlot(
      {
        width: CHART_WIDTH,
        height: CHART_HEIGHT,
        scales: { x: { time: pitchedAt === null } },
        legend: { show: false },
        cursor: { show: false },
        series: [
          {},
          ...plot.drawn.map((d) =>
            keep(d)
              ? {
                  scale: d.axis,
                  stroke: d.color,
                  width: d.role === "manual" ? 2 : 3,
                  dash: d.role === "manual" ? [4, 8] : undefined,
                  points: { show: d.role === "manual", size: 10, fill: d.color },
                }
              : { scale: d.axis, show: false },
          ),
        ],
        axes,
        hooks: { draw: [(u) => drawn(u.ctx.canvas.toDataURL("image/png"))] },
      },
      plot.data as uPlot.AlignedData,
      host,
    );
    try {
      return await url;
    } finally {
      u.destroy();
    }
  }

  async function copy() {
    if (!report) return;
    await navigator.clipboard.writeText(reportText(report));
    copied = true;
    setTimeout(() => (copied = false), 2000);
  }
</script>

<div class="bar no-print">
  <a href={href({ name: "batch", id: batchId })}>← Back to the batch</a>
  <button onclick={() => window.print()} disabled={!report}>Print / Save as PDF</button>
  <button class="link" onclick={copy} disabled={!report}>{copied ? "Copied" : "Copy as text"}</button>
</div>

<!-- Off screen: uPlot needs a parent to draw into, and only the image is kept. -->
<div class="offscreen" bind:this={host}></div>

{#if error}
  <p role="alert">{error.message}</p>
{:else if report}
  <article class="sheet">
    <h1>Batch {report.code}</h1>
    <dl>
      {#each report.facts as f (f.label)}
        <dt>{f.label}</dt>
        <dd>{f.value}</dd>
      {/each}
    </dl>
    {#if report.hygiene}<p class="note">{report.hygiene}</p>{/if}

    {#if chart}
      <img src={chart} alt="Fermentation curve of {report.code}" />
    {/if}

    {#if report.fermentation.length > 0}
      <h2>Fermentation</h2>
      <table>
        <thead>
          <tr><th></th><th>Gravity ({report.gravityUnit})</th><th>Temperature</th><th>pH</th></tr>
        </thead>
        <tbody>
          {#each report.fermentation as row (row.at)}
            <tr>
              <th>{row.at}</th>
              <td>{row.gravity ?? ""}</td>
              <td>{row.temp ?? ""}</td>
              <td>{row.ph?.replace("pH ", "") ?? ""}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    {#if report.ingredients.length > 0}
      <h2>Ingredients</h2>
      <ul>
        {#each report.ingredients as i, n (n)}
          <li><span class="stage">{i.stage}</span> {i.text}{#if i.cost}<span class="cost">{i.cost}</span>{/if}</li>
        {/each}
      </ul>
    {/if}
    {#if report.cost}<p><strong>Cost</strong> {report.cost}</p>{/if}

    {#if report.events.length > 0}
      <h2>Events</h2>
      <ul>
        {#each report.events as e, n (n)}
          <li><span class="when">{e.when}</span> {e.text}</li>
        {/each}
      </ul>
    {/if}

    {#if report.packaging.length > 0}
      <h2>Packaging</h2>
      <ul>
        {#each report.packaging as line, n (n)}
          <li>{line}</li>
        {/each}
      </ul>
      {#if report.packagingSummary}<p>{report.packagingSummary}</p>{/if}
    {/if}
  </article>
{:else}
  <p>Loading…</p>
{/if}

<style>
  .bar {
    display: flex;
    gap: 1rem;
    align-items: center;
    margin-bottom: 1rem;
  }

  .bar a,
  .bar .link {
    color: #7ba7d4;
  }

  .bar button {
    padding: 0.4rem 1rem;
    background: #2d5d8a;
    color: #fff;
    border: none;
    border-radius: 3px;
    font: inherit;
    cursor: pointer;
  }

  .bar button.link {
    padding: 0;
    background: none;
  }

  .offscreen {
    position: absolute;
    left: -10000px;
    top: 0;
  }

  /* A sheet of paper on screen, so the preview is the print. */
  .sheet {
    max-width: 48rem;
    padding: 2rem 2.5rem;
    background: #fff;
    color: #111;
    border-radius: 2px;
    font-size: 13px;
    line-height: 1.45;
  }

  h1 {
    margin: 0 0 1rem;
    font-size: 1.6rem;
  }

  h2 {
    margin: 1.5rem 0 0.5rem;
    font-size: 1.05rem;
    border-bottom: 1px solid #ccc;
    padding-bottom: 0.2rem;
    break-after: avoid;
  }

  dl {
    grid-template-columns: max-content 1fr;
    gap: 0.2rem 1.5rem;
  }

  dt,
  .note,
  .stage,
  .when {
    color: #555;
  }

  .stage {
    display: inline-block;
    min-width: 6.5rem;
    text-transform: capitalize;
  }

  .when {
    display: inline-block;
    min-width: 11rem;
  }

  .cost {
    margin-left: 0.75rem;
    color: #555;
  }

  img {
    display: block;
    width: 100%;
    margin: 1rem 0 0;
  }

  table {
    border-collapse: collapse;
    font-variant-numeric: tabular-nums;
  }

  th,
  td {
    text-align: left;
    padding: 0.1rem 1.5rem 0.1rem 0;
  }

  thead th {
    color: #555;
    font-weight: normal;
    border-bottom: 1px solid #ccc;
  }

  tbody th {
    font-weight: normal;
    color: #555;
  }

  tr {
    break-inside: avoid;
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    padding: 0.1rem 0;
    break-inside: avoid;
  }

  @media print {
    .sheet {
      max-width: none;
      padding: 0;
    }
  }
</style>
