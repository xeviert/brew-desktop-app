<script lang="ts">
  import { onMount, untrack } from "svelte";
  import {
    batchDashboard,
    onSyncStatus,
    setBatchOg,
    toApiError,
    type ApiError,
    type Config,
    type Dashboard,
  } from "$lib/api";
  import { estimateText, fgSourceText, formatGravityIn, gravitySource, stabilityText } from "$lib/dashboard";
  import { age } from "$lib/format";
  import { formatTemp } from "$lib/units";

  let { batchId, code, ui }: { batchId: number; code: string; ui: Config["ui"] } = $props();

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

  async function useAsOg(sg: number) {
    saving = true;
    try {
      await setBatchOg(batchId, sg);
      await refresh();
    } catch (e) {
      error = toApiError(e);
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    void ui.gravity_display;
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
  <h3>{code}</h3>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}

  {#if card}
    <dl>
      <dt>OG</dt>
      <dd>
        {#if card.display.og !== null}
          {formatGravityIn(card.display.og, unit)}
        {:else}
          <span class="muted">no OG</span>
          {#if card.og_candidate && card.display.og_candidate !== null}
            {@const c = card.og_candidate}
            <button disabled={saving} onclick={() => useAsOg(c.sg)}>
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
    </dl>
  {/if}
</div>

<style>
  .card {
    background: #1e2127;
    border: 1px solid #333842;
    padding: 0.75rem 1rem;
    margin-bottom: 0.75rem;
  }

  h3 {
    margin: 0 0 0.5rem;
  }

  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 1rem;
    margin: 0;
  }

  dt {
    color: #9aa0a6;
  }

  dd {
    margin: 0;
  }

  .muted {
    color: #6f757d;
  }

  button {
    margin-left: 0.5rem;
    background: #1e2127;
    color: #9aa0a6;
    border: 1px solid #333842;
    padding: 0.1rem 0.5rem;
  }
</style>
