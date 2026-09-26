<script lang="ts">
  import type { Config, Rollup, StrainRollup } from "$lib/api";
  import { span } from "$lib/dashboard";
  import { formatSpread, rollupLabel } from "$lib/query";
  import { href } from "$lib/route";
  import { toDisplayTemp } from "$lib/units";

  let {
    rollup,
    codes,
    unevaluated,
    ui,
  }: {
    rollup: Rollup;
    /** Batch codes by id, from the query's rows. */
    codes: Map<number, string>;
    unevaluated: number;
    ui: Config["ui"];
  } = $props();

  const pct = (v: number) => (v * 100).toFixed(1);
  const temp = (c: number) => toDisplayTemp(c, ui.temp_display).toFixed(1);

  function band(r: StrainRollup): { text: string; why: string | null } {
    if (r.band === null) {
      return { text: "—", why: r.group.kind === "mixed" ? "no single strain" : "no band on the strain record" };
    }
    const text = `${(r.band.min * 100).toFixed(0)}–${(r.band.max * 100).toFixed(0)}`;
    if (r.band_check === "below" || r.band_check === "above") {
      return { text: `${text} ${r.band_check === "below" ? "↓" : "↑"}`, why: `median ${r.band_check} the datasheet band` };
    }
    return { text, why: null };
  }
</script>

{#if rollup.rows.length}
  <table>
    <thead>
      <tr>
        <th>Strain</th>
        <th>n</th>
        <th>App. atten. %</th>
        <th>Datasheet %</th>
        <th>Lag</th>
        <th>To terminal</th>
        <th>Temp °{ui.temp_display}</th>
      </tr>
    </thead>
    <tbody>
      {#each rollup.rows as r (r.group.kind === "strain" ? r.group.id : "mixed")}
        {@const b = band(r)}
        <tr class:thin={r.thin} title={r.thin ? "Fewer than 3 batches" : undefined}>
          <td title={r.batch_ids.map((id) => codes.get(id) ?? id).join(", ")}>{rollupLabel(r.group)}</td>
          <td class="num">{r.n}</td>
          <td class="num">{formatSpread(r.attenuation, pct)}</td>
          <td class="num" class:flag={r.band_check === "below" || r.band_check === "above"} title={b.why}>
            {b.text}
          </td>
          <td class="num">{formatSpread(r.lag_secs, span)}</td>
          <td class="num">{formatSpread(r.terminal_secs, span)}</td>
          <td class="num">{formatSpread(r.ferm_temp_c, temp)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
  <p class="hint">Median (range) over the batches that have each value.</p>
{:else}
  <p class="hint">No matched batch has a yeast recorded.</p>
{/if}

{#if rollup.no_yeast.length}
  <p class="hint">
    No yeast recorded, not counted:
    {#each rollup.no_yeast as id, i (id)}
      {i ? ", " : ""}<a href={href({ name: "batch", id })}>{codes.get(id) ?? id}</a>
    {/each}
  </p>
{/if}
{#if unevaluated}
  <p class="hint">{unevaluated} batch{unevaluated === 1 ? "" : "es"} couldn't be judged and aren't counted.</p>
{/if}

<style>
  table {
    border-collapse: collapse;
    width: 100%;
    margin-bottom: 0.75rem;
  }

  th,
  td {
    padding: 0.25rem 0.5rem;
    border-bottom: 1px solid #333842;
    text-align: left;
    white-space: nowrap;
  }

  th {
    color: #9aa0a6;
    font-weight: normal;
  }

  td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  tr.thin td {
    color: #6f757d;
  }

  td.flag {
    color: #e0a35f;
    cursor: help;
  }

  .hint {
    color: #6f757d;
  }

  .hint a {
    color: #e6e6e6;
  }
</style>
