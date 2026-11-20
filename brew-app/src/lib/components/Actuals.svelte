<script lang="ts">
  import { untrack } from "svelte";
  import {
    addBatchIngredient,
    batchActuals,
    deleteBatchIngredient,
    listLotChoices,
    toApiError,
    updateBatchIngredient,
    type Actual,
    type Actuals,
    type ApiError,
    type LotChoice,
    type PlanLine,
    type RecipeItem,
  } from "$lib/api";
  import {
    actualForm,
    byLineStage,
    deltaText,
    hasLot,
    lotLabel,
    lotsFor,
    planSummary,
    toActualInput,
    unitsFor,
    type ActualForm,
  } from "$lib/actuals";
  import { localDateTime } from "$lib/batch";
  import { confirmAction } from "$lib/confirm";
  import { STAGES } from "$lib/recipe";
  import { formatQuantity } from "$lib/reference";
  import { formatMoney, formatTimestamp } from "$lib/units";

  // 3c's plan against what went in, and what it cost. `revision` refetches; `onchanged`
  // tells the host a write landed.
  let {
    batchId,
    pitchedAt,
    readonly,
    revision,
    onchanged,
  }: {
    batchId: number;
    pitchedAt: number | null;
    readonly: boolean;
    revision: number;
    onchanged: () => void;
  } = $props();

  let actuals = $state.raw<Actuals | null>(null);
  let lots = $state.raw<LotChoice[]>([]);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);
  // What the form is for: a new unplanned addition (`key: null`), recording a planned
  // line, or an edit. `key` places the form under its row; a `planned` line fixes the
  // stage, since changing it would unpair the addition from the plan.
  let editing = $state<{
    key: string | null;
    ingredient: string | null;
    ingredientId: number | null;
    actual: Actual | null;
    planned: RecipeItem | null;
  } | null>(null);
  let form = $state<ActualForm | null>(null);

  const groups = $derived(actuals ? byLineStage(actuals.lines) : []);
  const pickable = $derived(editing ? lotsFor(lots, editing.ingredientId) : lots);
  const units = $derived(form ? unitsFor(lots, form.lot, form.unit) : []);
  // An edited addition's lot may have been archived since; it stays selectable for it.
  const keptLot = $derived(
    editing?.actual && !pickable.some((l) => l.id === editing?.actual?.ingredient_lot_id) ? editing.actual : null,
  );
  const recorded = $derived(actuals ? actuals.lines.some((l) => l.kind !== "missing") : false);

  async function load() {
    try {
      [actuals, lots] = await Promise.all([batchActuals(batchId), listLotChoices()]);
    } catch (e) {
      error = toApiError(e);
    }
  }

  $effect(() => {
    void revision;
    untrack(() => void load());
  });

  function open(line: PlanLine | null, key: string | null = null, actual: Actual | null = null) {
    error = null;
    if (line && actual) {
      const planned = line.kind === "both" ? line.planned : null;
      editing = { key, ingredient: line.ingredient, ingredientId: actual.ingredient_id, actual, planned };
      form = actualForm({ actual }, lots);
    } else if (line && line.kind === "missing") {
      editing = {
        key,
        ingredient: line.ingredient,
        ingredientId: line.planned.ingredient_id,
        actual: null,
        planned: line.planned,
      };
      form = actualForm({ planned: line.planned }, lots, pitchedAt);
    } else {
      editing = { key: null, ingredient: null, ingredientId: null, actual: null, planned: null };
      form = actualForm(null, lots);
    }
  }

  function close() {
    editing = null;
    form = null;
    error = null;
  }

  /** A new lot of another dimension resets the unit to that ingredient's default. */
  function pickLot() {
    if (!form) return;
    const picked = lots.find((l) => l.id === form?.lot);
    if (picked && !unitsFor(lots, picked.id, form.unit).includes(form.unit)) form.unit = picked.default_unit;
  }

  async function save() {
    if (!form || !editing) return;
    const input = toActualInput(form);
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    const target = editing.actual;
    busy = true;
    error = null;
    try {
      await (target ? updateBatchIngredient(target.id, input.value) : addBatchIngredient(batchId, input.value));
      close();
      onchanged();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  async function remove(a: Actual) {
    if (!(await confirmAction(`Delete the ${formatQuantity(a.entry)} of ${a.ingredient}?`, "Delete"))) return;
    busy = true;
    error = null;
    try {
      await deleteBatchIngredient(a.id);
      onchanged();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  function submit(e: SubmitEvent) {
    e.preventDefault();
    void save();
  }

  function escape(e: KeyboardEvent) {
    if (editing && e.key === "Escape") close();
  }

  const focus = (el: HTMLElement) => el.focus();

  const planned = (line: PlanLine) => (line.kind === "unplanned" ? "" : formatQuantity(line.planned.entry));
  const plannedNotes = (line: PlanLine) => (line.kind === "unplanned" ? null : line.planned.notes);
  const actualOf = (line: PlanLine): Actual | null => (line.kind === "missing" ? null : line.actual);
</script>

{#snippet entry()}
  {#if form && editing}
    <form class="entry" onsubmit={submit}>
      <h4>
        {#if editing.actual}Edit {editing.ingredient}{:else if editing.planned}Record {editing.ingredient}{:else}Add an
          addition{/if}
        {#if editing.planned}<small>planned {planSummary(editing.planned)}</small>{/if}
      </h4>
      <div class="fields">
        <label>
          <span>Lot</span>
          {#if pickable.length === 1 && !keptLot}
            <output>{lotLabel(pickable[0])}</output>
          {:else}
            <select bind:value={form.lot} onchange={pickLot}>
              {#each pickable as l (l.id)}
                <option value={l.id}>{lotLabel(l)}</option>
              {/each}
              {#if keptLot}
                <option value={keptLot.ingredient_lot_id}>{keptLot.ingredient} — {keptLot.lot_label} (archived)</option>
              {/if}
            </select>
          {/if}
        </label>
        <label>
          <span>Quantity</span>
          <span class="inline">
            <input type="text" inputmode="decimal" bind:value={form.qty} {@attach focus} />
            <select bind:value={form.unit}>
              {#each units as u (u)}
                <option value={u}>{u}</option>
              {/each}
            </select>
          </span>
        </label>
        <label>
          <span>When</span>
          <span class="inline">
            <input type="datetime-local" bind:value={form.at} />
            <button
              type="button"
              class="link"
              onclick={() => {
                if (form) form.at = localDateTime(Date.now() / 1000);
              }}>Now</button
            >
          </span>
          <small>Leave blank if it was part of the stage.</small>
        </label>
        {#if !editing.planned}
          <label>
            <span>Stage</span>
            <select bind:value={form.stage}>
              {#each STAGES as s (s)}
                <option value={s}>{s}</option>
              {/each}
            </select>
          </label>
        {/if}
        <label class="wide">
          <span>Notes</span>
          <input type="text" bind:value={form.notes} />
        </label>
      </div>
      {#if error}
        <p role="alert">{error.message}</p>
      {/if}
      <div class="actions">
        <button type="submit" disabled={busy}>Save</button>
        <button type="button" class="link" onclick={close}>Cancel</button>
      </div>
    </form>
  {/if}
{/snippet}

<svelte:window onkeydown={escape} />

<section class="ref actuals">
  <nav class="bar">
    <h3>Ingredients</h3>
    {#if !readonly && !editing}
      <button class="link" onclick={() => open(null)} disabled={lots.length === 0}>Add an addition</button>
      {#if lots.length === 0}<small>No active lots: add one under Ingredients.</small>{/if}
    {/if}
  </nav>

  {#if editing && editing.key === null}
    {@render entry()}
  {/if}

  {#if actuals && actuals.lines.length === 0}
    <p class="empty">
      {actuals.recipe_version_id === null ? "No recipe and nothing added yet." : "The recipe has no lines."}
    </p>
  {/if}

  {#if groups.length > 0}
    <table>
      <thead>
        <tr><th>Ingredient</th><th>Planned</th><th>Actual</th><th>vs plan</th><th>Cost</th><th></th></tr>
      </thead>
      <tbody>
        {#each groups as g (g.stage)}
          <tr class="stage"><th colspan="6">{g.stage}</th></tr>
          {#each g.items as line, i (`${g.stage}-${i}`)}
            {@const key = `${g.stage}-${i}`}
            {@const a = actualOf(line)}
            {@const delta = deltaText(line)}
            {@const notes = plannedNotes(line)}
            <tr class={line.kind} class:current={editing?.key === key}>
              <td>
                {line.ingredient}
                {#if notes}<small>{notes}</small>{/if}
              </td>
              <td>{planned(line)}</td>
              <td>
                {#if a}
                  {formatQuantity(a.entry)}
                  <small
                    >{a.lot_label}{a.lot_archived ? " (archived)" : ""}{a.added_at !== null
                      ? ` · ${formatTimestamp(a.added_at)}`
                      : ""}</small
                  >
                {:else}
                  <span class="muted">not recorded</span>
                {/if}
              </td>
              <td>
                {#if line.kind === "unplanned"}<span class="flag">unplanned</span>{:else if delta}<span
                    class:off={delta !== "on plan"}>{delta}</span
                  >{/if}
              </td>
              <td>
                {#if a?.line_cost}{formatMoney(a.line_cost)}{:else if a}<span class="muted">unpriced</span
                  >{:else}<span class="muted">—</span>{/if}
              </td>
              <td>
                {#if !readonly && !editing}
                  <span class="row-actions">
                    {#if a}
                      <button class="link" disabled={busy} onclick={() => open(line, key, a)}>Edit</button>
                      <button class="link" disabled={busy} onclick={() => remove(a)}>Delete</button>
                    {:else if line.kind === "missing" && !hasLot(lots, line.planned.ingredient_id)}
                      <small title="Add a lot of it under Ingredients to record it">No lot yet</small>
                    {:else}
                      <button class="link" disabled={busy} onclick={() => open(line, key)}>Record</button>
                    {/if}
                  </span>
                {/if}
              </td>
            </tr>
            {#if editing?.key === key}
              <tr class="form-row"><td colspan="6">{@render entry()}</td></tr>
            {/if}
          {/each}
        {/each}
      </tbody>
    </table>
  {/if}

  {#if error && !editing}
    <p role="alert">{error.message}</p>
  {/if}

  {#if actuals}
    <p class="cost">
      {#if actuals.cost_error}
        <span class="flag">{actuals.cost_error.message}</span>
      {:else if actuals.cost?.total}
        Cost <strong>{formatMoney(actuals.cost.total)}</strong>
        {#if actuals.cost.per_litre && actuals.cost.volume_l !== null}
          · {formatMoney(actuals.cost.per_litre)}/L over {actuals.cost.volume_l} L
        {/if}
        {#if actuals.cost.lines_without_cost > 0}
          · <span class="flag">{actuals.cost.lines_without_cost} unpriced</span>
        {/if}
      {:else if actuals.cost && actuals.cost.lines_without_cost > 0}
        <span class="muted">No cost: none of the lots used is priced. Set a lot's cost under Ingredients.</span>
      {:else if !recorded && actuals.lines.length > 0}
        <span class="muted">Nothing recorded yet.</span>
      {/if}
    </p>
  {/if}
</section>

<style>
  .actuals {
    max-width: 52rem;
    margin-top: 1.5rem;
  }

  .bar {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  h3 {
    margin: 0 0 0.5rem;
  }

  h4 {
    margin: 0 0 0.75rem;
  }

  h4 small {
    margin-left: 0.5rem;
    font-weight: normal;
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }

  th,
  td {
    text-align: left;
    padding: 0.25rem 0.5rem 0.25rem 0;
    border-bottom: 1px solid #2a2e36;
    vertical-align: baseline;
  }

  thead th {
    color: #9aa0a6;
    font-weight: normal;
  }

  tr.stage th {
    padding-top: 0.75rem;
    color: #9aa0a6;
    font-weight: normal;
    text-transform: capitalize;
  }

  tr.current td {
    border-bottom: none;
  }

  tr.form-row > td {
    padding: 0 0 0.5rem;
  }

  small,
  .muted,
  .empty {
    color: #6f757d;
  }

  td small {
    display: block;
  }

  td:nth-child(n + 2) {
    white-space: nowrap;
  }

  .flag,
  .off {
    color: #e0a35f;
  }

  .entry {
    padding: 0.75rem;
    margin: 0.25rem 0 0.75rem;
    background: #1e2127;
    border-left: 3px solid #2d5d8a;
  }

  .fields {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.5rem 1.5rem;
  }

  .fields label {
    grid-template-columns: 1fr;
    gap: 0.25rem;
    margin: 0;
    max-width: none;
  }

  .fields label small {
    grid-column: 1;
  }

  .fields .wide {
    grid-column: 1 / -1;
  }

  .fields output {
    padding: 0.35rem 0;
  }

  .entry .actions {
    margin-bottom: 0;
  }

  .inline {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: inherit;
  }

  .inline input[inputmode="decimal"] {
    width: 7rem;
  }

  .cost {
    margin-top: 0.5rem;
  }
</style>
