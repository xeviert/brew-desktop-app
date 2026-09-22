<script lang="ts">
  import {
    addSource,
    probeLog,
    toApiError,
    type ApiError,
    type LogProbe,
    type SourceSummary,
  } from "$lib/api";

  let { sources, onchanged }: { sources: SourceSummary[]; onchanged: (s: SourceSummary[]) => void } =
    $props();

  // Validate then Add, two explicit steps. A single Add that silently probes first
  // would report a graph-log URL as "could not add", which names the wrong problem.
  let input = $state("");
  let probe = $state<LogProbe | null>(null);
  let name = $state("");
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  // The probe describes whatever was in the box when Validate ran. Editing the URL
  // afterwards must retire it, or Add would write a source for a different file than
  // the one on screen.
  const stale = $derived(probe !== null && probe.url !== input && probe.remote_path !== input);

  /**
   * Extra advice for the mistakes the backend can name exactly. `code` is stable;
   * `message` already says what happened, so this only says what to do about it.
   */
  const hints: Record<string, string> = {
    "fermentrack.graph_log": "Use the _full.csv beside it — same beer, the other file.",
    "fermentrack.gravity_log": "That is a gravity sensor's own log, not a beer log.",
    "fermentrack.not_a_log": "Check the URL points at a file under /data/.",
    "fermentrack.log_missing": "Renaming a beer in Fermentrack renames its log file.",
    "fermentrack.not_fermentrack": "The host answered, but not as a Fermentrack.",
  };

  async function validate() {
    busy = true;
    error = null;
    probe = null;
    try {
      probe = await probeLog(input.trim());
      name = probe.suggested_name;
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  async function add() {
    busy = true;
    error = null;
    try {
      const added = await addSource(input.trim(), name.trim());
      onchanged([...sources, added]);
      input = "";
      probe = null;
      name = "";
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  function age(ts: number | null): string {
    if (ts === null) return "never";
    const mins = Math.round((Date.now() / 1000 - ts) / 60);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins} min ago`;
    const hours = Math.round(mins / 60);
    return hours < 48 ? `${hours} h ago` : `${Math.round(hours / 24)} d ago`;
  }
</script>

<section>
  <h2>Sources</h2>

  {#if sources.length === 0}
    <p class="empty">No logs yet. Paste one below.</p>
  {:else}
    <ul>
      {#each sources as source (source.id)}
        <li>
          <span class="name">{source.name}</span>
          <span class="path" title={source.remote_path ?? ""}>{source.remote_path ?? "—"}</span>
          <span class="age">{age(source.newest_reading)}</span>
          {#if source.last_error}
            <span class="err" role="alert">{source.last_error}</span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  <h3>Add a log</h3>
  <!-- No API lists the beers, and the log id and beer name are both in the filename,
       so there is nothing to pick from: the URL is pasted once, here. -->
  <label>
    <span>Log URL</span>
    <input
      type="text"
      bind:value={input}
      placeholder="http://fermentrack.local/data/Device 1 - B17 - Saison_full.csv"
    />
  </label>

  <div class="actions">
    <button onclick={validate} disabled={busy || input.trim() === ""}>
      {busy && !probe ? "Checking…" : "Validate"}
    </button>
    {#if probe && !stale}
      <button onclick={add} disabled={busy}>Add</button>
    {/if}
  </div>

  {#if probe && !stale}
    <dl>
      <dt>File</dt>
      <dd>{probe.file_name}</dd>
      {#if probe.device_id !== null}
        <dt>Device</dt>
        <dd>{probe.device_id}{probe.log_id !== null ? ` · beer ${probe.log_id}` : ""}</dd>
      {/if}
      <dt>Gravity</dt>
      <dd>{probe.header.gravity_enabled ? "yes" : "no sensor"}</dd>
      {#if probe.header.unknown_columns.length > 0}
        <dt>Unknown columns</dt>
        <dd class="warn">{probe.header.unknown_columns.join(", ")}</dd>
      {/if}
    </dl>

    <label>
      <span>Name</span>
      <input type="text" bind:value={name} />
    </label>
  {:else if stale}
    <p class="warn">URL changed — validate again.</p>
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
    {#if hints[error.code]}
      <p class="hint">{hints[error.code]}</p>
    {/if}
  {/if}
</section>

<style>
  section {
    max-width: 40rem;
    margin-top: 2rem;
  }

  ul {
    list-style: none;
    padding: 0;
    margin: 0 0 1rem;
  }

  li {
    display: grid;
    grid-template-columns: 10rem 1fr auto;
    gap: 0.25rem 1rem;
    align-items: baseline;
    padding: 0.4rem 0;
    border-bottom: 1px solid #2a2e36;
  }

  .path {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: #6f757d;
  }

  .age {
    color: #9aa0a6;
    font-variant-numeric: tabular-nums;
  }

  .err {
    grid-column: 1 / -1;
    color: #e06c5f;
  }

  .empty {
    color: #6f757d;
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

  input {
    padding: 0.35rem 0.5rem;
    background: #1e2127;
    color: inherit;
    border: 1px solid #333842;
    border-radius: 3px;
    font: inherit;
  }

  dl {
    display: grid;
    grid-template-columns: 10rem 1fr;
    gap: 0.25rem 1rem;
    margin: 0.75rem 0;
  }

  dt {
    color: #9aa0a6;
  }

  dd {
    margin: 0;
  }

  .actions {
    display: flex;
    gap: 0.75rem;
    margin: 1rem 0;
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

  .warn {
    color: #e0a35f;
  }

  .hint {
    color: #9aa0a6;
    margin-top: -0.5rem;
  }

  p[role="alert"] {
    color: #e06c5f;
  }
</style>
