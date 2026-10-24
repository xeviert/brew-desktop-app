<script lang="ts">
  import {
    addRecipeItem,
    archiveRecipe,
    deleteRecipeItem,
    deleteRecipeVersion,
    forkRecipeVersion,
    getRecipe,
    getRecipeVersion,
    listIngredients,
    restoreRecipe,
    toApiError,
    updateRecipe,
    updateRecipeItem,
    updateRecipeVersion,
    type ApiError,
    type BeverageType,
    type Ingredient,
    type RecipeDetail,
    type RecipeItemView,
    type RecipeVersion,
  } from "$lib/api";
  import {
    BEVERAGE_TYPES,
    STAGES,
    byStage,
    formatOffset,
    itemForm,
    pickable,
    targetsForm,
    toItemInput,
    toVersionInput,
    type ItemForm,
    type TargetsForm,
  } from "$lib/recipe";
  import { formatQuantity, sameDimension, unitsLike } from "$lib/reference";
  import { confirmAction } from "$lib/confirm";
  import { href } from "$lib/route";
  import { formatTimestamp } from "$lib/units";

  let { id, version }: { id: number; version: number | null } = $props();

  let detail = $state<RecipeDetail | null>(null);
  let current = $state<RecipeVersion | null>(null);
  let ingredients = $state<Ingredient[]>([]);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);

  let editingRecipe = $state(false);
  let recipeName = $state("");
  let recipeType = $state<BeverageType>("seltzer");
  let recipeNotes = $state("");

  let targets = $state<TargetsForm>({ volume: "", og: "", fg: "", abv: "", notes: "" });
  /** The line being edited, `"new"` for the add form, or nothing. */
  let editingItem = $state<number | "new" | null>(null);
  let line = $state<ItemForm>(itemForm(null, "kg"));
  let compareWith = $state<number | null>(null);

  const tab = $derived(
    detail === null
      ? null
      : version === null
        ? (detail.versions.at(-1) ?? null)
        : (detail.versions.find((v) => v.version_no === version) ?? null),
  );
  const locked = $derived(current?.locked_at != null);
  const newest = $derived(detail !== null && tab !== null && tab.id === detail.versions.at(-1)?.id);
  const lineIngredient = $derived(ingredients.find((i) => i.id === line.ingredient_id) ?? null);
  const groups = $derived(current ? byStage(current.items) : []);
  const others = $derived(detail?.versions.filter((v) => v.id !== tab?.id) ?? []);
  const targetsDirty = $derived(current !== null && JSON.stringify(targets) !== JSON.stringify(targetsForm(current)));

  async function loadRecipe() {
    try {
      detail = await getRecipe(id);
      ingredients = await listIngredients(true);
    } catch (e) {
      error = toApiError(e);
    }
  }

  async function loadVersion(versionId: number) {
    try {
      current = await getRecipeVersion(versionId);
      targets = targetsForm(current);
    } catch (e) {
      error = toApiError(e);
    }
  }

  // The recipe reloads when the route's id changes; the version when the tab does.
  $effect(() => {
    void id;
    detail = null;
    current = null;
    void loadRecipe();
  });
  // On the id, not on `tab`: a reloaded `detail` makes a new `tab` object for the same
  // version, and rerunning here would wipe the error the reload was showing.
  const tabId = $derived(tab?.id);
  $effect(() => {
    const versionId = tabId;
    editingItem = null;
    error = null;
    compareWith = null;
    if (versionId !== undefined) void loadVersion(versionId);
  });

  /**
   * A write refused because the version locked under it (a batch was made off it
   * elsewhere) reloads, so the screen shows the badge and the fork instead.
   */
  async function attempt(write: () => Promise<unknown>, after?: () => void) {
    busy = true;
    error = null;
    try {
      await write();
      after?.();
      await loadRecipe();
      if (tab) await loadVersion(tab.id);
    } catch (e) {
      error = toApiError(e);
      if (error.code === "db.recipe_version_locked") {
        await loadRecipe();
        if (tab) await loadVersion(tab.id);
      }
    } finally {
      busy = false;
    }
  }

  function editRecipe() {
    if (!detail) return;
    recipeName = detail.recipe.name;
    recipeType = detail.recipe.beverage_type;
    recipeNotes = detail.recipe.notes ?? "";
    error = null;
    editingRecipe = true;
  }

  function saveRecipe() {
    void attempt(
      () =>
        updateRecipe(id, {
          name: recipeName.trim(),
          beverage_type: recipeType,
          notes: recipeNotes.trim() === "" ? null : recipeNotes.trim(),
        }),
      () => (editingRecipe = false),
    );
  }

  function setArchived(archived: boolean) {
    void attempt(() => (archived ? archiveRecipe(id) : restoreRecipe(id)));
  }

  function saveTargets() {
    if (!current) return;
    const input = toVersionInput($state.snapshot(targets));
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    const versionId = current.id;
    void attempt(() => updateRecipeVersion(versionId, input.value));
  }

  async function fork() {
    if (!current) return;
    busy = true;
    error = null;
    try {
      const forked = await forkRecipeVersion(current.id);
      location.hash = href({ name: "recipe", id, version: forked.version_no });
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  async function deleteDraft() {
    if (!current) return;
    const n = current.items.length;
    const ok = await confirmAction(
      `Delete draft v${current.version_no} and its ${n} line${n === 1 ? "" : "s"}?`,
      "Delete",
    );
    if (!ok) return;
    busy = true;
    error = null;
    try {
      await deleteRecipeVersion(current.id);
      location.hash = href({ name: "recipe", id, version: null });
      await loadRecipe();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  function editLine(item: RecipeItemView | null) {
    error = null;
    line = itemForm(item, "kg");
    editingItem = item?.id ?? "new";
  }

  // A line's unit follows its ingredient: a new line starts in the ingredient's unit,
  // and switching sucrose for DAP on an existing one moves kg to g.
  function pickIngredient() {
    if (!lineIngredient) return;
    if (editingItem === "new" || !sameDimension(line.unit, lineIngredient.default_unit)) {
      line.unit = lineIngredient.default_unit;
    }
  }

  function saveLine() {
    if (!current) return;
    const input = toItemInput($state.snapshot(line));
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    const versionId = current.id;
    const target = editingItem;
    void attempt(
      () =>
        target === "new" ? addRecipeItem(versionId, input.value) : updateRecipeItem(target as number, input.value),
      () => (editingItem = null),
    );
  }

  function removeLine(item: RecipeItemView) {
    void attempt(() => deleteRecipeItem(item.id));
  }

  function cancel() {
    editingItem = null;
    editingRecipe = false;
    error = null;
  }

  function compare() {
    if (compareWith === null || !tab) return;
    const [from, to] = [compareWith, tab.version_no].sort((a, b) => a - b);
    location.hash = href({ name: "recipe-diff", id, from, to });
  }

  const pct = (f: number) => `${Number((f * 100).toFixed(2))} %`;
</script>

<section class="ref">
  <p class="crumbs"><a href={href({ name: "recipes" })}>Recipes</a></p>

  {#if detail}
    {#if editingRecipe}
      <h2>Edit recipe</h2>
      <label>
        <span>Name</span>
        <input type="text" bind:value={recipeName} />
      </label>
      <label>
        <span>Beverage</span>
        <select bind:value={recipeType}>
          {#each BEVERAGE_TYPES as t (t)}
            <option value={t}>{t}</option>
          {/each}
        </select>
        <small>fixed once a batch has been made from any version</small>
      </label>
      <label>
        <span>Notes</span>
        <input type="text" bind:value={recipeNotes} />
      </label>
      <div class="actions">
        <button onclick={saveRecipe} disabled={busy || recipeName.trim() === ""}>Save</button>
        <button class="link" onclick={cancel}>Cancel</button>
      </div>
    {:else}
      <header>
        <h2 class:archived={detail.recipe.archived_at !== null}>{detail.recipe.name}</h2>
        <span class="detail">{detail.recipe.beverage_type}{detail.recipe.archived_at !== null ? " · archived" : ""}</span>
        <span class="row-actions">
          <button class="link" onclick={editRecipe}>Edit</button>
          {#if detail.recipe.archived_at === null}
            <button class="link" onclick={() => setArchived(true)}>Archive</button>
          {:else}
            <button class="link" onclick={() => setArchived(false)}>Restore</button>
          {/if}
        </span>
      </header>
      {#if detail.recipe.notes}<p class="detail">{detail.recipe.notes}</p>{/if}
    {/if}

    <nav class="tabs">
      {#each detail.versions as v (v.id)}
        <a href={href({ name: "recipe", id, version: v.version_no })} class:active={tab?.id === v.id}
          >v{v.version_no}{v.locked_at !== null ? " 🔒" : ""}</a
        >
      {/each}
    </nav>
  {/if}

  {#if current && tab && current.id === tab.id}
    {#if locked}
      <div class="badge">
        <span>
          Locked {current.locked_at !== null ? formatTimestamp(current.locked_at, { dateStyle: "medium" }) : ""} ·
          {#if current.batch_count > 0}
            <a href={href({ name: "query", id: null, preset: { version: current.id } })}
              >used by {current.batch_count} batch{current.batch_count === 1 ? "" : "es"}</a
            >
          {:else}
            used by a batch
          {/if}
        </span>
        <a href={href({ name: "batch-new", version: current.id })}>Start a batch from v{current.version_no}</a>
        <button onclick={fork} disabled={busy}>Fork to v{(detail?.versions.at(-1)?.version_no ?? 0) + 1}</button>
      </div>
      <p class="hint">A version a batch was made from never changes. Edit a fork instead.</p>
    {/if}

    <h3>Targets</h3>
    {#if locked}
      <dl>
        <dt>Volume</dt>
        <dd>{current.target_volume_l === null ? "—" : `${current.target_volume_l} L`}</dd>
        <dt>OG</dt>
        <dd>{current.target_og === null ? "—" : `${targets.og} ${current.gravity_unit}`}</dd>
        <dt>FG</dt>
        <dd>{current.target_fg === null ? "—" : `${targets.fg} ${current.gravity_unit}`}</dd>
        <dt>ABV</dt>
        <dd>{current.target_abv === null ? "—" : pct(current.target_abv)}</dd>
        {#if current.notes}
          <dt>Notes</dt>
          <dd>{current.notes}</dd>
        {/if}
      </dl>
    {:else}
      <label>
        <span>Volume (L)</span>
        <input type="text" inputmode="decimal" bind:value={targets.volume} />
      </label>
      <label>
        <span>OG ({current.gravity_unit})</span>
        <input type="text" inputmode="decimal" bind:value={targets.og} />
      </label>
      <label>
        <span>FG ({current.gravity_unit})</span>
        <input type="text" inputmode="decimal" bind:value={targets.fg} />
      </label>
      <label>
        <span>ABV (%)</span>
        <input type="text" inputmode="decimal" bind:value={targets.abv} />
      </label>
      <label>
        <span>Notes</span>
        <input type="text" bind:value={targets.notes} />
      </label>
      <div class="actions">
        <button onclick={saveTargets} disabled={busy || !targetsDirty}>Save targets</button>
      </div>
    {/if}
    {#if current.abv_alternate !== null}
      <p class="hint">From target OG and FG: {pct(current.abv_alternate)} ABV (alternate formula)</p>
    {/if}

    <h3>Lines</h3>
    {#if groups.length === 0}
      <p class="empty">No lines yet.</p>
    {/if}
    <table>
      <tbody>
        {#each groups as g (g.stage)}
          <tr class="stage"><th colspan="5">{g.stage}</th></tr>
          {#each g.items as item (item.id)}
            <tr>
              <td>
                {item.ingredient}
                {#if item.ingredient_archived}<small>archived</small>{/if}
              </td>
              <td>{formatQuantity(item.entry)}</td>
              <td>{formatOffset(item.offset_hours)}</td>
              <td class="detail">{item.notes ?? ""}</td>
              <td class="row-actions">
                {#if !locked}
                  <button class="link" onclick={() => editLine(item)}>Edit</button>
                  <button class="link" onclick={() => removeLine(item)}>Delete</button>
                {/if}
              </td>
            </tr>
          {/each}
        {/each}
      </tbody>
    </table>

    {#if !locked}
      {#if editingItem !== null}
        <h4>{editingItem === "new" ? "New line" : "Edit line"}</h4>
        <label>
          <span>Ingredient</span>
          <select bind:value={line.ingredient_id} onchange={pickIngredient}>
            <option value={null}>— pick one —</option>
            {#each pickable(ingredients, line.ingredient_id) as i (i.id)}
              <option value={i.id}>{i.name}{i.archived_at !== null ? " (archived)" : ""}</option>
            {/each}
          </select>
          {#if ingredients.length === 0}
            <small>none yet — add them under <a href={href({ name: "ingredients", tab: "ingredients" })}>Ingredients</a></small>
          {/if}
        </label>
        <label>
          <span>Quantity</span>
          <div class="pair">
            <input type="text" inputmode="decimal" bind:value={line.qty} />
            <select bind:value={line.unit}>
              {#each unitsLike(lineIngredient?.default_unit ?? line.unit) as u (u)}
                <option value={u}>{u}</option>
              {/each}
            </select>
          </div>
        </label>
        <label>
          <span>Stage</span>
          <select bind:value={line.stage}>
            {#each STAGES as s (s)}
              <option value={s}>{s}</option>
            {/each}
          </select>
        </label>
        <label>
          <span>Hours from pitch</span>
          <input type="text" inputmode="decimal" bind:value={line.offset} />
          <small>blank if not timed; negative is before pitch</small>
        </label>
        <label>
          <span>Notes</span>
          <input type="text" bind:value={line.notes} />
        </label>
        <div class="actions">
          <button onclick={saveLine} disabled={busy}>Save line</button>
          <button class="link" onclick={cancel}>Cancel</button>
        </div>
      {:else}
        <div class="actions">
          <button onclick={() => editLine(null)}>Add a line</button>
          <a class="start" href={href({ name: "batch-new", version: current.id })}
            >Start a batch from v{current.version_no}</a
          >
          {#if newest && (detail?.versions.length ?? 0) > 1}
            <button class="link" onclick={deleteDraft} disabled={busy}>Delete draft v{current.version_no}</button>
          {/if}
        </div>
      {/if}
    {/if}

    {#if others.length > 0}
      <div class="actions compare">
        <span class="detail">Compare v{current.version_no} with</span>
        <select bind:value={compareWith}>
          <option value={null}>—</option>
          {#each others as v (v.id)}
            <option value={v.version_no}>v{v.version_no}</option>
          {/each}
        </select>
        <button onclick={compare} disabled={compareWith === null}>Compare</button>
      </div>
    {/if}
  {:else if detail && !tab}
    <p class="empty">No such version.</p>
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  section {
    max-width: 52rem;
  }

  .crumbs {
    margin: 0;
  }

  .crumbs a,
  .badge a,
  .start {
    color: #7ba7d4;
  }

  header {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  h2.archived {
    color: #6f757d;
  }

  .detail,
  .hint,
  td small {
    color: #6f757d;
  }

  .tabs {
    display: flex;
    gap: 1rem;
    margin: 0.75rem 0 1rem;
    border-bottom: 1px solid #2a2e36;
  }

  .tabs a {
    padding-bottom: 0.3rem;
    color: #9aa0a6;
    text-decoration: none;
  }

  .tabs a.active {
    color: inherit;
    border-bottom: 2px solid #2d5d8a;
  }

  .badge {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.5rem 0.75rem;
    background: #1e2127;
    border-left: 3px solid #e0a35f;
  }

  h3 {
    margin: 1.25rem 0 0.5rem;
  }

  table {
    border-collapse: collapse;
  }

  td,
  th {
    padding: 0.25rem 1.25rem 0.25rem 0;
    text-align: left;
    font-variant-numeric: tabular-nums;
  }

  tr.stage th {
    padding-top: 0.6rem;
    color: #9aa0a6;
    font-weight: normal;
    text-transform: capitalize;
  }

  .pair {
    display: flex;
    gap: 0.5rem;
  }

  .pair input {
    flex: 1;
  }

  .compare {
    margin-top: 1.5rem;
  }
</style>
