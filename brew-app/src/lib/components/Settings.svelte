<script lang="ts">
  import { checkFermentrack, listJournals, setConfig, toApiError, type ApiError, type Config } from "$lib/api";

  let { config, onsaved }: { config: Config; onsaved: (c: Config) => void } = $props();

  /**
   * Svelte's number binding is `value === "" ? null : +value`, so an empty field
   * puts `null` in the draft. That is not a `Config`, and sending it anyway fails in
   * serde *before* the command runs — Tauri answers with a raw string, not an
   * `ApiError`, and the pane shows "invalid args `config` for command `set_config`".
   * Modelling the hole instead lets Save refuse it with something readable.
   */
  type Draft = Omit<Config, "sync" | "health"> & {
    sync: { poll_interval_secs: number | null };
    health: { battery_warn_v: number | null; rssi_warn_dbm: number | null };
  };

  // A working copy behind an explicit Save, not save-on-keystroke: every save is an
  // fsync'd file write. `$state.snapshot` first — `structuredClone` on a `$state`
  // proxy throws `DataCloneError`.
  // svelte-ignore state_referenced_locally -- the one-shot read is the seed; the
  // `$effect` below keeps it in step afterwards.
  let draft = $state<Draft>(clone(config));
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let saved = $state(false);
  let testing = $state(false);
  /** `null` until Test has run. A string either way — the count, or why not. */
  let reachable = $state<string | null>(null);
  let testingBrewd = $state(false);
  let brewdReachable = $state<string | null>(null);

  const dirty = $derived(JSON.stringify(draft) !== JSON.stringify(config));

  // Not coerced to a number on input: snapping an empty field back to 0 or to the
  // default fights the user mid-keystroke. Let it stay empty and block Save.
  const intervalMissing = $derived(!Number.isFinite(draft.sync.poll_interval_secs));
  const batteryMissing = $derived(!Number.isFinite(draft.health.battery_warn_v));
  const rssiMissing = $derived(!Number.isFinite(draft.health.rssi_warn_dbm));

  // Re-seed from the stored value whenever it changes, so a save's normalised result
  // (trimmed URL, clamped interval) lands in the fields. Reads only `config`, so the
  // reassignment `onsaved` triggers upstream cannot loop.
  $effect(() => {
    draft = clone(config);
  });

  function clone(c: Config): Draft {
    return structuredClone($state.snapshot(c));
  }

  async function save() {
    saving = true;
    error = null;
    saved = false;
    try {
      const snapshot = $state.snapshot(draft);
      const poll_interval_secs = snapshot.sync.poll_interval_secs;
      const { battery_warn_v, rssi_warn_dbm } = snapshot.health;
      if (poll_interval_secs === null || battery_warn_v === null || rssi_warn_dbm === null) return;
      // Adopt what was stored: the URL may come back trimmed, the interval clamped.
      const stored = await setConfig({
        ...snapshot,
        sync: { poll_interval_secs },
        health: { battery_warn_v, rssi_warn_dbm },
      });
      onsaved(stored);
      saved = true;
    } catch (e) {
      error = toApiError(e);
    } finally {
      saving = false;
    }
  }

  /**
   * `GET /api/devices/` against whatever is in the field, saved or not. It is the only
   * endpoint that enumerates anything, and all it lists is device ids — which is why
   * the log URL itself still has to be pasted in the Sources pane.
   */
  async function test() {
    testing = true;
    reachable = null;
    try {
      const devices = await checkFermentrack(draft.fermentrack.base_url);
      reachable = devices.length === 1 ? "1 device" : `${devices.length} devices`;
    } catch (e) {
      reachable = toApiError(e).message;
    } finally {
      testing = false;
    }
  }

  /** `GET /journals/`: only brewd answers it with a journal list, so a count is proof. */
  async function testBrewd() {
    testingBrewd = true;
    brewdReachable = null;
    try {
      const journals = await listJournals(draft.brewd.base_url);
      brewdReachable = journals.length === 1 ? "1 journal" : `${journals.length} journals`;
    } catch (e) {
      brewdReachable = toApiError(e).message;
    } finally {
      testingBrewd = false;
    }
  }
</script>

<section>
  <h2>Settings</h2>

  <label>
    <span>Fermentrack URL</span>
    <!-- `text`, not `url`: an empty value is legal and means "not set up yet". -->
    <input type="text" bind:value={draft.fermentrack.base_url} placeholder="http://fermentrack.local" />
    <small>
      <button class="link" onclick={test} disabled={testing || !draft.fermentrack.base_url.trim()}>
        {testing ? "Testing…" : "Test connection"}
      </button>
      {#if reachable}<span>· {reachable}</span>{/if}
    </small>
  </label>

  <label>
    <span>brewd URL</span>
    <input type="text" bind:value={draft.brewd.base_url} placeholder="http://brewd.local:8080" />
    <small>
      <button class="link" onclick={testBrewd} disabled={testingBrewd || !draft.brewd.base_url.trim()}>
        {testingBrewd ? "Testing…" : "Test connection"}
      </button>
      {#if brewdReachable}<span>· {brewdReachable}</span>{:else}<span>· the Pi — leave empty until it is set up</span>{/if}
    </small>
  </label>

  <label>
    <span>Poll interval</span>
    <input type="number" min="5" max="86400" step="5" bind:value={draft.sync.poll_interval_secs} />
    {#if intervalMissing}
      <small class="warn">required — 5 to 86400 seconds</small>
    {:else}
      <small>seconds — takes effect in Phase 2</small>
    {/if}
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

  <label>
    <span>CSV export</span>
    <select bind:value={draft.export.dialect}>
      <option value="rfc4180">RFC 4180</option>
      <option value="excel_fr">Excel (French) — ; separator, , decimal</option>
    </select>
    <small>files are always in canonical units</small>
  </label>

  <label>
    <span>iSpindle outputs</span>
    <!-- No default: the device publishes a bare number, and SG and °P both look plausible. -->
    <select bind:value={draft.gravity.ispindel_unit}>
      <option value={null}>Not set</option>
      <option value="sg">Specific gravity</option>
      <option value="plato">Plato</option>
    </select>
    <small>whatever its calibration polynomial was written in</small>
  </label>

  <label>
    <span>Battery warning</span>
    <input type="number" min="3" max="4.2" step="0.05" bind:value={draft.health.battery_warn_v} />
    {#if batteryMissing}
      <small class="warn">required — 3.0 to 4.2 V</small>
    {:else}
      <small>volts — a device under this is flagged</small>
    {/if}
  </label>

  <label>
    <span>Signal warning</span>
    <input type="number" min="-100" max="-50" step="1" bind:value={draft.health.rssi_warn_dbm} />
    {#if rssiMissing}
      <small class="warn">required — −100 to −50 dBm</small>
    {:else}
      <small>dBm — a device under this is flagged</small>
    {/if}
  </label>

  <div class="actions">
    <button onclick={save} disabled={!dirty || saving || intervalMissing || batteryMissing || rssiMissing}>
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

  small.warn {
    color: #e0a35f;
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

  button.link {
    padding: 0;
    background: none;
    color: #7ba7d4;
    text-decoration: underline;
  }

  button.link:disabled {
    background: none;
    color: #6f757d;
    text-decoration: none;
  }

  p[role="alert"] {
    color: #e06c5f;
  }
</style>
