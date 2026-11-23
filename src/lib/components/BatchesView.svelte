<script lang="ts">
  import { archiveBatch, listBatches, restoreBatch, toApiError, type ApiError, type BatchSummary } from "$lib/api";
  import { byStatus, pitchText, recipeLabel } from "$lib/batch";
  import { href } from "$lib/route";

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
    <a class="compare" href={href({ name: "compare", ids: [] })}>Compare curves</a>
    <label class="check">
      <input type="checkbox" bind:checked={showArchived} />
      <span>Show archived</span>
    </label>
  </nav>

  {#if loaded && groups.length === 0}
    <p class="empty">No batches yet. <a href={href({ name: "batch-new", version: null })}>Start one</a>.</p>
  {:else if groups.length}
    <div class="row head">
      <span>Batch</span>
      <span>Type</span>
      <span>Recipe</span>
      <span>Vessel</span>
      <span>Pitched</span>
      <span></span>
    </div>
  {/if}
  {#each groups as g (g.status)}
    <h3>{g.status} <span class="count">{g.items.length}</span></h3>
    {#each g.items as b (b.id)}
      <div class="row" class:archived={b.archived_at !== null}>
        <a class="code" href={href({ name: "batch", id: b.id })}>{b.code}</a>
        <span class="detail">{b.beverage_type}</span>
        <span class="detail">{recipeLabel(b)}</span>
        <span class="detail">{b.vessel_name ?? "—"}</span>
        <span class="detail">{pitchText(b)}</span>
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
    max-width: 64rem;
  }

  .bar {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  .bar .new {
    padding: 0.3rem 0.8rem;
    background: var(--accent);
    color: #fff;
    border-radius: var(--radius-sm);
    text-decoration: none;
    align-self: center;
  }

  .bar .compare,
  .empty a {
    color: var(--accent-text);
  }

  .count {
    margin-left: 0.3rem;
    color: var(--text-faint);
  }

  .bar .check {
    display: flex;
    gap: 0.4rem;
    margin: 0 0 0 auto;
  }

  h3 {
    margin: 1.25rem 0 0.25rem;
    color: var(--text-muted);
    font-weight: normal;
    font-size: 0.95rem;
    text-transform: capitalize;
  }

  .row {
    display: grid;
    grid-template-columns: 9rem 5rem 1fr 7rem 11rem 4rem;
    align-items: baseline;
    gap: 1rem;
    padding: 0.3rem 0;
    border-bottom: 1px solid var(--border);
  }

  .row.head {
    margin-top: 1rem;
    color: var(--text-faint);
    font-size: 0.85rem;
  }

  .row .code {
    color: inherit;
    font-weight: 600;
    text-decoration: none;
  }

  .row .code:hover {
    text-decoration: underline;
  }

  .row .row-actions {
    justify-content: flex-end;
    opacity: 0;
  }

  .row:hover .row-actions,
  .row .row-actions:focus-within,
  .row.archived .row-actions {
    opacity: 1;
  }

  .detail {
    color: var(--text-muted);
  }

  .row.archived .code,
  .row.archived .detail {
    color: var(--text-faint);
  }
</style>
