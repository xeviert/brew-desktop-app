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
  } from "$lib/api";
  import {
    actualForm,
    byLineStage,
    deltaText,
    lotLabel,
    lotsFor,
    toActualInput,
    unitsFor,
    type ActualForm,
  } from "$lib/actuals";
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
  // What the form is for: a new unplanned addition, recording a planned line, or an edit.
  let editing = $state<{ ingredientId: number | null; actual: Actual | null } | null>(null);
  let form = $state<ActualForm | null>(null);

  const groups = $derived(actuals ? byLineStage(actuals.lines) : []);
  const pickable = $derived(editing ? lotsFor(lots, editing.ingredientId) : lots);
  const units = $derived(form ? unitsFor(lots, form.lot, form.unit) : []);
  // An edited addition's lot may have been archived since; it stays selectable for it.
  const keptLot = $derived(
    editing?.actual && !pickable.some((l) => l.id === editing?.actual?.ingredient_lot_id) ? editing.actual : null,
  );

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

  function open(line: PlanLine | null, actual: Actual | null = null) {
    error = null;
    if (actual) {
      editing = { ingredientId: actual.ingredient_id, actual };
      form = actualForm({ actual }, lots);
    } else if (line && line.kind === "missing") {
      editing = { ingredientId: line.planned.ingredient_id, actual: null };
      form = actualForm({ planned: line.planned }, lots, pitchedAt);
    } else {
      editing = { ingredientId: null, actual: null };
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

  const planned = (line: PlanLine) => (line.kind === "unplanned" ? "" : formatQuantity(line.planned.entry));
  const actualOf = (line: PlanLine): Actual | null => (line.kind === "missing" ? null : line.actual);
</script>

<section class="ref actuals">
  <nav class="bar">
    <h3>Ingredients</h3>
    {#if !readonly && !editing}
      <button class="link" onclick={() => open(null)} disabled={lots.length === 0}>Add an addition</button>
      {#if lots.length === 0}<small>No active lots: add one under Ingredients.</small>{/if}
    {/if}
  </nav>

  {#if actuals && actuals.lines.length === 0}
    <p class="empty">
      {actuals.recipe_version_id === null ? "No recipe and nothing added yet." : "The recipe has no lines."}
    </p>
  {/if}

  {#if groups.length > 0}
    <table>
      <thead>
        <tr><th>Ingredient</th><th>Planned</th><th>Actual</th><th></th><th>Cost</th><th></th></tr>
      </thead>
      <tbody>
        {#each groups as g (g.stage)}
          <tr class="stage"><th colspan="6">{g.stage}</th></tr>
          {#each g.items as line, i (`${g.stage}-${i}`)}
            {@const a = actualOf(line)}
            {@const delta = deltaText(line)}
            <tr class={line.kind}>
              <td>{line.ingredient}</td>
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
                  <span class="flag">missing</span>
                {/if}
              </td>
              <td>
                {#if line.kind === "unplanned"}<span class="flag">unplanned</span>{:else if delta}<span
                    class:off={delta !== "on plan"}>{delta}</span
                  >{/if}
              </td>
              <td>{a?.line_cost ? formatMoney(a.line_cost) : a ? "—" : ""}</td>
              <td class="row-actions">
                {#if !readonly && !editing}
                  {#if a}
                    <button class="link" disabled={busy} onclick={() => open(line, a)}>Edit</button>
                    <button class="link" disabled={busy} onclick={() => remove(a)}>Delete</button>
                  {:else}
                    <button class="link" disabled={busy} onclick={() => open(line)}>Record</button>
                  {/if}
                {/if}
              </td>
            </tr>
          {/each}
        {/each}
      </tbody>
    </table>
  {/if}

  {#if form && editing}
    <div class="entry">
      <h4>{editing.actual ? "Edit addition" : editing.ingredientId !== null ? "Record planned addition" : "Add an addition"}</h4>
      <label>
        <span>Lot</span>
        <select bind:value={form.lot} onchange={pickLot}>
          {#each pickable as l (l.id)}
            <option value={l.id}>{lotLabel(l)}</option>
          {/each}
          {#if keptLot}
            <option value={keptLot.ingredient_lot_id}>{keptLot.ingredient} — {keptLot.lot_label} (archived)</option>
          {/if}
        </select>
        {#if pickable.length === 0 && !keptLot}<small>No active lot of this ingredient: add one under Ingredients.</small>{/if}
      </label>
      <label>
        <span>Quantity</span>
        <span class="inline">
          <input type="text" inputmode="decimal" bind:value={form.qty} />
          <select bind:value={form.unit}>
            {#each units as u (u)}
              <option value={u}>{u}</option>
            {/each}
          </select>
        </span>
      </label>
      <label>
        <span>Stage</span>
        <select bind:value={form.stage}>
          {#each STAGES as s (s)}
            <option value={s}>{s}</option>
          {/each}
        </select>
      </label>
      <label>
        <span>Added at</span>
        <input type="datetime-local" bind:value={form.at} />
        <small>Blank if not timed.</small>
      </label>
      <label>
        <span>Notes</span>
        <input type="text" bind:value={form.notes} />
      </label>
      <div class="actions">
        <button onclick={save} disabled={busy}>Save</button>
        <button class="link" onclick={close}>Cancel</button>
      </div>
    </div>
  {/if}

  {#if error}
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
        <span class="muted">No cost: none of the lots used is priced.</span>
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
    margin: 0 0 0.5rem;
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

  small,
  .muted,
  .empty {
    color: #6f757d;
  }

  td small {
    display: block;
  }

  .flag,
  .off {
    color: #e0a35f;
  }

  .entry {
    padding: 0.5rem 0.75rem;
    margin: 0.75rem 0;
    background: #1e2127;
    border-left: 3px solid #2d5d8a;
  }

  .inline {
    display: flex;
    gap: 0.5rem;
    color: inherit;
  }

  .inline input {
    width: 7rem;
  }

  .cost {
    margin-top: 0.5rem;
  }
</style>
