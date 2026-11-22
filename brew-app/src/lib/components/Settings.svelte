<script lang="ts">
  import { onMount } from "svelte";
  import {
    alertSyncStatus,
    checkFermentrack,
    listJournals,
    onAlerts,
    setConfig,
    testNotification,
    toApiError,
    type AlertSyncStatus,
    type ApiError,
    type Config,
  } from "$lib/api";
  import { syncText } from "$lib/alerts";

  let { config, onsaved }: { config: Config; onsaved: (c: Config) => void } = $props();

  /**
   * Svelte's number binding is `value === "" ? null : +value`, so an empty field
   * puts `null` in the draft. That is not a `Config`, and sending it anyway fails in
   * serde *before* the command runs — Tauri answers with a raw string, not an
   * `ApiError`, and the pane shows "invalid args `config` for command `set_config`".
   * Modelling the hole instead lets Save refuse it with something readable.
   */
  type Draft = Omit<Config, "sync" | "health" | "alerts"> & {
    sync: { poll_interval_secs: number | null };
    health: { battery_warn_v: number | null; rssi_warn_dbm: number | null };
    alerts: {
      temp_rate_delta_c: number | null;
      temp_rate_window_hours: number | null;
      overdue_grace_hours: number | null;
    };
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
  let notifying = $state(false);
  let notified = $state<string | null>(null);
  let alertStatus = $state.raw<AlertSyncStatus | null>(null);

  onMount(() => {
    alertSyncStatus().then((s) => (alertStatus = s), () => {});
    return onAlerts((s) => (alertStatus = s));
  });

  /** Through `brewd` to its ntfy topic, with the saved URL and token. */
  async function notifyTest() {
    notifying = true;
    notified = null;
    try {
      await testNotification();
      notified = "sent — check the phone";
    } catch (e) {
      notified = toApiError(e).message;
    } finally {
      notifying = false;
    }
  }

  const dirty = $derived(JSON.stringify(draft) !== JSON.stringify(config));

  // Not coerced to a number on input: snapping an empty field back to 0 or to the
  // default fights the user mid-keystroke. Let it stay empty and block Save.
  const intervalMissing = $derived(!Number.isFinite(draft.sync.poll_interval_secs));
  const batteryMissing = $derived(!Number.isFinite(draft.health.battery_warn_v));
  const rssiMissing = $derived(!Number.isFinite(draft.health.rssi_warn_dbm));
  const rateMissing = $derived(
    !Number.isFinite(draft.alerts.temp_rate_delta_c) || !Number.isFinite(draft.alerts.temp_rate_window_hours),
  );
  const graceMissing = $derived(!Number.isFinite(draft.alerts.overdue_grace_hours));
  // The Rust side refuses anything but three letters, and refuses it in serde, as a raw string.
  const currencyInvalid = $derived(!/^[A-Za-z]{3}$/.test(draft.money.default_currency.trim()));

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
      const { temp_rate_delta_c, temp_rate_window_hours, overdue_grace_hours } = snapshot.alerts;
      if (poll_interval_secs === null || battery_warn_v === null || rssi_warn_dbm === null) return;
      if (temp_rate_delta_c === null || temp_rate_window_hours === null || overdue_grace_hours === null) return;
      // Adopt what was stored: the URL may come back trimmed, the interval clamped.
      const stored = await setConfig({
        ...snapshot,
        sync: { poll_interval_secs },
        health: { battery_warn_v, rssi_warn_dbm },
        alerts: { temp_rate_delta_c, temp_rate_window_hours, overdue_grace_hours },
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

  // Not `<a href="#id">`: the router owns the hash, and would read it as a route.
  function jump(id: string) {
    document.getElementById(id)?.scrollIntoView({ behavior: "smooth" });
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
  <nav class="toc">
    <button class="link" onclick={() => jump("settings-display")}>Display</button>
    <button class="link" onclick={() => jump("settings-connections")}>Connections</button>
    <button class="link" onclick={() => jump("settings-alerts")}>Alerts</button>
    <button class="link" onclick={() => jump("settings-devices")}>iSpindle</button>
    <button class="link" onclick={() => jump("sources")}>Sources</button>
    <button class="link" onclick={() => jump("vessels")}>Vessels</button>
  </nav>

  <h3 id="settings-display">Display</h3>
  <p class="intro">How numbers are shown and exported. Nothing stored changes.</p>

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
      <option value="sg">Specific gravity (1.050)</option>
      <option value="plato">Plato (°P)</option>
      <option value="brix">Brix (°Bx)</option>
    </select>
    <small>also the unit gravities are typed in</small>
  </label>

  <label>
    <span>CSV export</span>
    <select bind:value={draft.export.dialect}>
      <option value="rfc4180">Standard — comma separated, 1.5</option>
      <option value="excel_fr">Excel, European — semicolon separated, 1,5</option>
    </select>
    <small>pick European if Excel opens exports as one column. Values are always SG, °C, g and L</small>
  </label>

  <label>
    <span>Default currency</span>
    <input type="text" maxlength="3" bind:value={draft.money.default_currency} />
    {#if currencyInvalid}
      <small class="warn">a three-letter code, e.g. EUR</small>
    {:else}
      <small>filled in when you add a lot's cost; amounts are never converted</small>
    {/if}
  </label>

  <h3 id="settings-connections">Connections</h3>
  <p class="intro">
    Where the app fetches sensor readings from. Leave a URL empty if you don't run that service. Each log or
    journal is then added under <button class="link" onclick={() => jump("sources")}>Sources</button>.
  </p>

  <label>
    <span>brewd URL</span>
    <input type="text" bind:value={draft.brewd.base_url} placeholder="http://brewd.local:8080" />
    <small>
      the logger on the Raspberry Pi that records the iSpindle and temperature probes.
      <button class="link" onclick={testBrewd} disabled={testingBrewd || !draft.brewd.base_url.trim()}>
        {testingBrewd ? "Testing…" : "Test connection"}
      </button>
      {#if brewdReachable}<span>· {brewdReachable}</span>{/if}
    </small>
  </label>

  <label>
    <span>Fermentrack URL</span>
    <!-- `text`, not `url`: an empty value is legal and means "not set up yet". -->
    <input type="text" bind:value={draft.fermentrack.base_url} placeholder="http://fermentrack.local" />
    <small>
      a Fermentrack server, if you have one; its CSV logs can be added as sources.
      <button class="link" onclick={test} disabled={testing || !draft.fermentrack.base_url.trim()}>
        {testing ? "Testing…" : "Test connection"}
      </button>
      {#if reachable}<span>· {reachable}</span>{/if}
    </small>
  </label>

  <label>
    <span>Check every</span>
    <input type="number" min="5" max="86400" step="5" bind:value={draft.sync.poll_interval_secs} />
    {#if intervalMissing}
      <small class="warn">required — 5 to 86400 seconds</small>
    {:else}
      <small>seconds between fetches of new readings from each source; slows down while one is failing</small>
    {/if}
  </label>

  <h3 id="settings-alerts">Alerts</h3>
  <p class="intro">
    brewd on the Pi checks every reading against each fermenting batch — finished, stalled, too warm or cold, a
    sensor gone quiet — and pushes what fires to your phone through ntfy, laptop shut or not. Each batch's own
    thresholds are on its page.
  </p>

  <label>
    <span>brewd token</span>
    <input
      type="password"
      autocomplete="off"
      autocapitalize="none"
      spellcheck="false"
      bind:value={draft.brewd.token}
      placeholder="the http.token in brewd.toml"
    />
    <small>
      lets the app tell brewd what to watch, and acknowledge or snooze alerts. Without it, alerts are still shown
      but brewd isn't told about new batches.
    </small>
  </label>

  <div class="status-row">
    <span>Last check</span>
    <span role="status">{alertStatus ? syncText(alertStatus) : "—"}</span>
    <small>
      <button class="link" onclick={notifyTest} disabled={notifying || dirty || !config.brewd.token.trim()}>
        {notifying ? "Sending…" : "Send a test notification"}
      </button>
      {#if dirty}<span>· save first</span>{/if}
      {#if notified}<span>· {notified}</span>{/if}
    </small>
  </div>

  <label>
    <span>Temperature moves</span>
    <div class="pair">
      <input type="number" min="0.5" max="10" step="0.5" bind:value={draft.alerts.temp_rate_delta_c} />
      °C within
      <input type="number" min="0.5" max="24" step="0.5" bind:value={draft.alerts.temp_rate_window_hours} />
      h
    </div>
    {#if rateMissing}
      <small class="warn">required — 0.5 to 10 °C within 0.5 to 24 h</small>
    {:else}
      <small>while still in the batch's range: a stuck relay or a fridge door left open</small>
    {/if}
  </label>

  <label>
    <span>Overdue after</span>
    <input type="number" min="1" max="72" step="1" bind:value={draft.alerts.overdue_grace_hours} />
    {#if graceMissing}
      <small class="warn">required — 1 to 72 hours</small>
    {:else}
      <small>hours past due with nothing logged: a schedule step ready to move on, a reminder, a timed addition</small>
    {/if}
  </label>

  <h3 id="settings-devices">iSpindle and wireless sensors</h3>
  <p class="intro">How to read the iSpindle, and when the home screen flags a sensor's battery or Wi-Fi.</p>

  <label>
    <span>iSpindle reads in</span>
    <!-- No default: the device publishes a bare number, and SG and °P both look plausible. -->
    <select bind:value={draft.gravity.ispindel_unit}>
      <option value={null}>Not set</option>
      <option value="sg">Specific gravity</option>
      <option value="plato">Plato</option>
    </select>
    <small>
      the unit its calibration formula gives. The iSpindle sends a bare number, so its gravity isn't stored until
      this is set
    </small>
  </label>

  <label>
    <span>Low battery below</span>
    <input type="number" min="3" max="4.2" step="0.05" bind:value={draft.health.battery_warn_v} />
    {#if batteryMissing}
      <small class="warn">required — 3.0 to 4.2 V</small>
    {:else}
      <small>volts. A full Li-ion cell reads about 4.2 V</small>
    {/if}
  </label>

  <label>
    <span>Weak signal below</span>
    <input type="number" min="-100" max="-50" step="1" bind:value={draft.health.rssi_warn_dbm} />
    {#if rssiMissing}
      <small class="warn">required — −100 to −50 dBm</small>
    {:else}
      <small>dBm, Wi-Fi strength. Closer to 0 is stronger: −60 is good, −85 is about to drop out</small>
    {/if}
  </label>

  <div class="actions">
    <button onclick={save} disabled={!dirty || saving || intervalMissing || batteryMissing || rssiMissing || rateMissing || graceMissing || currencyInvalid}>
      {saving ? "Saving…" : "Save settings"}
    </button>
    {#if saved && !dirty}
      <span class="ok">Saved</span>
    {:else if dirty}
      <span class="pending">Unsaved changes</span>
    {:else}
      <span class="note">Sources and vessels below save as you go.</span>
    {/if}
  </div>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  section {
    max-width: 40rem;
  }

  .toc {
    display: flex;
    gap: 1rem;
    margin-bottom: 0.5rem;
  }



  h3 {
    margin: 1.75rem 0 0.25rem;
  }

  .intro {
    margin: 0 0 0.75rem;
    color: #9aa0a6;
  }

  .pending {
    color: #e0a35f;
  }

  .note {
    color: #6f757d;
  }

  label {
    display: grid;
    grid-template-columns: 11rem 1fr;
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

  .status-row {
    display: grid;
    grid-template-columns: 11rem 1fr;
    gap: 0.5rem 1rem;
    margin-bottom: 0.75rem;
  }

  .status-row > span:first-child {
    color: #9aa0a6;
  }

  .pair {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .pair input {
    width: 4.5rem;
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
