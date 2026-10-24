<script lang="ts">
  import { onMount } from "svelte";
  import BatchLifecycle from "$lib/components/BatchLifecycle.svelte";
  import Curve from "$lib/components/Curve.svelte";
  import StatusCard from "$lib/components/StatusCard.svelte";
  import Timeline from "$lib/components/Timeline.svelte";
  import {
    exportBatchReadings,
    listBatches,
    toApiError,
    type ApiError,
    type BatchSummary,
    type Config,
    type SourceStatus,
  } from "$lib/api";
  import { csvName, saveCsv } from "$lib/export";
  import { href } from "$lib/route";

  // A stand-in host until U6's batch screen: a picker, the lifecycle panel, the card and
  // the curve.
  // The batch is the route's; the picker navigates rather than holding its own.
  let {
    config,
    batchId,
    syncs,
  }: { config: Config; batchId: number; syncs: Record<number, SourceStatus> } = $props();

  let batches = $state<BatchSummary[]>([]);
  let loaded = $state(false);
  let error = $state<ApiError | null>(null);
  let exportError = $state<ApiError | null>(null);

  // Bumped by every write from this screen. A manual reading or an event raises no
  // `sync://status`, so without it the card, the curve and the timeline would not see it.
  let revision = $state(0);

  async function changed() {
    await load();
    revision++;
  }

  const batch = $derived(batches.find((b) => b.id === batchId) ?? null);

  // Archived ones too: the route may name one, and restoring it happens here.
  async function load() {
    try {
      batches = await listBatches(true);
    } catch (e) {
      error = toApiError(e);
    } finally {
      loaded = true;
    }
  }

  onMount(() => {
    void load();
  });

  /** Every reading bound to the batch, not the curve's zoom. */
  async function exportReadings() {
    if (!batch) return;
    const { id, code } = batch;
    try {
      await saveCsv(csvName([code, "readings"]), (path) => exportBatchReadings(id, path));
      exportError = null;
    } catch (e) {
      exportError = toApiError(e);
    }
  }

  function pick(e: Event) {
    location.hash = href({ name: "batch", id: Number((e.currentTarget as HTMLSelectElement).value) });
  }
</script>

<section>
  <h2>Batch</h2>

  {#if error}
    <p role="alert">{error.message}</p>
  {:else if loaded && batches.length === 0}
    <p class="empty">No batches yet. <a href={href({ name: "batch-new", version: null })}>Create one</a>.</p>
  {:else if loaded}
    <select value={batchId} onchange={pick}>
      {#each batches as b (b.id)}
        <option value={b.id}>{b.code} — {b.status}{b.archived_at !== null ? " (archived)" : ""}</option>
      {/each}
    </select>
    <a class="compare" href={href({ name: "compare", ids: [batchId] })}>Compare with…</a>
    <button class="export" disabled={!batch} onclick={exportReadings}>Export readings</button>
    {#if exportError}<p role="alert">{exportError.message}</p>{/if}

    {#if batch}
      <BatchLifecycle {batch} onchanged={changed} />
      <!-- Keyed: a curve's zoom and cached data belong to one batch, and any write from
           this screen changes what the card and curve read. -->
      {#key `${batch.id}:${revision}`}
        <StatusCard batchId={batch.id} code={batch.code} ui={config.ui} {syncs} />
        <Curve batchId={batch.id} pitchedAt={batch.pitched_at} ui={config.ui} />
      {/key}
      {#key batch.id}
        <Timeline
          batchId={batch.id}
          {config}
          readonly={batch.archived_at !== null}
          {revision}
          onchanged={() => void changed()}
        />
      {/key}
    {:else}
      <p class="empty">No batch {batchId}.</p>
    {/if}
  {/if}
</section>

<style>
  select {
    margin-bottom: 0.75rem;
  }

  .compare {
    margin-left: 1rem;
    color: #9aa0a6;
  }

  .export {
    margin-left: 1rem;
  }

  .empty {
    color: #6f757d;
  }
</style>
