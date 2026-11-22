<script lang="ts">
  import { addEvent, toApiError, updateEvent, type ApiError, type BatchEvent } from "$lib/api";
  import { EVENT_KINDS, eventForm, toEventInput, type EventForm } from "$lib/timeline";

  // Something done to the batch. `event` is the one being corrected, or `null` for a new one.
  let {
    batchId,
    event,
    onsaved,
    oncancel,
  }: { batchId: number; event: BatchEvent | null; onsaved: () => void; oncancel: () => void } = $props();

  // svelte-ignore state_referenced_locally -- a seed; the editor is keyed on the event.
  let form = $state<EventForm>(eventForm(event, Date.now() / 1000));
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  async function save() {
    const input = toEventInput(form);
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    busy = true;
    error = null;
    try {
      await (event ? updateEvent(event.id, input.value) : addEvent(batchId, input.value));
      onsaved();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="ref entry">
  <h4>{event ? "Edit event" : "Add an event"}</h4>
  <label>
    <span>When</span>
    <input type="datetime-local" bind:value={form.at} />
  </label>
  <label>
    <span>What</span>
    <select bind:value={form.kind}>
      {#each EVENT_KINDS as k (k.value)}
        <option value={k.value}>{k.label}</option>
      {/each}
    </select>
  </label>
  <label>
    <span>Note</span>
    <input type="text" bind:value={form.note} placeholder="2 g DAP" />
  </label>
  <div class="actions">
    <button onclick={save} disabled={busy}>Save</button>
    <button class="link" onclick={oncancel}>Cancel</button>
  </div>
  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</div>

<style>
  .entry {
    padding: 0.5rem 0.75rem;
    margin-bottom: 0.75rem;
    background: #1e2127;
    border-left: 3px solid #2d5d8a;
  }

  h4 {
    margin: 0 0 0.5rem;
  }
</style>
