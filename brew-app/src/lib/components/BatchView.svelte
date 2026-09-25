<script lang="ts">
  import { onMount } from "svelte";
  import Curve from "$lib/components/Curve.svelte";
  import { listBatches, toApiError, type ApiError, type Batch, type Config } from "$lib/api";

  // A stand-in host until the Phase 3 batch screen exists: a picker and the curve.
  let { config }: { config: Config } = $props();

  let batches = $state<Batch[]>([]);
  let batchId = $state<number | null>(null);
  let error = $state<ApiError | null>(null);

  const batch = $derived(batches.find((b) => b.id === batchId) ?? null);

  onMount(() => {
    void (async () => {
      try {
        batches = await listBatches();
        batchId = batches.find((b) => b.status === "fermenting")?.id ?? batches[0]?.id ?? null;
      } catch (e) {
        error = toApiError(e);
      }
    })();
  });
</script>

<section>
  <h2>Batch</h2>

  {#if error}
    <p role="alert">{error.message}</p>
  {:else if batches.length === 0}
    <p class="empty">No batches yet.</p>
  {:else}
    <select bind:value={batchId}>
      {#each batches as b (b.id)}
        <option value={b.id}>{b.code} — {b.status}</option>
      {/each}
    </select>

    {#if batch}
      <!-- Keyed: a curve's zoom and cached data belong to one batch. -->
      {#key batch.id}
        <Curve batchId={batch.id} pitchedAt={batch.pitched_at} ui={config.ui} />
      {/key}
    {/if}
  {/if}
</section>

<style>
  select {
    margin-bottom: 0.75rem;
  }

  .empty {
    color: #6f757d;
  }
</style>
