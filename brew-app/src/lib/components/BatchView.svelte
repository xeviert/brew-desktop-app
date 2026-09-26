<script lang="ts">
  import { onMount } from "svelte";
  import Curve from "$lib/components/Curve.svelte";
  import StatusCard from "$lib/components/StatusCard.svelte";
  import {
    listBatches,
    toApiError,
    type ApiError,
    type Batch,
    type Config,
    type SourceStatus,
  } from "$lib/api";
  import { href } from "$lib/route";

  // A stand-in host until the Phase 3 batch screen exists: a picker, the card and the curve.
  // The batch is the route's; the picker navigates rather than holding its own.
  let {
    config,
    batchId,
    syncs,
  }: { config: Config; batchId: number; syncs: Record<number, SourceStatus> } = $props();

  let batches = $state<Batch[]>([]);
  let loaded = $state(false);
  let error = $state<ApiError | null>(null);

  const batch = $derived(batches.find((b) => b.id === batchId) ?? null);

  onMount(() => {
    void (async () => {
      try {
        batches = await listBatches();
      } catch (e) {
        error = toApiError(e);
      } finally {
        loaded = true;
      }
    })();
  });

  function pick(e: Event) {
    location.hash = href({ name: "batch", id: Number((e.currentTarget as HTMLSelectElement).value) });
  }
</script>

<section>
  <h2>Batch</h2>

  {#if error}
    <p role="alert">{error.message}</p>
  {:else if loaded && batches.length === 0}
    <p class="empty">No batches yet.</p>
  {:else if loaded}
    <select value={batchId} onchange={pick}>
      {#each batches as b (b.id)}
        <option value={b.id}>{b.code} — {b.status}</option>
      {/each}
    </select>
    <a class="compare" href={href({ name: "compare", ids: [batchId] })}>Compare with…</a>

    {#if batch}
      <!-- Keyed: a curve's zoom and cached data belong to one batch. -->
      {#key batch.id}
        <StatusCard batchId={batch.id} code={batch.code} ui={config.ui} {syncs} />
        <Curve batchId={batch.id} pitchedAt={batch.pitched_at} ui={config.ui} />
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

  .empty {
    color: #6f757d;
  }
</style>
