<script lang="ts">
  import {
    packageBatch,
    recordPitch,
    toApiError,
    transitionBatch,
    undoPitch,
    vesselHygiene,
    type ApiError,
    type BatchSummary,
    type PackagingInput,
  } from "$lib/api";
  import { NEXT, fromLocalDateTime, localDateTime } from "$lib/batch";
  import { pitchWarning } from "$lib/cleaning";
  import { confirmAction } from "$lib/confirm";
  import PackagingFields from "$lib/components/PackagingFields.svelte";
  import { packagingForm, toPackagingInput, type PackagingForm } from "$lib/packaging";
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
  // What was packaged, recorded with the move. None is the move alone.
  let packLines = $state<PackagingForm[]>([]);

  const next = $derived(NEXT[batch.status]);

  // 7b: a vessel not sanitised since its last batch. A warning; the pitch still goes in.
  let hygieneWarning = $state<string | null>(null);
  $effect(() => {
    const vesselId = batch.status === "planned" ? batch.vessel_id : null;
    const name = batch.vessel_name ?? "The vessel";
    hygieneWarning = null;
    if (vesselId === null) return;
    vesselHygiene(vesselId)
      .then((h) => {
        if (batch.vessel_id === vesselId) hygieneWarning = pitchWarning(h, name);
      })
      .catch(() => {});
  });

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
    if (next.to !== "packaged") {
      void attempt(() => transitionBatch(batch.id, next.to, null));
      return;
    }
    const t = at(packAt, "packaging time");
    if (t === undefined) return;
    const lines: PackagingInput[] = [];
    for (const form of packLines) {
      const line = toPackagingInput(form);
      if (!line.ok) {
        error = { code: "form", message: line.error };
        return;
      }
      lines.push(line.value);
    }
    void attempt(() => packageBatch(batch.id, t, lines));
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
      {#if hygieneWarning}
        <p class="warn">{hygieneWarning} Log it under Settings → Vessels.</p>
      {/if}
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
        {#each packLines as _, i (i)}
          <div class="pack-line">
            <PackagingFields bind:form={packLines[i]} />
            <button class="link" onclick={() => packLines.splice(i, 1)}>Remove</button>
          </div>
        {/each}
        <button class="link" onclick={() => packLines.push(packagingForm(null))}>
          {packLines.length === 0 ? "Record what was packaged" : "Another line"}
        </button>
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
    color: var(--text-faint);
  }

  .warn {
    color: var(--warn);
  }

  .pack-line {
    padding: 0.6rem 0.75rem;
    margin-bottom: 0.5rem;
    background: var(--surface-2);
    border-left: 3px solid var(--accent);
  }

  .pack-line > button {
    margin-top: 0.4rem;
  }
</style>
