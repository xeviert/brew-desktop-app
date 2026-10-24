<script lang="ts">
  import {
    archiveBatch,
    exportBatchReadings,
    listVessels,
    restoreBatch,
    toApiError,
    updateBatch,
    type ApiError,
    type BatchSummary,
    type Vessel,
  } from "$lib/api";
  import { editForm, headerFacts, recipeLabel, toBatchEdit, type EditForm } from "$lib/batch";
  import { confirmAction } from "$lib/confirm";
  import { csvName, saveCsv } from "$lib/export";
  import { href } from "$lib/route";

  // What the batch is and the writes that are about the batch itself: edit, archive,
  // restore. The status moves are `BatchLifecycle`'s.
  let { batch, onchanged }: { batch: BatchSummary; onchanged: () => Promise<void> } = $props();

  let busy = $state(false);
  let error = $state<ApiError | null>(null);
  let editing = $state<EditForm | null>(null);
  let vessels = $state<Vessel[]>([]);

  const archived = $derived(batch.archived_at !== null);
  const facts = $derived(headerFacts(batch));

  async function attempt(write: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await write();
      editing = null;
      await onchanged();
    } catch (e) {
      error = toApiError(e);
      if (error.code === "db.batch_archived") await onchanged();
    } finally {
      busy = false;
    }
  }

  async function setArchived(archive: boolean) {
    if (archive && (batch.status === "fermenting" || batch.status === "conditioning")) {
      const ok = await confirmAction(
        `Archive ${batch.code} while it is ${batch.status}? Its vessel's sensors stop recording to it now.`,
        "Archive",
      );
      if (!ok) return;
    }
    void attempt(() => (archive ? archiveBatch(batch.id) : restoreBatch(batch.id)));
  }

  async function edit() {
    error = null;
    try {
      vessels = await listVessels();
      editing = editForm(batch);
    } catch (e) {
      error = toApiError(e);
    }
  }

  function save() {
    if (!editing) return;
    const input = toBatchEdit(editing);
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    const value = input.value;
    void attempt(() => updateBatch(batch.id, value));
  }

  function cancel() {
    editing = null;
    error = null;
  }

  /** Every reading bound to the batch, not the curve's zoom. */
  async function exportReadings() {
    const { id, code } = batch;
    try {
      await saveCsv(csvName([code, "readings"]), (path) => exportBatchReadings(id, path));
      error = null;
    } catch (e) {
      error = toApiError(e);
    }
  }
</script>

<header class="ref batch-header">
  <a class="back" href={href({ name: "batches" })}>← Batches</a>
  <div class="title">
    <h2>{batch.code}</h2>
    <span class="status" class:archived>{batch.status}{archived ? " · archived" : ""}</span>
    <span class="row-actions">
      {#if !archived}
        <button class="link" onclick={edit} disabled={busy || editing !== null}>Edit</button>
        <button class="link" onclick={() => setArchived(true)} disabled={busy}>Archive</button>
      {:else}
        <button class="link" onclick={() => setArchived(false)} disabled={busy}>Restore</button>
      {/if}
      <button class="link" onclick={exportReadings}>Export readings</button>
      <a href={href({ name: "compare", ids: [batch.id] })}>Compare with…</a>
    </span>
  </div>
  <p class="detail">
    {#if batch.recipe_id !== null && batch.version_no !== null}
      <a href={href({ name: "recipe", id: batch.recipe_id, version: batch.version_no })}>{recipeLabel(batch)}</a>
    {:else}
      {recipeLabel(batch)}
    {/if}
    {#each facts as f (f)}{` · ${f}`}{/each}
  </p>
  {#if batch.notes}<p class="detail">{batch.notes}</p>{/if}

  {#if editing}
    <div class="edit">
      <label>
        <span>Code</span>
        <input type="text" bind:value={editing.code} />
      </label>
      <label>
        <span>Vessel</span>
        <select bind:value={editing.vessel_id} disabled={batch.pitched_at !== null}>
          <option value={null}>None</option>
          {#each vessels as v (v.id)}
            <option value={v.id}>{v.name}</option>
          {/each}
          {#if batch.vessel_id !== null && !vessels.some((v) => v.id === batch.vessel_id)}
            <option value={batch.vessel_id}>{batch.vessel_name}</option>
          {/if}
        </select>
        {#if batch.pitched_at !== null}<small>Fixed by the pitch: undo it to move the batch.</small>{/if}
      </label>
      <label>
        <span>Volume (L)</span>
        <input type="text" inputmode="decimal" bind:value={editing.volume} />
      </label>
      <label>
        <span>Notes</span>
        <input type="text" bind:value={editing.notes} />
      </label>
      <div class="actions">
        <button onclick={save} disabled={busy}>Save</button>
        <button class="link" onclick={cancel}>Cancel</button>
      </div>
    </div>
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</header>

<style>
  .batch-header {
    margin-bottom: 1rem;
  }

  .back {
    color: #9aa0a6;
    text-decoration: none;
  }

  .title {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  h2 {
    margin: 0.25rem 0;
  }

  .status {
    text-transform: capitalize;
    font-weight: 600;
  }

  .status.archived,
  .detail,
  small {
    color: #6f757d;
  }

  .detail {
    margin: 0.15rem 0;
  }

  .detail a,
  .row-actions a {
    color: #7ba7d4;
  }

  .row-actions {
    margin-left: auto;
    display: flex;
    gap: 1rem;
  }

  .edit {
    max-width: 32rem;
    margin-top: 0.5rem;
  }
</style>
