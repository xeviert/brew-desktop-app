<script lang="ts">
  import { onMount } from "svelte";
  import {
    addCleaning,
    deleteCleaning,
    toApiError,
    updateCleaning,
    vesselCleanings,
    type ApiError,
    type Cleaning,
    type CleaningKind,
    type CleaningLog,
  } from "$lib/api";
  import { fromLocalDateTime, localDateTime } from "$lib/batch";
  import { CLEANING_KINDS, cleaningLabel } from "$lib/cleaning";
  import { confirmAction } from "$lib/confirm";
  import { formatTimestamp } from "$lib/units";

  // One vessel's cleaning log, newest first. `onchanged` refreshes the pane's hygiene line.
  let { vesselId, readonly, onchanged }: { vesselId: number; readonly: boolean; onchanged: () => void } = $props();

  let log = $state.raw<CleaningLog | null>(null);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);
  /** The entry being edited: its id, or `"new"`. */
  let draftFor = $state<number | "new" | null>(null);
  let draft = $state({ kind: "clean_and_sanitize" as CleaningKind, chemical: "", notes: "", at: "" });

  onMount(() => {
    void load();
  });

  async function load() {
    try {
      log = await vesselCleanings(vesselId);
      error = null;
    } catch (e) {
      error = toApiError(e);
    }
  }

  function startDraft(entry: Cleaning | null) {
    draftFor = entry?.id ?? "new";
    draft = {
      kind: entry?.kind ?? "clean_and_sanitize",
      // The last chemical used is the likeliest next one.
      chemical: entry?.chemical ?? log?.entries[0]?.chemical ?? "",
      notes: entry?.notes ?? "",
      at: localDateTime(entry?.cleaned_at ?? Date.now() / 1000),
    };
    error = null;
  }

  async function attempt(write: () => Promise<CleaningLog>) {
    busy = true;
    error = null;
    try {
      log = await write();
      onchanged();
      return true;
    } catch (e) {
      error = toApiError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    const at = fromLocalDateTime(draft.at);
    if (at === null) {
      error = { code: "form", message: "Not readable: when" };
      return;
    }
    const input = {
      kind: draft.kind,
      chemical: draft.chemical.trim() === "" ? null : draft.chemical.trim(),
      notes: draft.notes.trim() === "" ? null : draft.notes.trim(),
    };
    const id = draftFor;
    const ok = await attempt(() =>
      id === "new" ? addCleaning(vesselId, at, input) : updateCleaning(id as number, at, input),
    );
    if (ok) draftFor = null;
  }

  async function remove(entry: Cleaning) {
    const when = formatTimestamp(entry.cleaned_at);
    if (!(await confirmAction(`Delete the ${cleaningLabel(entry.kind).toLowerCase()} entry of ${when}?`, "Delete")))
      return;
    void attempt(() => deleteCleaning(entry.id));
  }
</script>

<div class="cleaning">
  {#if !readonly && draftFor === null}
    <button class="link" onclick={() => startDraft(null)}>Log a clean</button>
  {/if}
  {#if draftFor === "new"}
    {@render form()}
  {/if}

  {#if log && log.entries.length === 0 && draftFor !== "new"}
    <p class="empty">Nothing logged.</p>
  {/if}

  {#if log && log.entries.length > 0}
    <ul>
      {#each log.entries as entry (entry.id)}
        <li>
          <span class="when">{formatTimestamp(entry.cleaned_at)}</span>
          <span>
            {cleaningLabel(entry.kind)}{entry.chemical ? ` · ${entry.chemical}` : ""}
            {#if entry.notes}<small>{entry.notes}</small>{/if}
          </span>
          {#if !readonly && draftFor === null}
            <span class="row-actions">
              <button class="link" disabled={busy} onclick={() => startDraft(entry)}>Edit</button>
              <button class="link" disabled={busy} onclick={() => remove(entry)}>Delete</button>
            </span>
          {/if}
          {#if draftFor === entry.id}
            {@render form()}
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if error && draftFor === null}
    <p role="alert">{error.message}</p>
  {/if}
</div>

{#snippet form()}
  <form class="entry" onsubmit={save}>
    <label>
      <span>What</span>
      <select bind:value={draft.kind}>
        {#each CLEANING_KINDS as k (k.value)}
          <option value={k.value}>{k.label}</option>
        {/each}
      </select>
    </label>
    <label>
      <span>With</span>
      <input type="text" bind:value={draft.chemical} placeholder="Star San 1.5 mL/L" />
    </label>
    <label>
      <span>When</span>
      <input type="datetime-local" bind:value={draft.at} />
    </label>
    <label>
      <span>Notes</span>
      <input type="text" bind:value={draft.notes} />
    </label>
    {#if error}
      <p role="alert">{error.message}</p>
    {/if}
    <div class="actions">
      <button type="submit" disabled={busy}>{draftFor === "new" ? "Log it" : "Save"}</button>
      <button type="button" class="link" onclick={() => ((draftFor = null), (error = null))}>Cancel</button>
    </div>
  </form>
{/snippet}

<style>
  .cleaning {
    margin: 0.25rem 0 0.5rem;
  }

  ul {
    list-style: none;
    margin: 0.25rem 0 0;
    padding: 0;
  }

  li {
    display: grid;
    grid-template-columns: 11rem 1fr auto;
    gap: 0.25rem 0.75rem;
    align-items: baseline;
    padding: 0.2rem 0;
  }

  .when,
  small,
  .empty {
    color: #6f757d;
  }

  small {
    display: block;
  }

  .row-actions,
  .actions {
    display: flex;
    gap: 0.75rem;
    align-items: center;
  }

  .entry {
    grid-column: 1 / -1;
    padding: 0.6rem 0.75rem;
    margin: 0.4rem 0;
    background: #1e2127;
    border-left: 3px solid #2d5d8a;
  }

  .entry label {
    grid-template-columns: 5rem 1fr;
    margin-bottom: 0.4rem;
  }

  .actions {
    margin-top: 0.5rem;
  }

  button.link {
    padding: 0;
    background: none;
    color: #7ba7d4;
  }

  button.link:disabled {
    color: #4a5160;
  }
</style>
