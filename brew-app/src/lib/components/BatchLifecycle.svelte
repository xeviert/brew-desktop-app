<script lang="ts">
  import {
    archiveBatch,
    listVessels,
    recordPitch,
    restoreBatch,
    toApiError,
    transitionBatch,
    undoPitch,
    updateBatch,
    type ApiError,
    type BatchSummary,
    type Vessel,
  } from "$lib/api";
  import { NEXT, editForm, fromLocalDateTime, localDateTime, recipeLabel, toBatchEdit, type EditForm } from "$lib/batch";
  import { confirmAction } from "$lib/confirm";
  import { href } from "$lib/route";
  import { formatTimestamp } from "$lib/units";

  // The status, the one move forward from it, and the batch's own fields. `onchanged`
  // reloads the batch; this panel holds no copy of it.
  let { batch, onchanged }: { batch: BatchSummary; onchanged: () => Promise<void> } = $props();

  // Codes that mean the batch moved under this screen: reload rather than leave a
  // button that will fail the same way.
  const STALE = new Set(["db.illegal_batch_transition", "db.batch_archived", "db.batch_already_pitched"]);

  let busy = $state(false);
  let error = $state<ApiError | null>(null);
  let editing = $state<EditForm | null>(null);
  let vessels = $state<Vessel[]>([]);
  // Both default to when the panel was opened, editable down to the minute.
  let pitchAt = $state(localDateTime(Date.now() / 1000));
  let pitchNote = $state("");
  let packAt = $state(localDateTime(Date.now() / 1000));

  const archived = $derived(batch.archived_at !== null);
  const next = $derived(NEXT[batch.status]);

  async function attempt(write: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await write();
      editing = null;
      await onchanged();
    } catch (e) {
      error = toApiError(e);
      if (STALE.has(error.code)) await onchanged();
    } finally {
      busy = false;
    }
  }

  function at(value: string, label: string): number | undefined {
    const v = fromLocalDateTime(value);
    if (v === null) error = { code: "form", message: `Not readable: ${label}` };
    return v ?? undefined;
  }

  function pitch() {
    const t = at(pitchAt, "pitch time");
    if (t === undefined) return;
    void attempt(() => recordPitch(batch.id, t, pitchNote.trim() === "" ? null : pitchNote.trim()));
  }

  async function undo() {
    const pitched = batch.pitched_at !== null ? formatTimestamp(batch.pitched_at) : "";
    const ok = await confirmAction(
      `Undo the pitch at ${pitched}? ${batch.code} goes back to planned, and the sensor bindings the pitch opened are dropped.`,
      "Undo pitch",
    );
    if (ok) void attempt(() => undoPitch(batch.id));
  }

  function advance() {
    if (!next) return;
    let t: number | null = null;
    if (next.to === "packaged") {
      const v = at(packAt, "packaging time");
      if (v === undefined) return;
      t = v;
    }
    void attempt(() => transitionBatch(batch.id, next.to, t));
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
</script>

<div class="ref lifecycle">
  <header>
    <span class="status" class:archived>{batch.status}{archived ? " · archived" : ""}</span>
    <span class="detail">
      {#if batch.recipe_id !== null && batch.version_no !== null}
        <a href={href({ name: "recipe", id: batch.recipe_id, version: batch.version_no })}>{recipeLabel(batch)}</a>
      {:else}
        {recipeLabel(batch)}
      {/if}
      {batch.vessel_name ? `· ${batch.vessel_name}` : ""}
      {batch.volume_l !== null ? `· ${batch.volume_l} L` : ""}
      {batch.pitched_at !== null ? `· pitched ${formatTimestamp(batch.pitched_at)}` : ""}
    </span>
    <span class="row-actions">
      {#if !archived}
        <button class="link" onclick={edit} disabled={busy || editing !== null}>Edit</button>
        <button class="link" onclick={() => setArchived(true)} disabled={busy}>Archive</button>
      {:else}
        <button class="link" onclick={() => setArchived(false)} disabled={busy}>Restore</button>
      {/if}
    </span>
  </header>
  {#if batch.notes}<p class="detail">{batch.notes}</p>{/if}

  {#if editing}
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
  {:else if !archived}
    {#if batch.status === "planned"}
      <label>
        <span>Pitched at</span>
        <input type="datetime-local" bind:value={pitchAt} />
      </label>
      <label>
        <span>Note</span>
        <input type="text" bind:value={pitchNote} placeholder="EC-1118, 10 g rehydrated" />
      </label>
      <div class="actions">
        <button onclick={pitch} disabled={busy}>Pitch</button>
        <small>t=0 for the curve, the finish detector and every comparison.</small>
      </div>
    {:else if next}
      {#if next.to === "packaged"}
        <label>
          <span>Packaged at</span>
          <input type="datetime-local" bind:value={packAt} />
          <small>The vessel's sensors stop recording to this batch then.</small>
        </label>
      {/if}
      <div class="actions">
        <button onclick={advance} disabled={busy}>{next.label}</button>
        {#if batch.status === "fermenting"}
          <button class="link" onclick={undo} disabled={busy}>Undo pitch</button>
        {/if}
      </div>
    {/if}
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</div>

<style>
  .lifecycle {
    max-width: 48rem;
    margin-bottom: 1rem;
  }

  header {
    display: flex;
    align-items: baseline;
    gap: 1rem;
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

  .detail a {
    color: #7ba7d4;
  }

  .row-actions {
    margin-left: auto;
  }
</style>
