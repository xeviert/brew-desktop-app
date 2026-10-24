<script lang="ts">
  import {
    archiveLot,
    createLot,
    listLots,
    restoreLot,
    toApiError,
    updateLot,
    type ApiError,
    type Ingredient,
    type Lot,
  } from "$lib/api";
  import {
    formatQuantity,
    formatUnitCost,
    lotForm,
    toLotInput,
    unitsLike,
    type LotForm,
  } from "$lib/reference";
  import { href } from "$lib/route";
  import { formatMoney, formatTimestamp, type Currency } from "$lib/units";

  let {
    ingredient,
    currency,
    showArchived,
    onchanged,
  }: {
    ingredient: Ingredient;
    currency: Currency;
    showArchived: boolean;
    /** The ingredient's lot count moved. */
    onchanged: () => void;
  } = $props();

  let lots = $state<Lot[]>([]);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);
  /** The lot being edited, `"new"` for the add form, or nothing. */
  let editing = $state<number | "new" | null>(null);
  let form = $state<LotForm>(lotForm(null, "kg", "EUR"));

  const units = $derived(unitsLike(ingredient.default_unit));
  const day = { dateStyle: "medium" } as const;

  async function load() {
    try {
      lots = await listLots(ingredient.id, showArchived);
    } catch (e) {
      error = toApiError(e);
    }
  }

  // Runs on mount too. Reads `ingredient.id` and `showArchived` synchronously, so a
  // change to either reloads.
  $effect(() => {
    void load();
  });

  function edit(lot: Lot | null) {
    error = null;
    form = lotForm(lot, ingredient.default_unit, currency);
    editing = lot?.id ?? "new";
  }

  // A refusal belongs to the form it came from; closing the form drops it.
  function cancel() {
    editing = null;
    error = null;
  }

  async function save() {
    const input = toLotInput(ingredient.id, $state.snapshot(form));
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    busy = true;
    error = null;
    try {
      if (editing === "new") await createLot(input.value);
      else if (editing !== null) await updateLot(editing, input.value);
      editing = null;
      await load();
      onchanged();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  async function setArchived(lot: Lot, archived: boolean) {
    error = null;
    try {
      await (archived ? archiveLot(lot.id) : restoreLot(lot.id));
      await load();
      onchanged();
    } catch (e) {
      error = toApiError(e);
    }
  }
</script>

<div class="lots">
  {#if lots.length === 0}
    <p class="empty">No lots{showArchived ? "" : " in stock"}.</p>
  {:else}
    <table>
      <thead>
        <tr>
          <th>Qty</th>
          <th>Supplier / lot</th>
          <th>Cost</th>
          <th>Purchased</th>
          <th>Expires</th>
          <th>Used by</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each lots as lot (lot.id)}
          <tr class:archived={lot.archived_at !== null}>
            <td>{formatQuantity(lot.entry)}</td>
            <td>{[lot.supplier, lot.lot_code].filter(Boolean).join(" · ") || "—"}</td>
            <td>
              {#if lot.cost}
                {formatMoney(lot.cost)} <small>{formatUnitCost(lot)}</small>
              {:else}
                —
              {/if}
            </td>
            <td>{lot.purchased_at !== null ? formatTimestamp(lot.purchased_at, day) : "—"}</td>
            <td>{lot.expires_at !== null ? formatTimestamp(lot.expires_at, day) : "—"}</td>
            <td>
              {#if lot.batch_count > 0}
                <a href={href({ name: "query", id: null, preset: { lot: lot.id } })}
                  >{lot.batch_count} batch{lot.batch_count === 1 ? "" : "es"}</a
                >
              {:else}
                —
              {/if}
            </td>
            <td class="row-actions">
              <button class="link" onclick={() => edit(lot)}>Edit</button>
              {#if lot.archived_at === null}
                <button class="link" onclick={() => setArchived(lot, true)} title="Used up — hides it from pickers"
                  >Archive</button
                >
              {:else}
                <button class="link" onclick={() => setArchived(lot, false)}>Restore</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if editing !== null}
    <h4>{editing === "new" ? "New lot" : "Edit lot"}</h4>
    <label>
      <span>Quantity</span>
      <div class="pair">
        <input type="text" inputmode="decimal" bind:value={form.qty} />
        <select bind:value={form.unit}>
          {#each units as u (u)}
            <option value={u}>{u}</option>
          {/each}
        </select>
      </div>
    </label>
    <label>
      <span>Cost (whole lot)</span>
      <div class="pair">
        <input type="text" inputmode="decimal" placeholder="12,50" bind:value={form.cost} />
        <input type="text" maxlength="3" class="code" bind:value={form.currency} />
      </div>
      <small>what the receipt says for all of it; blank if unknown</small>
    </label>
    <label>
      <span>Supplier</span>
      <input type="text" bind:value={form.supplier} />
    </label>
    <label>
      <span>Lot code</span>
      <input type="text" bind:value={form.lot_code} />
    </label>
    <label>
      <span>Purchased</span>
      <input type="date" bind:value={form.purchased} />
    </label>
    <label>
      <span>Expires</span>
      <input type="date" bind:value={form.expires} />
    </label>
    <label>
      <span>Notes</span>
      <input type="text" bind:value={form.notes} />
    </label>
    <div class="actions">
      <button onclick={save} disabled={busy}>Save</button>
      <button class="link" onclick={cancel}>Cancel</button>
    </div>
  {:else}
    <button class="link" onclick={() => edit(null)}>Add a lot</button>
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</div>

<style>
  .lots {
    margin: 0.25rem 0 0.75rem 1.5rem;
  }

  table {
    border-collapse: collapse;
    margin-bottom: 0.5rem;
  }

  th,
  td {
    padding: 0.2rem 0.75rem 0.2rem 0;
    text-align: left;
    font-variant-numeric: tabular-nums;
  }

  th {
    color: #6f757d;
    font-weight: normal;
  }

  tr.archived td {
    color: #6f757d;
  }

  td small {
    color: #6f757d;
  }

  .row-actions {
    display: flex;
    gap: 0.75rem;
  }

  .pair {
    display: flex;
    gap: 0.5rem;
  }

  .pair input:first-child {
    flex: 1;
  }

  .code {
    width: 3.5rem;
    text-transform: uppercase;
  }

  h4 {
    margin: 0.75rem 0 0.5rem;
  }

  a {
    color: #7ba7d4;
  }
</style>
