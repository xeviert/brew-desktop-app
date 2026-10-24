<script lang="ts">
  import { archiveBatch, listBatches, restoreBatch, toApiError, type ApiError, type BatchSummary } from "$lib/api";
  import { byStatus, recipeLabel } from "$lib/batch";
  import { href } from "$lib/route";
  import { formatTimestamp } from "$lib/units";

  let batches = $state<BatchSummary[]>([]);
  let showArchived = $state(false);
  let loaded = $state(false);
  let error = $state<ApiError | null>(null);

  const groups = $derived(byStatus(batches));

  async function load() {
    try {
      batches = await listBatches(showArchived);
    } catch (e) {
      error = toApiError(e);
    } finally {
      loaded = true;
    }
  }

  // Runs on mount, and again when the toggle flips.
  $effect(() => {
    void load();
  });

  async function setArchived(batch: BatchSummary, archived: boolean) {
    error = null;
    try {
      await (archived ? archiveBatch(batch.id) : restoreBatch(batch.id));
      await load();
    } catch (e) {
      error = toApiError(e);
    }
  }
</script>

<section class="ref">
  <nav class="bar">
    <h2>Batches</h2>
    <a class="new" href={href({ name: "batch-new", version: null })}>New batch</a>
    <label class="check">
      <input type="checkbox" bind:checked={showArchived} />
      <span>Show archived</span>
    </label>
  </nav>

  {#if loaded && groups.length === 0}
    <p class="empty">No batches yet.</p>
  {/if}
  {#each groups as g (g.status)}
    <h3>{g.status}</h3>
    {#each g.items as b (b.id)}
      <div class="row" class:archived={b.archived_at !== null}>
        <a href={href({ name: "batch", id: b.id })}>{b.code}</a>
        <span class="detail">
          {recipeLabel(b)}
          {b.vessel_name ? `· ${b.vessel_name}` : ""}
          {b.pitched_at !== null ? `· pitched ${formatTimestamp(b.pitched_at, { dateStyle: "medium" })}` : ""}
        </span>
        <span class="row-actions">
          {#if b.archived_at === null}
            <button class="link" onclick={() => setArchived(b, true)}>Archive</button>
          {:else}
            <button class="link" onclick={() => setArchived(b, false)}>Restore</button>
          {/if}
        </span>
      </div>
    {/each}
  {/each}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  section {
    max-width: 48rem;
  }

  .bar {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  .bar .new {
    color: #7ba7d4;
  }

  .bar .check {
    display: flex;
    gap: 0.4rem;
    margin: 0 0 0 auto;
  }

  h3 {
    margin: 1.25rem 0 0.25rem;
    color: #9aa0a6;
    font-weight: normal;
    font-size: 0.95rem;
    text-transform: capitalize;
  }

  .row {
    display: grid;
    grid-template-columns: 12rem 1fr auto;
    align-items: baseline;
    gap: 1rem;
    padding: 0.3rem 0;
    border-bottom: 1px solid #2a2e36;
  }

  .row a {
    color: inherit;
  }

  .row.archived a,
  .detail {
    color: #6f757d;
  }
</style>
