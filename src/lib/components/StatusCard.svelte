<script lang="ts">
  import { onMount, untrack } from "svelte";
  import Devices from "$lib/components/Devices.svelte";
  import {
    batchDashboard,
    onSyncStatus,
    setBatchFg,
    setBatchOg,
    toApiError,
    type ApiError,
    type Config,
    type Dashboard,
    type SourceStatus,
  } from "$lib/api";
  import { estimateText, fgSourceText, formatGravityIn, gravitySource, stabilityText } from "$lib/dashboard";
  import { age } from "$lib/format";
  import { formatTemp } from "$lib/units";

  // `revision` refetches in place after a write from the screen; `readonly` hides the
  // OG/FG writes on an archived batch.
  let {
    batchId,
    ui,
    syncs,
    revision,
    readonly,
  }: {
    batchId: number;
    ui: Config["ui"];
    syncs: Record<number, SourceStatus>;
    revision: number;
    readonly: boolean;
  } = $props();

  let card = $state.raw<Dashboard | null>(null);
  let error = $state<ApiError | null>(null);
  let saving = $state(false);

  const unit = $derived(card?.gravity_unit ?? "SG");
  const now = $derived(card ? Date.now() / 1000 : 0);

  let busy = false;
  let again = false;

  async function refresh() {
    if (busy) {
      again = true;
      return;
    }
    busy = true;
    try {
      card = await batchDashboard(batchId);
      error = null;
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
      if (again) {
        again = false;
        void refresh();
      }
    }
  }

  /** OG or FG, in SG, or `null` to clear it. */
  async function setGravity(which: "og" | "fg", sg: number | null) {
    saving = true;
    try {
      await (which === "og" ? setBatchOg(batchId, sg) : setBatchFg(batchId, sg));
      await refresh();
    } catch (e) {
      error = toApiError(e);
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    void ui.gravity_display;
    void revision;
    untrack(() => void refresh());
  });

  onMount(() => {
    // Every poll, not only ones that added rows: a poll that adds nothing is exactly
    // when the temperature goes stale.
    return onSyncStatus((s) => {
      const ids = card?.source_ids ?? [];
      if (ids.length === 0 || ids.includes(s.source_id)) void refresh();
    });
  });
</script>

<div class="card">
  {#if error}
    <p role="alert">{error.message}</p>
  {/if}

  {#if card}
    <dl>
      <dt>OG</dt>
      <dd>
        {#if card.display.og !== null}
          {formatGravityIn(card.display.og, unit)}
          {#if !readonly}<button class="clear" disabled={saving} onclick={() => setGravity("og", null)}>clear</button>{/if}
        {:else}
          <span class="muted">no OG</span>
          {#if !readonly && card.og_candidate && card.display.og_candidate !== null}
            {@const c = card.og_candidate}
            <button disabled={saving} onclick={() => setGravity("og", c.sg)}>
              Use {formatGravityIn(card.display.og_candidate, unit)} ({gravitySource(c)}, {age(c.ts)}) as OG
            </button>
          {/if}
        {/if}
      </dd>

      <dt>Gravity</dt>
      <dd>
        {#if card.gravity && card.display.gravity !== null}
          {formatGravityIn(card.display.gravity, unit)}
          <span class="muted">{gravitySource(card.gravity)}, {age(card.gravity.ts)}</span>
        {:else}
          <span class="muted">—</span>
        {/if}
      </dd>

      <dt>FG</dt>
      <dd>
        {#if card.display.fg !== null}
          {formatGravityIn(card.display.fg, unit)}
          {#if !readonly}<button class="clear" disabled={saving} onclick={() => setGravity("fg", null)}>clear</button>{/if}
        {:else}
          <span class="muted">not set</span>
          {#if !readonly && card.gravity && card.display.gravity !== null}
            {@const g = card.gravity}
            <button disabled={saving} onclick={() => setGravity("fg", g.sg)}>
              Use {formatGravityIn(card.display.gravity, unit)} ({gravitySource(g)}, {age(g.ts)}) as FG
            </button>
          {/if}
        {/if}
      </dd>

      <dt>ABV</dt>
      <dd class:muted={card.estimate.state !== "estimated"}>{estimateText(card.estimate)}</dd>

      <dt>pH</dt>
      <dd>
        {#if card.ph}
          {card.ph.value.toFixed(2)} <span class="muted">{age(card.ph.ts)}</span>
        {:else}
          <span class="muted">—</span>
        {/if}
      </dd>

      <dt>Temp</dt>
      <dd>
        {#if card.temp}
          {#if card.temp.stale}
            <s title="No reading for longer than this device reports">{formatTemp(card.temp.value, ui.temp_display)}</s>
          {:else}
            {formatTemp(card.temp.value, ui.temp_display)}
          {/if}
          <span class="muted">{card.temp.label}, {age(card.temp.ts)}</span>
        {:else}
          <span class="muted">—</span>
        {/if}
      </dd>

      <dt>Status</dt>
      <dd>
        {stabilityText(card.stability, now)}
        {#if card.expected_fg && card.display.expected_fg_lo !== null && card.display.expected_fg_hi !== null}
          <span class="muted">
            expected FG {formatGravityIn(card.display.expected_fg_lo, unit)}–{formatGravityIn(
              card.display.expected_fg_hi,
              unit,
            )} ({fgSourceText(card.expected_fg)})
          </span>
        {/if}
      </dd>

      {#if card.devices.length > 0}
        <dt>Devices</dt>
        <dd><Devices devices={card.devices} {syncs} /></dd>
      {/if}
    </dl>
  {/if}
</div>

<style>
  .card {
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    padding: 0.75rem 1rem;
    margin-bottom: 0.75rem;
  }

  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 1rem;
    margin: 0;
  }

  dt {
    color: var(--text-muted);
  }

  dd {
    margin: 0;
  }

  .muted {
    color: var(--text-faint);
  }

  button {
    margin-left: 0.5rem;
    background: var(--surface-2);
    color: var(--text-muted);
    border: 1px solid var(--border-strong);
    padding: 0.1rem 0.5rem;
  }

  button.clear {
    border: none;
    color: var(--text-faint);
  }
</style>
