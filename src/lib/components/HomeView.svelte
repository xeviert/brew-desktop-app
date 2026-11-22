<script lang="ts">
  import { onMount, untrack } from "svelte";
  import AlertList from "$lib/components/AlertList.svelte";
  import Devices from "$lib/components/Devices.svelte";
  import {
    homeSummary,
    listAlerts,
    onAlerts,
    onSyncStatus,
    type AlertRow,
    toApiError,
    type ApiError,
    type Config,
    type HomeSummary,
    type SourceStatus,
  } from "$lib/api";
  import { alertTitle, openByBatch } from "$lib/alerts";
  import { formatGravityIn, gravitySource, stabilityText } from "$lib/dashboard";
  import { age } from "$lib/format";
  import { quietSources, silenceText } from "$lib/home";
  import { href } from "$lib/route";
  import { formatTemp } from "$lib/units";

  let { ui, syncs }: { ui: Config["ui"]; syncs: Record<number, SourceStatus> } = $props();

  let home = $state.raw<HomeSummary | null>(null);
  let error = $state<ApiError | null>(null);
  // Phase 6. Every open alert, from the last round `brewd`'s log was read in.
  let alerts = $state.raw<AlertRow[]>([]);
  const alertsByBatch = $derived(openByBatch(alerts));

  const unit = $derived(home?.gravity_unit ?? "SG");
  const now = $derived(home ? Date.now() / 1000 : 0);

  const guessFor = (sourceId: number) =>
    home?.devices.find((d) => d.source_id === sourceId)?.guess ?? null;

  let busy = false;
  let again = false;

  async function refresh() {
    if (busy) {
      again = true;
      return;
    }
    busy = true;
    try {
      [home, alerts] = await Promise.all([homeSummary(), listAlerts(null, true)]);
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

  $effect(() => {
    void ui.gravity_display;
    untrack(() => void refresh());
  });

  // Every poll of any source: a poll that adds nothing is when a stream goes stale.
  // And every alert round that changed something.
  onMount(() => {
    const stopSync = onSyncStatus(() => void refresh());
    const stopAlerts = onAlerts(() => void refresh());
    return () => {
      stopSync();
      stopAlerts();
    };
  });
</script>

<section>
  <nav class="bar">
    <h2>Fermenting and conditioning</h2>
    <a href={href({ name: "batches" })}>All batches</a>
    <a href={href({ name: "batch-new", version: null })}>New batch</a>
  </nav>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}

  {#if alerts.length > 0}
    <section class="open-alerts" aria-labelledby="open-alerts-title">
      <h3 id="open-alerts-title">Alerts</h3>
      <AlertList {alerts} showBatch onchanged={() => void refresh()} />
    </section>
  {/if}

  {#if home}
    {#if home.batches.length === 0}
      <p class="muted">
        Nothing fermenting or conditioning. Batches show here once pitched, with their latest readings —
        <a href={href({ name: "batch-new", version: null })}>start one</a>.
      </p>
    {/if}

    <div class="cards">
      {#each home.batches as b (b.dashboard.batch_id)}
        {@const quiet = quietSources(b.streams, syncs)}
        {@const stalled = b.dashboard.stability.state === "stalled"}
        {@const e = b.dashboard.estimate}
        {@const raised = alertsByBatch.get(b.dashboard.batch_id) ?? []}
        <a
          class="card"
          class:attention={stalled || quiet.length > 0 || raised.length > 0}
          href={href({ name: "batch", id: b.dashboard.batch_id })}
        >
          <h3>
            {b.code}
            <span class="pill">{b.status}</span>
            {#if b.vessel}<span class="muted">{b.vessel}</span>{/if}
          </h3>
          <dl>
            <dt>Gravity</dt>
            <dd>
              {#if b.dashboard.gravity && b.display.gravity !== null}
                {formatGravityIn(b.display.gravity, unit)}
                <span class="muted">{gravitySource(b.dashboard.gravity)}</span>
              {:else}
                <span class="muted">—</span>
              {/if}
            </dd>

            {#if e.state === "estimated"}
              <dt>Attenuation</dt>
              <dd>
                <span class="bar-track"><span class="bar-fill" style:width="{Math.min(100, e.apparent_attenuation * 100)}%"
                  ></span></span>
                {(e.apparent_attenuation * 100).toFixed(0)} %
              </dd>

              <dt>ABV</dt>
              <dd>{(e.abv_alternate * 100).toFixed(1)} % <span class="muted">(alternate)</span></dd>
            {/if}

            <dt>Temp</dt>
            <dd>
              {#if b.dashboard.temp}
                {#if b.dashboard.temp.stale}
                  <s>{formatTemp(b.dashboard.temp.value, ui.temp_display)}</s>
                {:else}
                  {formatTemp(b.dashboard.temp.value, ui.temp_display)}
                {/if}
              {:else}
                <span class="muted">—</span>
              {/if}
            </dd>

            <dt>Activity</dt>
            <dd class:warn={stalled}>{stabilityText(b.dashboard.stability, now)}</dd>

            <dt>Last reading</dt>
            <dd>{age(b.newest)}</dd>
          </dl>

          {#if raised.length > 0}
            <p class="raised">Alert: {raised.map(alertTitle).join(", ")}</p>
          {/if}

          {#if quiet.length > 0}
            <ul class="stale">
              {#each quiet as q (q.source_id)}
                <li class={q.silence}>
                  {q.label}{q.metrics ? ` (${q.metrics.join(", ")})` : ""}: {silenceText(q.silence, guessFor(q.source_id))},
                  last {age(q.newest)}
                </li>
              {/each}
            </ul>
          {/if}
        </a>
      {/each}
    </div>

    {#if home.devices.length > 0}
      <h2>Devices</h2>
      <Devices devices={home.devices} {syncs} />
    {/if}
  {:else if !error}
    <p class="muted">Loading…</p>
  {/if}
</section>

<style>
  .bar {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  .bar h2 {
    margin-right: auto;
  }

  .bar a,
  p.muted a {
    color: #7ba7d4;
  }

  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(20rem, 1fr));
    gap: 0.75rem;
    margin-bottom: 1.5rem;
  }

  .card {
    display: block;
    background: #1e2127;
    border: 1px solid #333842;
    padding: 0.75rem 1rem;
    color: inherit;
    text-decoration: none;
  }

  .card:hover {
    border-color: #4a5160;
  }

  .card.attention {
    border-color: #8a6a3a;
  }

  .pill {
    padding: 0.05rem 0.45rem;
    border-radius: 999px;
    background: #2a2e36;
    color: #9aa0a6;
    font-size: 0.8em;
    font-weight: normal;
    vertical-align: 0.1em;
  }

  .bar-track {
    display: inline-block;
    width: 4rem;
    height: 0.4rem;
    margin-right: 0.4rem;
    background: #2a2e36;
    border-radius: 2px;
    vertical-align: 0.1em;
    overflow: hidden;
  }

  .bar-fill {
    display: block;
    height: 100%;
    background: #2d5d8a;
  }

  .warn {
    color: #e0a35f;
  }

  h3 {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem;
    margin: 0 0 0.5rem;
  }

  h3 .muted {
    font-weight: normal;
    font-size: 0.9em;
  }

  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 1rem;
    margin: 0;
  }

  dt {
    color: #9aa0a6;
  }

  dd {
    margin: 0;
  }

  .open-alerts {
    margin-bottom: 1.25rem;
  }

  .open-alerts h3 {
    margin: 0 0 0.5rem;
  }

  .raised {
    margin: 0.5rem 0 0;
    color: #e0a35f;
    font-size: 0.9em;
  }

  .stale {
    list-style: none;
    margin: 0.5rem 0 0;
    padding: 0;
    font-size: 0.9em;
  }

  .silent {
    color: #e0a35f;
  }

  .unreachable,
  .sync_error,
  .muted {
    color: #6f757d;
  }
</style>
