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
        <div class="actuals">
          <Actuals
            batchId={batch.id}
            pitchedAt={batch.pitched_at}
            {readonly}
            {revision}
            onchanged={() => void changed()}
          />
        </div>
      </div>
      <div class="side">
        <div class="schedule">
          <Schedule batchId={batch.id} ui={config.ui} {readonly} {revision} onchanged={() => void changed()} />
        </div>
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

  /* Narrow: one column, the schedule and card first and the timeline before the ingredients, so
     "Add reading" stays near the numbers it changes. */
  .screen {
    display: flex;
    flex-direction: column;
  }

  .main,
  .side {
    display: contents;
  }

  .schedule {
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

  .actuals {
    order: 6;
  }

  /* Wide: the curve and ingredients beside the schedule, card, moves and timeline. */
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
