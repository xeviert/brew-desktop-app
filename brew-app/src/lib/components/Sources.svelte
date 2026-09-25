<script lang="ts">
  import {
    addJournalSource,
    addSource,
    listJournals,
    listSources,
    probeJournal,
    probeLog,
    rebuild,
    resync,
    toApiError,
    type ApiError,
    type JournalEntry,
    type JournalProbe,
    type LogProbe,
    type SourceStatus,
    type SourceSummary,
  } from "$lib/api";
  import { age, bytes, until } from "$lib/format";

  let {
    sources,
    syncs,
    onchanged,
  }: {
    sources: SourceSummary[];
    syncs: Record<number, SourceStatus>;
    onchanged: (s: SourceSummary[]) => void;
  } = $props();

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
    "brewd.unconfigured": "Set the brewd URL in Settings first.",
    "brewd.not_brewd": "Check the brewd URL in Settings — that host is something else.",
    "brewd.not_a_journal": "brewd serves it, but it does not start with a time column.",
    "brewd.ispindel_unit_unset": "Set “iSpindle outputs” in Settings; nothing is skipped meanwhile.",
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

  // brewd lists its journals, so unlike Fermentrack there is something to pick from.
  let journals = $state<JournalEntry[] | null>(null);
  let journalProbe = $state<JournalProbe | null>(null);
  let journalName = $state("");
  let journalBusy = $state(false);

  // Already-added journals are hidden rather than greyed: adding one twice would give
  // two sources storing the same readings.
  const added = $derived(new Set(sources.map((s) => s.remote_path)));
  const offered = $derived(
    (journals ?? []).filter((j) => !added.has(`/journals/${j.name}`)),
  );

  /** `<device>.<epoch>.csv` is a journal brewd rotated away when its header changed. */
  const isRotated = (name: string) => /\.\d+\.csv$/.test(name);

  async function loadJournals() {
    journalBusy = true;
    error = null;
    journalProbe = null;
    try {
      journals = await listJournals();
    } catch (e) {
      error = toApiError(e);
    } finally {
      journalBusy = false;
    }
  }

  async function pickJournal(journal: string) {
    journalBusy = true;
    error = null;
    journalProbe = null;
    try {
      journalProbe = await probeJournal(journal);
      journalName = journalProbe.suggested_name;
    } catch (e) {
      error = toApiError(e);
    } finally {
      journalBusy = false;
    }
  }

  async function addJournal() {
    if (!journalProbe) return;
    journalBusy = true;
    error = null;
    try {
      const source = await addJournalSource(journalProbe.journal, journalName.trim());
      onchanged([...sources, source]);
      journalProbe = null;
      journalName = "";
    } catch (e) {
      error = toApiError(e);
    } finally {
      journalBusy = false;
    }
  }

  /**
   * Live state beats the stored row. A `SourceSummary` is a snapshot from whenever the
   * list was last fetched; a `SourceStatus` is the task as it is right now, and the two
   * disagree the moment a poll finishes.
   */
  function newest(source: SourceSummary): number | null {
    return syncs[source.id]?.newest_reading ?? source.newest_reading;
  }

  function fault(source: SourceSummary): string | null {
    const live = syncs[source.id];
    return live ? live.last_error : source.last_error;
  }

  const labels: Record<string, string> = {
    idle: "idle",
    syncing: "syncing…",
    error: "failing",
    unreachable: "unreachable",
  };

  async function doResync(id: number) {
    error = null;
    try {
      await resync(id);
    } catch (e) {
      error = toApiError(e);
    }
  }

  /**
   * Rebuild deletes every reading for the source and derives them again. Only safe
   * because Tier 1 is derived data — but "safe to redo" is not "free", so the count
   * goes in the prompt rather than a bare "are you sure".
   */
  async function doRebuild(source: SourceSummary) {
    const n = source.reading_count;
    const ok = confirm(
      `Rebuild “${source.name}”?

` +
        `This deletes ${n.toLocaleString()} reading${n === 1 ? "" : "s"} and re-reads the ` +
        `whole log from the beginning. Your brew logs and notes are not affected.`,
    );
    if (!ok) return;

    error = null;
    try {
      await rebuild(source.id);
      // The counts the next confirmation shows come from here, and a rebuild is the
      // one action that changes them out from under the list.
      onchanged(await listSources());
    } catch (e) {
      error = toApiError(e);
    }
  }
</script>

