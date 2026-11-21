<script lang="ts">
  import Actuals from "$lib/components/Actuals.svelte";
  import BatchAlerts from "$lib/components/BatchAlerts.svelte";
  import BatchHeader from "$lib/components/BatchHeader.svelte";
  import BatchLifecycle from "$lib/components/BatchLifecycle.svelte";
  import Curve from "$lib/components/Curve.svelte";
  import Schedule from "$lib/components/Schedule.svelte";
  import StatusCard from "$lib/components/StatusCard.svelte";
  import Timeline from "$lib/components/Timeline.svelte";
  import { getBatch, toApiError, type ApiError, type BatchSummary, type Config, type SourceStatus } from "$lib/api";
  import { href } from "$lib/route";

  // The batch screen: what it is and its status moves, the card, the curve, what went in
  // and what was logged. The batch is the route's.
  let {
    config,
    batchId,
    syncs,
  }: { config: Config; batchId: number; syncs: Record<number, SourceStatus> } = $props();

  let batch = $state.raw<BatchSummary | null>(null);
  let missing = $state(false);
  let error = $state<ApiError | null>(null);

  // Bumped by every write from this screen. A manual reading or an event raises no
  // `sync://status`, so without it the card, the curve and the lists would not see it.
  // They refetch in place: the curve keeps its zoom.
  let revision = $state(0);

  async function load(id: number) {
    try {
      const b = await getBatch(id);
      if (id !== batchId) return;
      batch = b;
      missing = false;
      error = null;
    } catch (e) {
      if (id !== batchId) return;
      const err = toApiError(e);
      batch = null;
      if (err.code === "db.no_such_batch") missing = true;
      else error = err;
    }
  }

  async function changed() {
    await load(batchId);
    revision++;
  }

  $effect(() => {
    const id = batchId;
    batch = null;
    missing = false;
    void load(id);
  });

  const readonly = $derived(batch?.archived_at != null);

  const tabs = [
    { id: "schedule", label: "Schedule" },
    { id: "ingredients", label: "Ingredients" },
  ] as const;
  let tab = $state<(typeof tabs)[number]["id"]>("schedule");

  function arrow(event: KeyboardEvent) {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    const i = tabs.findIndex((t) => t.id === tab);
    tab = tabs[(i + (event.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length].id;
    document.getElementById(`tab-${tab}`)?.focus();
  }
</script>

{#if error}
  <p role="alert">{error.message}</p>
{:else if missing}
  <p class="empty">No batch {batchId}. <a href={href({ name: "batches" })}>All batches</a></p>
{:else if batch}
  <!-- Keyed: a curve's zoom and every list's open form belong to one batch. -->
  {#key batch.id}
    <BatchHeader {batch} onchanged={changed} />
    <div class="screen">
      <div class="main">
        <div class="curve">
          <Curve batchId={batch.id} pitchedAt={batch.pitched_at} ui={config.ui} {revision} />
        </div>
        <div class="tabbed">
          <div class="tabs" role="tablist">
            {#each tabs as t (t.id)}
              <button
                role="tab"
                id="tab-{t.id}"
                aria-selected={tab === t.id}
                aria-controls="panel-{t.id}"
                tabindex={tab === t.id ? 0 : -1}
                onclick={() => (tab = t.id)}
                onkeydown={arrow}>{t.label}</button
              >
            {/each}
          </div>
          <!-- Hidden, not unmounted: an open form survives a look at the other tab. -->
          <div role="tabpanel" id="panel-schedule" aria-labelledby="tab-schedule" hidden={tab !== "schedule"}>
            <Schedule batchId={batch.id} ui={config.ui} {readonly} {revision} onchanged={() => void changed()} />
          </div>
          <div role="tabpanel" id="panel-ingredients" aria-labelledby="tab-ingredients" hidden={tab !== "ingredients"}>
            <Actuals
              batchId={batch.id}
              pitchedAt={batch.pitched_at}
              {readonly}
              {revision}
              onchanged={() => void changed()}
            />
          </div>
        </div>
      </div>
      <div class="side">
        <div class="card">
          <StatusCard batchId={batch.id} ui={config.ui} {syncs} {revision} {readonly} />
        </div>
        <div class="moves">
          <BatchLifecycle {batch} onchanged={changed} />
        </div>
        <div class="alerts">
          <BatchAlerts batchId={batch.id} ui={config.ui} {readonly} />
        </div>
        <div class="timeline">
          <Timeline batchId={batch.id} {config} {readonly} {revision} onchanged={() => void changed()} />
        </div>
      </div>
    </div>
  {/key}
{/if}

<style>
  .empty {
    color: #6f757d;
  }

  .empty a {
    color: #7ba7d4;
  }

  /* Narrow: one column, the schedule and ingredients tabs first, then the card, so "Add reading"
     stays near the numbers it changes. */
  .screen {
    display: flex;
    flex-direction: column;
  }

  .main,
  .side {
    display: contents;
  }

  .tabbed {
    order: 1;
  }

  .card {
    order: 2;
  }

  .moves,
  .alerts {
    order: 3;
  }

  .curve {
    order: 4;
  }

  .timeline {
    order: 5;
  }

  .tabs {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid #333842;
    margin-bottom: 1rem;
  }

  .tabs button {
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    padding: 0.5rem 0.9rem;
    color: #9aa0a6;
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  .tabs button:hover {
    color: #e6e6e6;
  }

  .tabs button[aria-selected="true"] {
    color: #e6e6e6;
    border-bottom-color: #4fb3ff;
  }

  /* Wide: the curve and the schedule / ingredients tabs beside the card, moves and timeline. */
  @media (min-width: 1100px) {
    .screen {
      display: grid;
      grid-template-columns: minmax(0, 1fr) 30rem;
      gap: 1.5rem;
      align-items: start;
    }

    .main,
    .side {
      display: block;
      min-width: 0;
    }
  }
</style>
