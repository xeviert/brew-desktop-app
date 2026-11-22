<script lang="ts">
  import { recordQuickEntry, toApiError, type ApiError, type Config } from "$lib/api";
  import { GRAVITY_UNIT } from "$lib/batch";
  import { quickForm, toQuickEntry, type QuickForm } from "$lib/timeline";

  // 3e's quick-entry form: whatever was measured at one moment, in one call.
  let {
    batchId,
    config,
    onrecorded,
    oncancel,
  }: { batchId: number; config: Config; onrecorded: () => void; oncancel: () => void } = $props();

  // svelte-ignore state_referenced_locally -- a seed; the form opens fresh each time.
  let form = $state<QuickForm>(quickForm(config, Date.now() / 1000));
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  const temp = $derived(`°${config.ui.temp_display}`);

  async function save() {
    const input = toQuickEntry(form, config.ui.temp_display);
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    busy = true;
    error = null;
    try {
      await recordQuickEntry(batchId, input.value);
      onrecorded();
    } catch (e) {
      error = toApiError(e);
      // The backend's message carries the raw epoch; say it in the form's terms.
      if (error.code === "db.measurement_exists") {
        error = { ...error, message: "That reading is already logged at this time. Delete it first, or change the time." };
      }
    } finally {
      busy = false;
    }
  }
</script>

<div class="ref entry">
  <h4>Add a reading</h4>
  <label>
    <span>Taken at</span>
    <input type="datetime-local" bind:value={form.at} />
  </label>
  <label>
    <span>Gravity</span>
    <span class="inline">
      <input type="text" inputmode="decimal" bind:value={form.gravity} />
      <select bind:value={form.instrument}>
        <option value="hydrometer">hydrometer</option>
        <option value="refractometer">refractometer</option>
      </select>
      {#if form.instrument === "hydrometer"}
        <select bind:value={form.scale}>
          {#each ["sg", "plato", "brix"] as const as s (s)}
            <option value={s}>{GRAVITY_UNIT[s]}</option>
          {/each}
        </select>
      {:else}
        <span class="unit">°Bx</span>
      {/if}
    </span>
  </label>
  {#if form.instrument === "hydrometer"}
    <label>
      <span>Sample temp ({temp})</span>
      <input type="text" inputmode="decimal" bind:value={form.sampleTemp} />
      <small>Blank for no correction.</small>
    </label>
    <label>
      <span>Calibrated at ({temp})</span>
      <input type="text" inputmode="decimal" bind:value={form.calibrationTemp} />
    </label>
  {:else}
    <label>
      <span>Wort factor</span>
      <input type="text" inputmode="decimal" bind:value={form.wortCorrection} />
      <small>1.0 for a sugar wash. Read after the pitch, it is corrected against the batch's OG.</small>
    </label>
  {/if}
  <label>
    <span>pH</span>
    <input type="text" inputmode="decimal" bind:value={form.ph} />
  </label>
  <label>
    <span>Temp ({temp})</span>
    <input type="text" inputmode="decimal" bind:value={form.temp} />
  </label>
  <label>
    <span>TA (g/L)</span>
    <input type="text" inputmode="decimal" bind:value={form.ta} />
  </label>
  <label>
    <span>Taste note</span>
    <input type="text" bind:value={form.note} placeholder="dry, lemony, a little sulphur" />
  </label>
  <div class="actions">
    <button onclick={save} disabled={busy}>Record</button>
    <button class="link" onclick={oncancel}>Cancel</button>
  </div>
  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</div>

<style>
  .entry {
    padding: 0.5rem 0.75rem;
    margin-bottom: 0.75rem;
    background: #1e2127;
    border-left: 3px solid #2d5d8a;
  }

  h4 {
    margin: 0 0 0.5rem;
  }

  .inline {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    color: inherit;
  }

  .inline input {
    width: 7rem;
  }

  .unit {
    color: #9aa0a6;
  }
</style>
