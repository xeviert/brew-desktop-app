<script lang="ts">
  import { onMount, untrack } from "svelte";
  import Devices from "$lib/components/Devices.svelte";
  import {
    homeSummary,
    onSyncStatus,
    toApiError,
    type ApiError,
    type Config,
    type HomeSummary,
    type SourceStatus,
  } from "$lib/api";
  import { formatGravityIn, gravitySource, stabilityText } from "$lib/dashboard";
  import { age } from "$lib/format";
  import { quietSources, silenceText } from "$lib/home";
  import { href } from "$lib/route";
  import { formatTemp } from "$lib/units";

  let { ui, syncs }: { ui: Config["ui"]; syncs: Record<number, SourceStatus> } = $props();

  let home = $state.raw<HomeSummary | null>(null);
  let error = $state<ApiError | null>(null);

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
      home = await homeSummary();
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
  onMount(() => onSyncStatus(() => void refresh()));
</script>

<section>
  <h2>Fermenting</h2>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}

  {#if home}
    {#if home.batches.length === 0}
      <p class="muted">Nothing fermenting or conditioning.</p>
    {/if}

    <div class="cards">
      {#each home.batches as b (b.dashboard.batch_id)}
        {@const quiet = quietSources(b.streams, syncs)}
        <a class="card" href={href({ name: "batch", id: b.dashboard.batch_id })}>
          <h3>
            {b.code}
            <span class="muted">{b.status}{b.vessel ? ` · ${b.vessel}` : ""}</span>
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

            <dt>Status</dt>
            <dd>{stabilityText(b.dashboard.stability, now)}</dd>

            <dt>Newest</dt>
            <dd>{age(b.newest)}</dd>
          </dl>

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
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(18rem, 1fr));
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

  h3 {
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
