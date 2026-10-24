<script lang="ts">
  import {
    recordPitch,
    toApiError,
    transitionBatch,
    undoPitch,
    type ApiError,
    type BatchSummary,
  } from "$lib/api";
  import { NEXT, fromLocalDateTime, localDateTime } from "$lib/batch";
  import { confirmAction } from "$lib/confirm";
  import { formatTimestamp } from "$lib/units";

  // The one move forward from the batch's status, and undoing the pitch. `onchanged`
  // reloads the batch; this panel holds no copy of it. Nothing to show once archived or
  // packaged.
  let { batch, onchanged }: { batch: BatchSummary; onchanged: () => Promise<void> } = $props();

  // Codes that mean the batch moved under this screen: reload rather than leave a
  // button that will fail the same way.
  const STALE = new Set(["db.illegal_batch_transition", "db.batch_archived", "db.batch_already_pitched"]);

  let busy = $state(false);
  let error = $state<ApiError | null>(null);
  // Both default to when the panel was opened, editable down to the minute.
  let pitchAt = $state(localDateTime(Date.now() / 1000));
  let pitchNote = $state("");
  let packAt = $state(localDateTime(Date.now() / 1000));

  const next = $derived(NEXT[batch.status]);

  async function attempt(write: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await write();
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
</script>

{#if batch.archived_at === null && (batch.status === "planned" || next)}
  <div class="ref lifecycle">
    {#if batch.status === "planned"}
      <h3>Pitch</h3>
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

    {#if error}
      <p role="alert">{error.message}</p>
    {/if}
  </div>
{/if}

<style>
  .lifecycle {
    margin-bottom: 1rem;
  }

  h3 {
    margin: 0 0 0.5rem;
  }

  .lifecycle label {
    grid-template-columns: 7rem 1fr;
  }

  small {
    color: #6f757d;
  }
</style>
