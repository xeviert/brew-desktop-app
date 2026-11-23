<script lang="ts">
  import { onMount, untrack } from "svelte";
  import {
    addScheduleStep,
    batchDashboard,
    batchSchedule,
    completeScheduleStep,
    deleteScheduleStep,
    moveScheduleStep,
    onSyncStatus,
    seedLagerSchedule,
    toApiError,
    undoScheduleStep,
    updateScheduleStep,
    type ApiError,
    type Config,
    type Dashboard,
    type Schedule,
    type ScheduleStep,
  } from "$lib/api";
  import { confirmAction } from "$lib/confirm";
  import { currentIndex, progress, stepStart, targetsText } from "$lib/schedule";
  import { sincePitch } from "$lib/timeline";
  import { formatTimestamp, fromDisplayTemp, toDisplayTemp } from "$lib/units";

  // Where the batch is on its schedule, at a glance: done steps dimmed, the current one
  // against its targets, the rest waiting. Ticking a step writes a `step_done` event,
  // so `onchanged` redraws the timeline and curve too.
  let {
    batchId,
    ui,
    readonly,
    revision,
    onchanged,
  }: { batchId: number; ui: Config["ui"]; readonly: boolean; revision: number; onchanged: () => void } = $props();

  let schedule = $state.raw<Schedule | null>(null);
  let card = $state.raw<Dashboard | null>(null);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);
  let editing = $state(false);
  let completing = $state<number | null>(null);
  let doneNote = $state("");

  /** The step being edited: its id, or `"new"`. */
  let draftFor = $state<number | "new" | null>(null);
  let draft = $state({ name: "", min: null as number | null, max: null as number | null, target: null as number | null, days: null as number | null, note: "" });

  const steps = $derived(schedule?.steps ?? []);
  const unit = $derived(schedule?.gravity_unit ?? "SG");
  const current = $derived(currentIndex(steps));
  const now = $derived({
    ts: card ? Date.now() / 1000 : 0,
    temp_c: card?.temp?.value ?? null,
    sg: card?.gravity?.sg ?? null,
    gravity: card?.display.gravity ?? null,
    unit,
    temp_display: ui.temp_display,
  });
  const live = $derived(
    current === null || !schedule ? null : progress(steps[current], stepStart(steps, current, schedule.pitched_at), now),
  );
  const lastDone = $derived(steps.reduce((last, s, i) => (s.done_at !== null ? i : last), -1));

  async function load() {
    try {
      const [s, c] = await Promise.all([batchSchedule(batchId), batchDashboard(batchId)]);
      schedule = s;
      card = c;
      error = null;
    } catch (e) {
      error = toApiError(e);
    }
  }

  $effect(() => {
    void revision;
    void ui.gravity_display;
    untrack(() => void load());
  });

  onMount(() =>
    onSyncStatus((s) => {
      if (s.rows_added > 0 && card?.source_ids.includes(s.source_id)) void load();
    }),
  );

  /** A write that answers with the schedule. `logged`: it touched the timeline too. */
  async function attempt(write: () => Promise<Schedule>, logged = false) {
    busy = true;
    error = null;
    try {
      schedule = await write();
      if (logged) onchanged();
      return true;
    } catch (e) {
      error = toApiError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  const round = (v: number, digits: number) => Number(v.toFixed(digits));
  const finite = (v: number | null) => (v === null || !Number.isFinite(v) ? null : v);

  function startDraft(step: ScheduleStep | null) {
    draftFor = step?.id ?? "new";
    draft = {
      name: step?.name ?? "",
      min: step?.temp_min_c == null ? null : round(toDisplayTemp(step.temp_min_c, ui.temp_display), 1),
      max: step?.temp_max_c == null ? null : round(toDisplayTemp(step.temp_max_c, ui.temp_display), 1),
      target: step?.target == null ? null : round(step.target, unit === "SG" ? 3 : 1),
      days: step?.min_hours == null ? null : round(step.min_hours / 24, 2),
      note: step?.note ?? "",
    };
  }

  async function saveDraft(event: SubmitEvent) {
    event.preventDefault();
    const min = finite(draft.min);
    const max = finite(draft.max);
    const days = finite(draft.days);
    const input = {
      name: draft.name,
      temp_min_c: min === null ? null : fromDisplayTemp(min, ui.temp_display),
      temp_max_c: max === null ? null : fromDisplayTemp(max, ui.temp_display),
      target: finite(draft.target),
      min_hours: days === null ? null : days * 24,
      note: draft.note.trim() || null,
    };
    const id = draftFor;
    const ok = await attempt(() => (id === "new" ? addScheduleStep(batchId, input) : updateScheduleStep(id as number, input)));
    if (ok) draftFor = null;
  }

  async function remove(step: ScheduleStep) {
    if (!(await confirmAction(`Delete the "${step.name}" step?`, "Delete"))) return;
    void attempt(() => deleteScheduleStep(step.id));
  }

  async function complete(step: ScheduleStep) {
    const ok = await attempt(() => completeScheduleStep(step.id, null, doneNote.trim() || null), true);
    if (ok) {
      completing = null;
      doneNote = "";
    }
  }
</script>

<section class="schedule" aria-labelledby="schedule-title">
  <nav class="bar">
    <h3 id="schedule-title">Schedule</h3>
    {#if steps.length > 0}
      <span class="where">
        {#if current === null}
          All {steps.length} steps done
        {:else}
          Step {current + 1} of {steps.length}
        {/if}
      </span>
    {/if}
    {#if !readonly && steps.length > 0}
      <button class="link" onclick={() => ((editing = !editing), (draftFor = null))}>{editing ? "Done editing" : "Edit"}</button>
    {/if}
  </nav>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}

  {#if schedule && steps.length === 0}
    <p class="muted">No schedule yet.</p>
    {#if !readonly}
      <div class="actions">
        <button disabled={busy} onclick={() => attempt(() => seedLagerSchedule(batchId))}>Use W-34/70 lager template</button>
        <button class="link" onclick={() => ((editing = true), startDraft(null))}>Add a step</button>
      </div>
    {/if}
  {/if}

  {#if steps.length > 0}
    <ol>
      {#each steps as step, i (step.id)}
        {@const state = step.done_at !== null ? "done" : i === current ? "current" : "upcoming"}
        <li class={state}>
          <span class="marker" aria-hidden="true">{state === "done" ? "✓" : i + 1}</span>
          <div class="body">
            <div class="head">
              <strong>{step.name}</strong>
              {#if state === "done" && step.done_at !== null}
                <span class="muted">
                  done {formatTimestamp(step.done_at)} · {sincePitch(step.done_at, schedule?.pitched_at ?? null)}
                </span>
              {:else}
                <span class="muted">{targetsText(step, unit, ui.temp_display)}</span>
              {/if}
            </div>

            {#if state === "current" && live}
              {#if schedule?.pitched_at === null}
                <p class="muted">Starts at the pitch.</p>
              {:else}
                <ul class="checks">
                  {#each live.checks as c (c.label)}
                    <li class:met={c.met === true} class:unmet={c.met === false}>
                      <span class="dot" aria-hidden="true"></span>
                      <span class="label">{c.label}</span>
                      <span>{c.detail}</span>
                      <span class="sr">{c.met === null ? "no reading" : c.met ? "met" : "not yet"}</span>
                    </li>
                  {/each}
                </ul>
                {#if live.ready}
                  <p class="ready" role="status">Ready for the next step</p>
                {/if}
              {/if}
              {#if step.note}<p class="note">{step.note}</p>{/if}
              {#if !readonly && schedule?.pitched_at !== null}
                {#if completing === step.id}
                  <form class="done-form" onsubmit={(e) => (e.preventDefault(), void complete(step))}>
                    <input type="text" placeholder="Note (optional)" bind:value={doneNote} aria-label="Note" />
                    <button type="submit" disabled={busy}>Mark done now</button>
                    <button type="button" class="link" onclick={() => (completing = null)}>Cancel</button>
                  </form>
                {:else}
                  <button class:go={live.ready} disabled={busy} onclick={() => ((completing = step.id), (doneNote = ""))}>
                    Mark "{step.name}" done
                  </button>
                {/if}
              {/if}
            {:else if state === "upcoming" && step.note}
              <p class="note muted">{step.note}</p>
            {/if}

            {#if editing && !readonly}
              <span class="edit-actions">
                {#if i === lastDone}
                  <button class="link" disabled={busy} onclick={() => attempt(() => undoScheduleStep(step.id), true)}>Undo done</button>
                {/if}
                <button class="link" disabled={busy || i === 0} onclick={() => attempt(() => moveScheduleStep(step.id, true))}>Up</button>
                <button class="link" disabled={busy || i === steps.length - 1} onclick={() => attempt(() => moveScheduleStep(step.id, false))}>Down</button>
                <button class="link" disabled={busy} onclick={() => startDraft(step)}>Edit</button>
                <button class="link" disabled={busy} onclick={() => remove(step)}>Delete</button>
              </span>
            {/if}
            {#if draftFor === step.id}
              {@render form()}
            {/if}
          </div>
        </li>
      {/each}
    </ol>
    {#if editing && !readonly && draftFor !== "new"}
      <button class="link" onclick={() => startDraft(null)}>Add a step</button>
    {/if}
  {/if}

  {#if draftFor === "new"}
    {@render form()}
  {/if}
</section>

{#snippet form()}
  <form class="step-form" onsubmit={saveDraft}>
    <label for="step-name">Name</label>
    <input id="step-name" type="text" required bind:value={draft.name} />
    <label for="step-min">Temp from</label>
    <span class="pair">
      <input id="step-min" type="number" step="0.5" bind:value={draft.min} placeholder="any" />
      <label for="step-max">to</label>
      <input id="step-max" type="number" step="0.5" bind:value={draft.max} placeholder="any" />
      <span>°{ui.temp_display}</span>
    </span>
    <label for="step-target">Gravity down to</label>
    <span class="pair">
      <input id="step-target" type="number" step={unit === "SG" ? 0.001 : 0.1} bind:value={draft.target} placeholder="none" />
      <span>{unit}</span>
    </span>
    <label for="step-days">At least</label>
    <span class="pair">
      <input id="step-days" type="number" step="0.5" min="0" bind:value={draft.days} placeholder="none" />
      <span>days</span>
    </span>
    <label for="step-note">Note</label>
    <input id="step-note" type="text" bind:value={draft.note} />
    <span></span>
    <span class="actions">
      <button type="submit" disabled={busy}>{draftFor === "new" ? "Add step" : "Save step"}</button>
      <button type="button" class="link" onclick={() => (draftFor = null)}>Cancel</button>
    </span>
  </form>
{/snippet}

<style>
  .schedule {
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

  .where {
    color: var(--text);
    font-weight: 600;
  }

  .muted {
    color: var(--text-faint);
  }

  ol {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  ol > li {
    display: grid;
    grid-template-columns: 1.75rem 1fr;
    gap: 0.6rem;
    position: relative;
    padding-bottom: 0.9rem;
  }

  /* The rail between markers. */
  ol > li:not(:last-child)::before {
    content: "";
    position: absolute;
    left: calc(0.875rem - 1px);
    top: 1.75rem;
    bottom: 0;
    width: 2px;
    background: var(--border-strong);
  }

  ol > li.done:not(:last-child)::before {
    background: var(--ok);
  }

  .marker {
    display: grid;
    place-items: center;
    width: 1.75rem;
    height: 1.75rem;
    border-radius: 50%;
    border: 2px solid var(--border-hover);
    color: var(--text-muted);
    font-size: 0.85em;
    font-weight: 600;
  }

  .done .marker {
    background: var(--ok);
    border-color: var(--ok);
    color: #14161a;
  }

  .current .marker {
    border-color: #4fb3ff;
    color: #4fb3ff;
    box-shadow: 0 0 0 3px #4fb3ff33;
  }

  .done .body {
    opacity: 0.6;
  }

  .current .body {
    padding: 0.5rem 0.75rem 0.75rem;
    border: 1px solid #4fb3ff66;
    border-left: 3px solid #4fb3ff;
    background: #4fb3ff0d;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.25rem 0.6rem;
    min-height: 1.75rem;
  }

  .current .head strong {
    color: #4fb3ff;
    font-size: 1.1em;
  }

  .checks {
    list-style: none;
    margin: 0.4rem 0;
    padding: 0;
  }

  .checks li {
    display: grid;
    grid-template-columns: 0.9rem 4.5rem 1fr;
    align-items: center;
    gap: 0.4rem;
    padding: 0.1rem 0;
  }

  .dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    background: var(--border-hover);
  }

  .met .dot {
    background: var(--ok);
  }

  .unmet .dot {
    background: var(--warn);
  }

  .label {
    color: var(--text-muted);
  }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }

  .ready {
    margin: 0.4rem 0;
    padding: 0.35rem 0.6rem;
    background: #7fb06926;
    border-left: 3px solid var(--ok);
    color: #b6e09c;
    font-weight: 600;
  }

  .note {
    margin: 0.3rem 0;
  }

  .edit-actions {
    display: flex;
    gap: 0.75rem;
    margin-top: 0.3rem;
  }

  .actions,
  .done-form {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-top: 0.5rem;
  }

  .step-form {
    display: grid;
    grid-template-columns: 8rem 1fr;
    align-items: center;
    gap: 0.4rem 0.5rem;
    margin: 0.5rem 0;
    padding: 0.6rem 0.75rem;
    border: 1px solid var(--border-strong);
  }

  .pair {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .pair input {
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
    padding: 0.35rem 0.9rem;
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: var(--radius-sm);
    font: inherit;
    cursor: pointer;
  }

  button.go {
    background: #3f8f3a;
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
