<script lang="ts">
  import { onMount } from "svelte";
  import {
    addLotAdjustment,
    deleteLotAdjustment,
    lotLedger,
    setLotRemaining,
    toApiError,
    type ApiError,
    type Lot,
    type LotAdjustment,
    type LotLedger,
    type Unit,
  } from "$lib/api";
  import { confirmAction } from "$lib/confirm";
  import { adjustForm, adjustmentText, toAdjustInput, type AdjustForm } from "$lib/inventory";
  import { formatQuantity } from "$lib/reference";
  import { formatTimestamp } from "$lib/units";

  // One lot's books: bought, into batches, adjusted, left; and the form that takes stock
  // out or counts what is there. `onchanged` refreshes the lot row.
  let {
    lot,
    units,
    onchanged,
  }: { lot: Lot; units: Unit[]; onchanged: () => void } = $props();

  let ledger = $state.raw<LotLedger | null>(null);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);
  let form = $state<AdjustForm>(adjustForm("kg"));

  const readonly = $derived(lot.archived_at !== null);

  onMount(() => {
    form = adjustForm(lot.entry.unit);
    void load();
  });

  async function load() {
    try {
      ledger = await lotLedger(lot.id);
      error = null;
    } catch (e) {
      error = toApiError(e);
    }
  }

  async function attempt(write: () => Promise<LotLedger>) {
    busy = true;
    error = null;
    try {
      ledger = await write();
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
    const input = toAdjustInput(form);
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    const v = input.value;
    const ok = await attempt(() =>
      v.mode === "count"
        ? setLotRemaining(lot.id, null, v.counted, v.reason)
        : addLotAdjustment(lot.id, null, v.adjustment),
    );
    if (ok) form = { ...adjustForm(form.unit), mode: form.mode };
  }

  async function remove(a: LotAdjustment) {
    if (!(await confirmAction(`Delete the ${adjustmentText(a.entry)} adjustment (${a.reason})?`, "Delete"))) return;
    void attempt(() => deleteLotAdjustment(a.id));
  }
</script>

<div class="ledger">
  {#if ledger}
    <p class="books">
      Bought {formatQuantity(ledger.stock.bought)} · into batches {formatQuantity(ledger.stock.used)}
      {#if ledger.stock.adjusted.value !== 0}· adjusted {adjustmentText(ledger.stock.adjusted)}{/if}
      · <strong>{formatQuantity(ledger.stock.remaining)} left</strong>
    </p>
    {#if ledger.adjustments.length > 0}
      <ul>
        {#each ledger.adjustments as a (a.id)}
          <li>
            <span class="when">{formatTimestamp(a.adjusted_at)}</span>
            <span>{adjustmentText(a.entry)} · {a.reason}</span>
            {#if !readonly}
              <button class="link" disabled={busy} onclick={() => remove(a)}>Delete</button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  {#if !readonly}
    <form onsubmit={save}>
      <select bind:value={form.mode} aria-label="Adjustment">
        <option value="remove">Take out</option>
        <option value="count">Counted, left</option>
      </select>
      <input type="text" inputmode="decimal" bind:value={form.qty} aria-label="Quantity" />
      <select bind:value={form.unit} aria-label="Unit">
        {#each units as u (u)}
          <option value={u}>{u}</option>
        {/each}
      </select>
      <input
        type="text"
        bind:value={form.reason}
        aria-label="Reason"
        placeholder={form.mode === "count" ? "stocktake" : "spilt, gave away"}
      />
      <button type="submit" disabled={busy}>{form.mode === "count" ? "Set" : "Take out"}</button>
    </form>
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</div>

<style>
  .ledger {
    margin: 0.25rem 0 0.75rem;
    padding: 0.5rem 0.75rem;
    background: var(--surface-2);
    border-left: 3px solid var(--accent);
  }

  .books {
    margin: 0 0 0.4rem;
  }

  ul {
    list-style: none;
    margin: 0 0 0.5rem;
    padding: 0;
  }

  li {
    display: flex;
    gap: 0.75rem;
    align-items: baseline;
  }

  .when {
    color: var(--text-faint);
    min-width: 11rem;
  }

  form {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    align-items: center;
  }

  form input[inputmode="decimal"] {
    width: 5rem;
  }

  form input:not([inputmode]) {
    flex: 1;
    min-width: 8rem;
  }
</style>