<section>
  <h2>Sources</h2>

  {#if sources.length === 0}
    <p class="empty">No logs yet. Paste one below.</p>
  {:else}
    <ul>
      {#each sources as source (source.id)}
        {@const live = syncs[source.id]}
        <li>
          <span class="name">
            <span class="dot {live?.phase ?? 'unknown'}" title={labels[live?.phase ?? ""] ?? "not polled"}
            ></span>
            {source.name}
          </span>
          <span class="path" title={source.remote_path ?? ""}>{source.remote_path ?? "—"}</span>
          <span class="age" title="age of the newest reading">{age(newest(source))}</span>
          <span class="next">
            {#if live?.phase === "syncing"}
              syncing…
            {:else if live}
              next {until(live.next_poll_at)}
            {:else}
              not polled
            {/if}
          </span>
          <span class="row-actions">
            <button class="minor" onclick={() => doResync(source.id)} disabled={!live}>
              Resync
            </button>
            <button class="minor" onclick={() => doRebuild(source)} disabled={!live}>
              Rebuild
            </button>
          </span>
          {#if fault(source)}
            <span class="err" role="alert">{fault(source)}</span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  <h3>Add from brewd</h3>
  <div class="actions">
    <button onclick={loadJournals} disabled={journalBusy}>
      {journalBusy && !journals ? "Loading…" : journals ? "Reload journals" : "Load journals"}
    </button>
  </div>

  {#if journals}
    {#if offered.length === 0}
      <p class="empty">
        {journals.length === 0 ? "brewd has no journals yet." : "Every journal is already added."}
      </p>
    {:else}
      <table>
        <tbody>
          {#each offered as journal (journal.name)}
            <tr class:picked={journalProbe?.journal === journal.name}>
              <td>
                {journal.name}
                {#if isRotated(journal.name)}<span class="tag">rotated</span>{/if}
              </td>
              <td class="num">{bytes(journal.bytes)}</td>
              <td class="num">{age(journal.modified)}</td>
              <td>
                <button class="minor" onclick={() => pickJournal(journal.name)} disabled={journalBusy}>
                  Pick
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}

  {#if journalProbe}
    <dl>
      <dt>Journal</dt>
      <dd>{journalProbe.journal}{journalProbe.rotated ? " · rotated, will not grow" : ""}</dd>
      <dt>Stores</dt>
      <dd>
        {journalProbe.header.mapped.map((c) => `${c.column} → ${c.metric}`).join(", ") || "nothing"}
      </dd>
      {#if journalProbe.header.unknown_columns.length > 0}
        <dt>Unknown columns</dt>
        <dd class="warn">{journalProbe.header.unknown_columns.join(", ")}</dd>
      {/if}
      {#if journalProbe.header.needs_ispindel_unit}
        <dt>Gravity</dt>
        <dd class="warn">waits for “iSpindle outputs” in Settings</dd>
      {/if}
    </dl>

    <label>
      <span>Name</span>
      <input type="text" bind:value={journalName} />
    </label>
    <div class="actions">
      <button onclick={addJournal} disabled={journalBusy}>Add</button>
    </div>
  {/if}

  <h3>Add a Fermentrack log</h3>
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
    grid-template-columns: 10rem 1fr auto auto auto;
    gap: 0.25rem 1rem;
    align-items: baseline;
    padding: 0.4rem 0;
    border-bottom: 1px solid #2a2e36;
  }

  .dot {
    display: inline-block;
    width: 0.5rem;
    height: 0.5rem;
    margin-right: 0.4rem;
    border-radius: 50%;
    background: #4a4f57;
  }

  .dot.idle {
    background: #5ba05b;
  }

  .dot.syncing {
    background: #5f9ae0;
  }

  .dot.error {
    background: #e06c5f;
  }

  .dot.unreachable {
    background: #e0a35f;
  }

  .next {
    color: #6f757d;
    font-variant-numeric: tabular-nums;
  }

  .row-actions {
    display: flex;
    gap: 0.4rem;
  }

  button.minor {
    padding: 0.15rem 0.5rem;
    background: transparent;
    color: #9aa0a6;
    border: 1px solid #333842;
    font-size: 0.85em;
  }

  button.minor:hover:not(:disabled) {
    color: #e6e8ea;
    border-color: #4a4f57;
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

  table {
    width: 100%;
    border-collapse: collapse;
    margin-bottom: 1rem;
  }

  td {
    padding: 0.3rem 0.5rem 0.3rem 0;
    border-bottom: 1px solid #2a2e36;
  }

  td.num {
    color: #9aa0a6;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  tr.picked td {
    color: #7ba7d4;
  }

  .tag {
    margin-left: 0.4rem;
    padding: 0 0.3rem;
    border: 1px solid #333842;
    border-radius: 3px;
    color: #9aa0a6;
    font-size: 0.8em;
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
