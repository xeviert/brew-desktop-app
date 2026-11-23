<script lang="ts">
  import { untrack } from "svelte";
  import {
    addReminder,
    batchReminders,
    completeReminder,
    deleteReminder,
    toApiError,
    undoReminder,
    updateReminder,
    type ApiError,
    type Reminder,
    type Reminders,
  } from "$lib/api";
  import { confirmAction } from "$lib/confirm";
  import { reminderDay, reminderState } from "$lib/schedule";
  import { formatTimestamp } from "$lib/units";

  // Calendar checks counted from the pitch ("taste on day 10"). `brewd` raises one as
  // due when its time comes; ticking it writes a `reminder_done` event, so `onchanged`
  // redraws the timeline and curve, and the alert clears.
  let {
    batchId,
    graceHours,
    readonly,
    revision,
    onchanged,
  }: { batchId: number; graceHours: number; readonly: boolean; revision: number; onchanged: () => void } = $props();

  let data = $state.raw<Reminders | null>(null);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);
  let editing = $state(false);
  /** The reminder being edited: its id, or `"new"`. */
  let draftFor = $state<number | "new" | null>(null);
  let draft = $state({ day: null as number | null, text: "" });

  const reminders = $derived(data?.reminders ?? []);
  const now = $derived(data ? Date.now() / 1000 : 0);

  $effect(() => {
    void revision;
    untrack(() => void load());
  });

  async function load() {
    try {
      data = await batchReminders(batchId);
      error = null;
    } catch (e) {
      error = toApiError(e);
    }
  }

  /** A write that answers with the reminders. `logged`: it touched the timeline too. */
  async function attempt(write: () => Promise<Reminders>, logged = false) {
    busy = true;
    error = null;
    try {
      data = await write();
      if (logged) onchanged();
      return true;
    } catch (e) {
      error = toApiError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  function startDraft(r: Reminder | null) {
    draftFor = r?.id ?? "new";
    draft = { day: r ? Number((r.after_hours / 24).toFixed(2)) : null, text: r?.text ?? "" };
  }

  async function saveDraft(event: SubmitEvent) {
    event.preventDefault();
    // `required` keeps an empty day from submitting; this narrows the type.
    if (draft.day === null) return;
    const input = { after_hours: draft.day * 24, text: draft.text };
    const id = draftFor;
    const ok = await attempt(() => (id === "new" ? addReminder(batchId, input) : updateReminder(id as number, input)));
    if (ok) draftFor = null;
  }

  async function remove(r: Reminder) {
    if (!(await confirmAction(`Delete the "${r.text}" reminder?`, "Delete"))) return;
    void attempt(() => deleteReminder(r.id));
  }
</script>

<section class="reminders" aria-labelledby="reminders-title">
  <nav class="bar">
    <h3 id="reminders-title">Reminders</h3>
    {#if !readonly && reminders.length > 0}
      <button class="link" onclick={() => ((editing = !editing), (draftFor = null))}>{editing ? "Done editing" : "Edit"}</button>
    {/if}
  </nav>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}

  {#if data && reminders.length === 0}
    <p class="muted">
      None. A reminder is a check on the calendar, counted from the pitch: taste on day 10.
      {#if !readonly}<button class="link" onclick={() => startDraft(null)}>Add one</button>{/if}
    </p>
  {/if}

  {#if reminders.length > 0}
    <ul>
      {#each reminders as r (r.id)}
        {@const s = reminderState(r, data?.pitched_at ?? null, now, graceHours)}
        <li class={s.state}>
          <span class="day">{reminderDay(r)}</span>
          <span class="text">{r.text}</span>
          <span class="when muted">
            {#if s.state === "done" && r.done_at !== null}
              done {formatTimestamp(r.done_at)}
            {:else if s.due !== null}
              {s.state === "upcoming" ? "" : `${s.state} · `}{formatTimestamp(s.due)}
            {:else}
              from the pitch
            {/if}
          </span>
          {#if !readonly}
            <span class="actions">
              {#if s.state === "due" || s.state === "overdue"}
                <button disabled={busy} onclick={() => attempt(() => completeReminder(r.id, null, null), true)}>Done</button>
              {:else if s.state === "upcoming" && s.due !== null && !editing}
                <button class="link" disabled={busy} onclick={() => attempt(() => completeReminder(r.id, null, null), true)}>Done early</button>
              {/if}
              {#if editing}
                {#if s.state === "done"}
                  <button class="link" disabled={busy} onclick={() => attempt(() => undoReminder(r.id), true)}>Undo done</button>
                {/if}
                <button class="link" disabled={busy} onclick={() => startDraft(r)}>Edit</button>
                <button class="link" disabled={busy} onclick={() => remove(r)}>Delete</button>
              {/if}
            </span>
          {/if}
          {#if draftFor === r.id}
            {@render form()}
          {/if}
        </li>
      {/each}
    </ul>
    {#if editing && !readonly && draftFor !== "new"}
      <button class="link" onclick={() => startDraft(null)}>Add a reminder</button>
    {/if}
  {/if}

  {#if draftFor === "new"}
    {@render form()}
  {/if}
</section>

{#snippet form()}
  <form class="reminder-form" onsubmit={saveDraft}>
    <label for="reminder-day">On day</label>
    <input id="reminder-day" type="number" min="0" step="any" required bind:value={draft.day} />
    <label for="reminder-text">To do</label>
    <input id="reminder-text" type="text" required bind:value={draft.text} placeholder="Taste" />
    <span></span>
    <span class="form-actions">
      <button type="submit" disabled={busy}>{draftFor === "new" ? "Add reminder" : "Save reminder"}</button>
      <button type="button" class="link" onclick={() => (draftFor = null)}>Cancel</button>
    </span>
  </form>
{/snippet}

<style>
  .reminders {
    margin-bottom: 1.5rem;
  }

  .bar {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  h3 {
    margin: 0 0 0.5rem;
  }

  .muted {
    color: var(--text-faint);
  }

  ul {
    list-style: none;
    margin: 0 0 0.5rem;
    padding: 0;
  }

  li {
    display: grid;
    grid-template-columns: 4.5rem 1fr auto auto;
    align-items: baseline;
    gap: 0.25rem 0.75rem;
    padding: 0.3rem 0.5rem;
    border-left: 3px solid transparent;
  }

  li.done {
    opacity: 0.6;
  }

  li.due {
    border-left-color: #4fb3ff;
    background: #4fb3ff0d;
  }

  li.overdue {
    border-left-color: var(--warn);
    background: #e0a35f14;
  }

  .day {
    color: var(--text-muted);
  }

  .overdue .when {
    color: var(--warn);
  }

  .actions,
  .form-actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .reminder-form {
    grid-column: 1 / -1;
    display: grid;
    grid-template-columns: 8rem 1fr;
    align-items: center;
    gap: 0.4rem 0.5rem;
    margin: 0.5rem 0;
    padding: 0.6rem 0.75rem;
    border: 1px solid var(--border-strong);
  }

  #reminder-day {
    width: 6rem;
  }

  input {
    padding: 0.3rem 0.5rem;
    background: var(--surface-2);
    color: inherit;
    border: 1px solid var(--border-hover);
    border-radius: var(--radius-sm);
    font: inherit;
  }

  button {
    padding: 0.2rem 0.8rem;
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: var(--radius-sm);
    font: inherit;
    cursor: pointer;
  }

  button:disabled {
    background: var(--border);
    color: var(--text-faint);
  }

  button.link {
    padding: 0;
    background: none;
    color: var(--accent-text);
  }

  button.link:disabled {
    color: var(--border-hover);
    background: none;
  }
</style>
