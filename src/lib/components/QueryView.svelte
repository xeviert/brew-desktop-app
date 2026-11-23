<script lang="ts">
  import { onMount, untrack } from "svelte";
  import {
    archiveSavedQuery,
    createSavedQuery,
    exportQuery,
    listSavedQueries,
    onSyncStatus,
    queryOptions,
    runBatchQuery,
    runSavedQuery,
    toApiError,
    updateSavedQuery,
    type ApiError,
    type BatchFilter,
    type BatchStatus,
    type Computed,
    type Config,
    type Criterion,
    type Missing,
    type QueryOptions,
    type QueryRow,
    type QueryView,
    type SavedQuery,
    type SavedQuerySummary,
  } from "$lib/api";
  import { formatGravityIn, span } from "$lib/dashboard";
  import { csvName, saveCsv } from "$lib/export";
  import {
    MAX_OVERLAY,
    emptyFilter,
    preselect,
    sortRows,
    toFilter,
    toForm,
    type FilterForm,
    type SortKey,
  } from "$lib/query";
  import { BEVERAGE_TYPES } from "$lib/recipe";
  import { href, type QueryPreset } from "$lib/route";
  import { formatMoney, formatTemp, formatTimestamp } from "$lib/units";
  import RollupTable from "./RollupTable.svelte";

  // `preset`: a fresh query started from a lot (U1) or a recipe version (U2).
  let { id, preset, ui }: { id: number | null; preset?: QueryPreset; ui: Config["ui"] } = $props();

  // Started here, not in `onMount`, so a version preset can wait for it in `load`.
  const optionsLoaded = queryOptions();

  const TYPES = BEVERAGE_TYPES;
  const STATUSES: BatchStatus[] = ["planned", "fermenting", "conditioning", "packaged"];

  let options = $state.raw<QueryOptions>({ recipes: [], strains: [], lots: [] });
  let saved = $state.raw<SavedQuerySummary[]>([]);
  /** The saved query `id` points at, as stored: the form is dirty when it differs. */
  let open = $state.raw<SavedQuery | null>(null);
  /** The unit the form's temperatures were typed in; trails `ui` until converted. */
  let formTemp = $state(untrack(() => ui.temp_display));
  let form = $state<FilterForm>(toForm(emptyFilter(), untrack(() => formTemp)));
  let name = $state("");
  let view = $state.raw<QueryView | null>(null);
  let error = $state<ApiError | null>(null);
  let saveError = $state<ApiError | null>(null);
  let picked = $state<number[]>([]);
  let sortKey = $state<SortKey>("code");
  let desc = $state(false);
  let mode = $state<"batches" | "strains">("batches");

  const parsed = $derived(toFilter(form, formTemp));
  const dirty = $derived(
    open !== null && parsed.ok && JSON.stringify(parsed.filter) !== JSON.stringify(open.filter),
  );
  const versions = $derived(options.recipes.find((r) => r.id === form.recipe_id)?.versions ?? []);
  const rows = $derived(view ? sortRows(view.rows, sortKey, desc) : []);
  const codes = $derived(new Map(view?.rows.map((r) => [r.batch_id, r.code])));
  const filtered = $derived(
    JSON.stringify(parsed) !== JSON.stringify(toFilter(toForm(emptyFilter(), formTemp), formTemp)),
  );

  function clearFilters() {
    form = toForm(emptyFilter(), formTemp);
  }

  const picks = (n: number) => (n === 0 ? "any" : `${n} picked`);

  let busy = false;
  let again = false;
  // Not state: which filter the table shows, so a re-render of the same one is no fetch.
  let shown = "";

  async function refresh(force = false) {
    if (!parsed.ok) return;
    const filter: BatchFilter = parsed.filter;
    const key = JSON.stringify(filter);
    if (key === shown && !force) return;
    if (busy) {
      again = true;
      return;
    }
    busy = true;
    try {
      const next = await runBatchQuery(filter);
      const fresh = key !== shown;
      view = next;
      shown = key;
      error = null;
      if (fresh) picked = preselect(next.rows);
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

  async function load(queryId: number | null, from: QueryPreset | undefined) {
    saveError = null;
    if (queryId === null) {
      open = null;
      name = "";
      const filter = emptyFilter();
      if (from && "lot" in from) filter.lot_ids = [from.lot];
      if (from && "version" in from) {
        // The version picker is scoped to a recipe, so the recipe comes along.
        const loaded = await optionsLoaded.catch(() => null);
        const recipe = loaded?.recipes.find((r) => r.versions.some((v) => v.id === from.version));
        filter.recipe_id = recipe?.id ?? null;
        filter.recipe_version_id = from.version;
      }
      form = toForm(filter, formTemp);
      return;
    }
    try {
      const run = await runSavedQuery(queryId);
      open = run.query;
      name = run.query.name;
      form = toForm(run.query.filter, formTemp);
      view = run.result;
      shown = JSON.stringify(run.query.filter);
      picked = preselect(run.result.rows);
      error = null;
    } catch (e) {
      open = null;
      view = null;
      error = toApiError(e);
    }
  }

  async function listSaved() {
    try {
      saved = await listSavedQueries();
    } catch (e) {
      error = toApiError(e);
    }
  }

  async function saveNew() {
    if (!parsed.ok) return;
    try {
      const q = await createSavedQuery(name, parsed.filter);
      saveError = null;
      await listSaved();
      location.hash = href({ name: "query", id: q.id });
    } catch (e) {
      saveError = toApiError(e);
    }
  }

  async function update() {
    if (!parsed.ok || open === null) return;
    try {
      open = await updateSavedQuery(open.id, name, parsed.filter);
      name = open.name;
      saveError = null;
      await listSaved();
    } catch (e) {
      saveError = toApiError(e);
    }
  }

  async function archive() {
    if (open === null) return;
    try {
      await archiveSavedQuery(open.id);
      await listSaved();
      location.hash = href({ name: "query", id: null });
    } catch (e) {
      saveError = toApiError(e);
    }
  }

  /** The filter the table shows, not the form: a half-typed edit isn't what was seen. */
  async function exportCsv() {
    if (!shown) return;
    const filter: BatchFilter = JSON.parse(shown);
    const table = mode;
    const what = table === "strains" ? "by strain" : "batches";
    try {
      await saveCsv(csvName([open?.name ?? "query", what]), (path) =>
        exportQuery(filter, table, path),
      );
    } catch (e) {
      error = toApiError(e);
    }
  }

  function pickRecipe() {
    if (!versions.some((v) => v.id === form.recipe_version_id)) form.recipe_version_id = null;
  }

  function sortBy(key: SortKey) {
    desc = sortKey === key ? !desc : false;
    sortKey = key;
  }

  function togglePick(batchId: number) {
    picked = picked.includes(batchId) ? picked.filter((x) => x !== batchId) : [...picked, batchId];
  }

  const REASONS: Partial<Record<Missing, string>> = {
    no_pitch: "not pitched",
    no_og: "no OG recorded",
    no_fg: "no FG recorded",
    no_temp: "no temperature logged",
    no_yeast: "no yeast addition recorded",
  };
  const reason = (m: Missing) => REASONS[m] ?? m.replaceAll("_", " ");

  const CRITERIA: Record<Criterion, string> = {
    og: "OG",
    fg: "FG",
    pitched: "pitch date",
    strain: "strain",
    ferm_temp: "ferment temperature",
  };

  /** A metric, or a dash that says why on hover. */
  function cell<T>(c: Computed<T>, f: (v: T) => string): { text: string; why: string | null } {
    return "value" in c ? { text: f(c.value), why: null } : { text: "—", why: reason(c.missing) };
  }

  const pct = (v: number) => `${(v * 100).toFixed(1)} %`;

  const COLUMNS: [SortKey, string][] = [
    ["code", "Batch"],
    ["pitched_at", "Pitched"],
    ["og", "OG"],
    ["fg", "FG"],
    ["lag", "Lag"],
    ["peak_rate", "Peak rate"],
    ["terminal", "To terminal"],
    ["attenuation", "App. atten."],
    ["abv", "ABV (Hall)"],
    ["ferm_temp", "Temp"],
    ["yan", "YAN added"],
    ["cost", "Cost / L"],
  ];

  function cells(r: QueryRow): { text: string; why: string | null }[] {
    const unit = view?.gravity_unit ?? "SG";
    const gravity = (v: number | null, m: Missing) =>
      v === null ? { text: "—", why: reason(m) } : { text: formatGravityIn(v, unit), why: null };
    return [
      {
        text: r.pitched_at === null ? "—" : formatTimestamp(r.pitched_at, { dateStyle: "medium" }),
        why: r.pitched_at === null ? reason("no_pitch") : null,
      },
      gravity(r.display_og, "no_og"),
      gravity(r.display_fg, "no_fg"),
      cell(r.lag, (v) => span(v.secs)),
      cell(r.peak_rate, (v) => `${(v.sg_per_day * 1000).toFixed(1)} pt/d`),
      cell(r.terminal, (v) => span(v.secs_from_pitch) + (v.fg_unchecked ? " ?" : "")),
      cell(r.apparent_attenuation, pct),
      cell(r.abv_alternate, pct),
      cell(r.ferm_temp, (v) => formatTemp(v.median, ui.temp_display)),
      cell(r.yan_added_mg_per_l, (v) => `${v.toFixed(0)} mg/L`),
      cell(r.cost_per_litre, (v) => formatMoney(v) + (r.cost_lines_without_cost ? " *" : "")),
    ];
  }

  $effect(() => {
    const queryId = id;
    const from = preset;
    untrack(() => void load(queryId, from));
  });
  // Every edit reruns, as the comparison does; `shown` drops the no-op ones.
  $effect(() => {
    void JSON.stringify(parsed);
    untrack(() => void refresh());
  });
  // Temperature is typed in the display unit, so the form follows a change to it.
  $effect(() => {
    const temp = ui.temp_display;
    untrack(() => {
      if (temp === formTemp || !parsed.ok) return;
      form = toForm(parsed.filter, temp);
      formTemp = temp;
    });
  });

  onMount(() => {
    void listSaved();
    void (async () => {
      try {
        options = await optionsLoaded;
      } catch (e) {
        error = toApiError(e);
      }
    })();
    return onSyncStatus((s) => {
      if (s.rows_added > 0) void refresh(true);
    });
  });
</script>

<section>
  <h2>Find batches</h2>
  <p class="hint intro">
    Narrow down your batches with the filters, then compare their numbers, overlay their
    curves, or export them. Save a filter to rerun it later.
  </p>

  {#if saved.length}
    <nav class="saved">
      <span class="hint">Saved filters:</span>
      <a href={href({ name: "query", id: null })} class:active={id === null}>none</a>
      {#each saved as q (q.id)}
        <a href={href({ name: "query", id: q.id })} class:active={id === q.id}>{q.name}</a>
      {/each}
    </nav>
  {/if}

  <form class="filter" onsubmit={(e) => e.preventDefault()}>
    <fieldset>
      <legend>Type</legend>
      {#each TYPES as t (t)}
        <label><input type="checkbox" value={t} bind:group={form.beverage_types} /> {t}</label>
      {/each}
    </fieldset>
    <fieldset>
      <legend>Status</legend>
      {#each STATUSES as s (s)}
        <label><input type="checkbox" value={s} bind:group={form.statuses} /> {s}</label>
      {/each}
      <label><input type="checkbox" bind:checked={form.include_archived} /> archived</label>
    </fieldset>
    <fieldset>
      <legend>Recipe</legend>
      <select bind:value={form.recipe_id} onchange={pickRecipe}>
        <option value={null}>any</option>
        {#each options.recipes as r (r.id)}
          <option value={r.id}>{r.name}{r.archived ? " (archived)" : ""}</option>
        {/each}
      </select>
      <select bind:value={form.recipe_version_id} disabled={form.recipe_id === null}>
        <option value={null}>any version</option>
        {#each versions as v (v.id)}
          <option value={v.id}>v{v.version_no}</option>
        {/each}
      </select>
    </fieldset>
    <fieldset class="ranges">
      <legend>Ranges <span class="hint">(blank = no limit)</span></legend>
      <span>OG</span>
      <span><input bind:value={form.og_min} placeholder="min" /> – <input bind:value={form.og_max} placeholder="max" /> SG</span>
      <span>FG</span>
      <span><input bind:value={form.fg_min} placeholder="min" /> – <input bind:value={form.fg_max} placeholder="max" /> SG</span>
      <span>Ferment temp</span>
      <span>
        <input bind:value={form.temp_min} placeholder="min" /> –
        <input bind:value={form.temp_max} placeholder="max" /> °{formTemp}
      </span>
      <span>Pitched</span>
      <span><input type="date" bind:value={form.pitched_from} /> – <input type="date" bind:value={form.pitched_to} /></span>
    </fieldset>
    <details class="pick">
      <summary>Yeast strain <span class="hint">{picks(form.strain_ids.length)}</span></summary>
      {#each options.strains as s (s.id)}
        <label><input type="checkbox" value={s.id} bind:group={form.strain_ids} /> {s.name}</label>
      {:else}
        <span class="hint">No strains yet.</span>
      {/each}
    </details>
    <details class="pick">
      <summary>Ingredient lot <span class="hint">{picks(form.lot_ids.length)}</span></summary>
      {#each options.lots as l (l.id)}
        <label>
          <input type="checkbox" value={l.id} bind:group={form.lot_ids} />
          {l.ingredient}
          <span class="hint">
            {[l.lot_code, l.supplier].filter(Boolean).join(" · ")}
            · {l.batch_count} batch{l.batch_count === 1 ? "" : "es"}
          </span>
        </label>
      {:else}
        <span class="hint">No lot used in a batch yet.</span>
      {/each}
    </details>
  </form>
  {#if filtered}
    <button class="link" onclick={clearFilters}>Clear filters</button>
  {/if}

  {#if !parsed.ok}
    <p role="alert">{parsed.error}</p>
  {:else if error}
    <p role="alert">{error.message}</p>
  {/if}

  {#if view}
    <div class="toolbar results">
      <strong>{view.rows.length} batch{view.rows.length === 1 ? "" : "es"} match</strong>
      <nav class="modes">
        <button class:active={mode === "batches"} onclick={() => (mode = "batches")}>Each batch</button>
        <button class:active={mode === "strains"} onclick={() => (mode = "strains")}>Averages by strain</button>
      </nav>
      <span class="spacer"></span>
      {#if mode === "batches"}
        <button
          disabled={picked.length < 2 || picked.length > MAX_OVERLAY}
          title="Tick 2 to {MAX_OVERLAY} batches to draw their curves on one chart"
          onclick={() => (location.hash = href({ name: "compare", ids: picked }))}
        >
          Overlay curves ({picked.length})
        </button>
      {/if}
      <button title="Save this table as CSV" onclick={exportCsv}>Export CSV</button>
    </div>

    {#if mode === "strains"}
      <RollupTable rollup={view.rollup} {codes} unevaluated={view.unevaluated.length} {ui} />
    {:else if view.rows.length}
      <table>
        <thead>
          <tr>
            <th title="Tick to overlay"></th>
            {#each COLUMNS as [key, label] (key)}
              <th>
                <button class="sort" onclick={() => sortBy(key)}>
                  {label}{sortKey === key ? (desc ? " ↓" : " ↑") : ""}
                </button>
              </th>
            {/each}
            <th>Strain</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as r (r.batch_id)}
            <tr class:archived={r.archived_at !== null}>
              <td>
                <input
                  type="checkbox"
                  checked={picked.includes(r.batch_id)}
                  disabled={r.pitched_at === null}
                  title={r.pitched_at === null ? "Not pitched: nothing to overlay" : undefined}
                  onchange={() => togglePick(r.batch_id)}
                />
              </td>
              <td><a href={href({ name: "batch", id: r.batch_id })}>{r.code}</a></td>
              {#each cells(r) as c, i (i)}
                <td class="num" class:missing={c.why !== null} title={c.why}>{c.text}</td>
              {/each}
              <td>{r.strains.map((s) => s.name).join(", ") || "—"}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <p class="empty">No batch matches.</p>
    {/if}

    {#if mode === "batches" && view.unevaluated.length}
      <p class="hint">Not listed above — missing the data a filter checks:</p>
      <ul class="unevaluated">
        {#each view.unevaluated as u (u.batch_id)}
          <li>
            <a href={href({ name: "batch", id: u.batch_id })}>{u.code}</a>
            <span class="hint">{CRITERIA[u.criterion]}: {reason(u.reason)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  <div class="toolbar save">
    <input placeholder="Name this filter" bind:value={name} />
    {#if open}
      <button onclick={update} disabled={!parsed.ok || (!dirty && name.trim() === open.name)}>Update</button>
      <button onclick={saveNew} disabled={!parsed.ok}>Save as new</button>
      <button onclick={archive}>Archive</button>
    {:else}
      <button onclick={saveNew} disabled={!parsed.ok || name.trim() === ""}>Save filter</button>
    {/if}
    {#if saveError}<span role="alert">{saveError.message}</span>{/if}
  </div>
</section>

<style>
  .saved {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 1rem;
    margin-bottom: 0.75rem;
  }

  .saved a {
    color: var(--text-muted);
    text-decoration: none;
  }

  .saved a.active {
    color: var(--text);
    border-bottom: 2px solid var(--accent);
  }

  .toolbar {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.5rem;
  }

  .intro {
    margin: 0 0 0.75rem;
    max-width: 40rem;
  }

  .results {
    margin-top: 1rem;
    flex-wrap: wrap;
  }

  .spacer {
    flex: 1;
  }

  .modes button {
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-muted);
    padding: 0.2rem 0;
    font: inherit;
    cursor: pointer;
  }

  .modes button.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }

  .save {
    margin-top: 1.5rem;
    padding-top: 0.75rem;
    border-top: 1px solid var(--border);
  }

  button.link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent-text);
    font: inherit;
    cursor: pointer;
  }

  .pick {
    flex-basis: 100%;
    border: 1px solid var(--border-strong);
    padding: 0.35rem 0.75rem;
  }

  .pick summary {
    cursor: pointer;
  }

  .pick[open] {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 0.75rem;
  }

  .pick[open] summary {
    flex-basis: 100%;
    margin-bottom: 0.25rem;
  }

  .modes {
    display: flex;
    gap: 0.5rem;
  }

  .filter {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
  }

  fieldset {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 0.75rem;
    border: 1px solid var(--border-strong);
    margin: 0;
  }

  .ranges {
    display: grid;
    grid-template-columns: auto auto;
    align-items: center;
  }

  .ranges input:not([type="date"]) {
    width: 5rem;
  }

  .hint,
  .empty {
    color: var(--text-faint);
  }

  table {
    border-collapse: collapse;
    width: 100%;
    margin-bottom: 0.75rem;
  }

  th,
  td {
    padding: 0.25rem 0.5rem;
    border-bottom: 1px solid var(--border-strong);
    text-align: left;
    white-space: nowrap;
  }

  td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  td.missing {
    color: var(--text-faint);
    cursor: help;
  }

  tr.archived td {
    color: var(--text-muted);
  }

  .sort {
    background: none;
    border: none;
    color: var(--text-muted);
    padding: 0;
    font: inherit;
    cursor: pointer;
  }

  td a,
  .unevaluated a {
    color: var(--text);
  }

  .unevaluated {
    list-style: none;
    padding: 0;
    margin: 0;
  }
</style>
