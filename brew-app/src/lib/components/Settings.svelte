<script lang="ts">
  import { setConfig, toApiError, type ApiError, type Config } from "$lib/api";

  let { config, onsaved }: { config: Config; onsaved: (c: Config) => void } = $props();

  // A working copy behind an explicit Save, not save-on-keystroke: every save is an
  // fsync'd file write. `$state.snapshot` first — `structuredClone` on a `$state`
  // proxy throws `DataCloneError`.
  // svelte-ignore state_referenced_locally -- the one-shot read is the seed; the
  // `$effect` below keeps it in step afterwards.
  let draft = $state<Config>(clone(config));
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let saved = $state(false);

  const dirty = $derived(JSON.stringify(draft) !== JSON.stringify(config));

  // Re-seed from the stored value whenever it changes, so a save's normalised result
  // (trimmed URL, clamped interval) lands in the fields. Reads only `config`, so the
  // reassignment `onsaved` triggers upstream cannot loop.
  $effect(() => {
    draft = clone(config);
  });

  function clone(c: Config): Config {
    return structuredClone($state.snapshot(c));
  }

  async function save() {
    saving = true;
    error = null;
    saved = false;
    try {
      // Adopt what was stored: the URL may come back trimmed, the interval clamped.
      const stored = await setConfig($state.snapshot(draft));
      onsaved(stored);
      saved = true;
    } catch (e) {
      error = toApiError(e);
    } finally {
      saving = false;
    }
  }
</script>

<section>
  <h2>Settings</h2>

  <label>
    <span>Fermentrack URL</span>
    <!-- `text`, not `url`: an empty value is legal and means "not set up yet". -->
    <input type="text" bind:value={draft.fermentrack.base_url} placeholder="http://fermentrack.local" />
  </label>

  <label>
    <span>Poll interval</span>
    <input type="number" min="5" max="86400" step="5" bind:value={draft.sync.poll_interval_secs} />
    <small>seconds — takes effect in Phase 2</small>
  </label>

  <label>
    <span>Temperature</span>
    <select bind:value={draft.ui.temp_display}>
      <option value="C">Celsius</option>
      <option value="F">Fahrenheit</option>
    </select>
  </label>

  <label>
    <span>Gravity</span>
    <select bind:value={draft.ui.gravity_display}>
      <option value="sg">Specific gravity</option>
      <option value="plato">Plato</option>
      <option value="brix">Brix</option>
    </select>
  </label>

  <div class="actions">
    <button onclick={save} disabled={!dirty || saving}>
      {saving ? "Saving…" : "Save"}
    </button>
    {#if saved && !dirty}
      <span class="ok">Saved</span>
    {/if}
  </div>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  section {
    max-width: 32rem;
  }

  label {
    display: grid;
    grid-template-columns: 10rem 1fr;
    align-items: center;
    gap: 0.5rem 1rem;
    margin-bottom: 0.75rem;
  }

  label span {
    color: #9aa0a6;
  }

  input,
  select {
    padding: 0.35rem 0.5rem;
    background: #1e2127;
    color: inherit;
    border: 1px solid #333842;
    border-radius: 3px;
    font: inherit;
  }

  small {
    grid-column: 2;
    color: #6f757d;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-top: 1rem;
  }

  button {
    padding: 0.4rem 1rem;
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
    cursor: default;
  }

  .ok {
    color: #7fb069;
  }

  p[role="alert"] {
    color: #e06c5f;
  }
</style>
