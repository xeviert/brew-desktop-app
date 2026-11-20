<script lang="ts">
  import { onMount } from "svelte";
  import AlertList from "$lib/components/AlertList.svelte";
  import {
    getBatchAlert,
    listAlerts,
    onAlerts,
    setBatchAlert,
    toApiError,
    type AlertRow,
    type ApiError,
    type BatchAlertForm,
    type Config,
  } from "$lib/api";
  import { formatTemp, fromDisplayTemp, toDisplayTemp } from "$lib/units";

  // Phase 6 on the batch screen: what `brewd` raised for this batch, and the settings
  // it judges this batch by. A save wakes the alert round, so `brewd` hears at once.
  let {
    batchId,
    ui,
    readonly,
  }: { batchId: number; ui: Config["ui"]; readonly: boolean } = $props();

  let alerts = $state.raw<AlertRow[]>([]);
  let form = $state.raw<BatchAlertForm | null>(null);
  let error = $state<ApiError | null>(null);
  let saving = $state(false);
  let saved = $state(false);

  // The draft, in display units. `null` is "the default".
  let min = $state<number | null>(null);
  let max = $state<number | null>(null);
  let target = $state<number | null>(null);
  let muted = $state(false);

  const open = $derived(alerts.filter((a) => a.cleared_at === null));
  const closed = $derived(alerts.filter((a) => a.cleared_at !== null));

  const shown = (c: number | null) =>
    c === null ? null : Number(toDisplayTemp(c, ui.temp_display).toFixed(1));
  const stored = (v: number | null) =>
    v === null || !Number.isFinite(v) ? null : fromDisplayTemp(v, ui.temp_display);

  function seed(f: BatchAlertForm) {
    form = f;
    min = shown(f.temp_min_c);
    max = shown(f.temp_max_c);
    target = f.target === null ? null : Number(f.target.toFixed(f.gravity_unit === "SG" ? 3 : 1));
    muted = f.muted;
  }

  async function loadAlerts() {
    try {
      alerts = await listAlerts(batchId, false);
    } catch (e) {
      error = toApiError(e);
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    saving = true;
    saved = false;
    error = null;
    try {
      seed(
        await setBatchAlert(batchId, {
          temp_min_c: stored(min),
          temp_max_c: stored(max),
          target: target === null || !Number.isFinite(target) ? null : target,
          muted,
        }),
      );
      saved = true;
    } catch (e) {
      error = toApiError(e);
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    const id = batchId;
    void ui.temp_display;
    void ui.gravity_display;
    getBatchAlert(id).then(seed, (e) => (error = toApiError(e)));
    void loadAlerts();
  });

  onMount(() => onAlerts(() => void loadAlerts()));

  const placeholder = (c: number | null) => (c === null ? "none" : `${formatTemp(c, ui.temp_display)} (yeast)`);
</script>

<section aria-labelledby="batch-alerts-title">
  <h3 id="batch-alerts-title">Alerts</h3>

  {#if open.length > 0}
    <AlertList alerts={open} onchanged={() => void loadAlerts()} />
  {:else}
    <p class="muted">Nothing raised.</p>
  {/if}

  {#if closed.length > 0}
    <details>
      <summary>{closed.length} cleared</summary>
      <AlertList alerts={closed} onchanged={() => void loadAlerts()} />
    </details>
  {/if}

  {#if form && !readonly}
    <form onsubmit={save}>
      <fieldset>
        <legend>Watch this batch for</legend>
        <div class="row">
          <label for="alert-min">Colder than</label>
          <input
            id="alert-min"
            name="temp_min"
            type="number"
            step="0.5"
            bind:value={min}
            placeholder={placeholder(form.yeast_min_c)}
            aria-describedby="alert-band-help"
          />
          <span>°{ui.temp_display}</span>
        </div>
        <div class="row">
          <label for="alert-max">Warmer than</label>
          <input
            id="alert-max"
            name="temp_max"
            type="number"
            step="0.5"
            bind:value={max}
            placeholder={placeholder(form.yeast_max_c)}
            aria-describedby="alert-band-help"
          />
          <span>°{ui.temp_display}</span>
        </div>
        <small id="alert-band-help">
          for 10 minutes. Empty takes the pitched yeast's range; no yeast range, no temperature alert.
        </small>
        <div class="row">
          <label for="alert-target">Gravity down to</label>
          <input
            id="alert-target"
            name="target"
            type="number"
            step={form.gravity_unit === "SG" ? 0.001 : 0.1}
            bind:value={target}
            placeholder="none"
            aria-describedby="alert-target-help"
          />
          <span>{form.gravity_unit}</span>
        </div>
        <small id="alert-target-help">
          once, e.g. to rack or dose. Finished, stalled and a silent sensor are always watched, and the ⅓ sugar break once the OG and expected FG are known.
        </small>
        <div class="check">
          <input id="alert-muted" name="muted" type="checkbox" bind:checked={muted} />
          <label for="alert-muted">Mute — no alerts for this batch</label>
        </div>
      </fieldset>
      <div class="actions">
        <button type="submit" disabled={saving}>{saving ? "Saving…" : "Save alert settings"}</button>
        <span role="status">{saved ? "Saved — brewd is told on its next check" : ""}</span>
      </div>
    </form>
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  h3 {
    margin: 1.5rem 0 0.5rem;
  }

  .muted,
  small {
    color: #6f757d;
  }

  details {
    margin-top: 0.5rem;
  }

  summary {
    cursor: pointer;
    color: #9aa0a6;
    margin-bottom: 0.5rem;
  }

  fieldset {
    margin: 0.75rem 0 0;
    padding: 0.5rem 0.75rem 0.75rem;
    border: 1px solid #333842;
  }

  legend {
    color: #9aa0a6;
    padding: 0 0.25rem;
  }

  .row {
    display: grid;
    grid-template-columns: 8rem 7rem auto;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }

  small {
    display: block;
    margin: 0.2rem 0 0;
    font-size: 0.85em;
  }

  .check {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }

  input[type="number"] {
    padding: 0.3rem 0.5rem;
    background: #1e2127;
    color: inherit;
    border: 1px solid #4a5160;
    border-radius: 3px;
    font: inherit;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-top: 0.75rem;
  }

  button {
    padding: 0.35rem 0.9rem;
    background: #2d5d8a;
    color: #fff;
    border: none;
    border-radius: 3px;
    font: inherit;
    cursor: pointer;
  }

  button:disabled {
    background: #2a2e36;
    color: #6f757d;
  }

  [role="status"] {
    color: #7fb069;
  }

  p[role="alert"] {
    color: #e06c5f;
  }
</style>
