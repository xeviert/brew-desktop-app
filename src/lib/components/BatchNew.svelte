<script lang="ts">
  import { onMount } from "svelte";
  import {
    createBatch,
    getRecipe,
    getRecipeVersion,
    listRecipes,
    listVessels,
    suggestBatchCode,
    toApiError,
    type ApiError,
    type RecipeSummary,
    type VersionTab,
    type Vessel,
  } from "$lib/api";
  import { GRAVITY_UNIT, toBatchInput, type BatchForm } from "$lib/batch";
  import { BEVERAGE_TYPES } from "$lib/recipe";
  import { href } from "$lib/route";
  import type { GravityDisplay } from "$lib/units";

  // `version` preselects a recipe version: the recipe screen's "Start a batch".
  let { version, gravity }: { version: number | null; gravity: GravityDisplay } = $props();

  let recipes = $state<RecipeSummary[]>([]);
  let vessels = $state<Vessel[]>([]);
  let versions = $state<VersionTab[]>([]);
  let recipeId = $state<number | null>(null);
  let form = $state<BatchForm>({
    code: "",
    recipe_version_id: null,
    beverage_type: "seltzer",
    vessel_id: null,
    volume: "",
    og: "",
    notes: "",
  });
  // Once the code is typed, a type change no longer replaces it.
  let codeTyped = $state(false);
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  const recipe = $derived(recipes.find((r) => r.id === recipeId) ?? null);
  const tab = $derived(versions.find((v) => v.id === form.recipe_version_id) ?? null);
  const vessel = $derived(vessels.find((v) => v.id === form.vessel_id) ?? null);

  onMount(() => {
    void (async () => {
      try {
        [recipes, vessels] = await Promise.all([listRecipes(false), listVessels()]);
        if (version !== null) {
          const v = await getRecipeVersion(version);
          await pickRecipe(v.recipe_id, v.id);
        } else {
          await suggest();
        }
      } catch (e) {
        error = toApiError(e);
      }
    })();
  });

  async function suggest() {
    if (!codeTyped) form.code = await suggestBatchCode(form.beverage_type);
  }

  /** The recipe decides the type, and its newest version is the default pick. */
  async function pickRecipe(id: number | null, versionId: number | null = null) {
    recipeId = id;
    versions = [];
    form.recipe_version_id = null;
    if (id !== null) {
      const detail = await getRecipe(id);
      versions = detail.versions;
      form.beverage_type = detail.recipe.beverage_type;
      await pickVersion(versionId ?? versions.at(-1)?.id ?? null);
    }
    await suggest();
  }

  /** The version's target volume prefills an untouched volume. */
  async function pickVersion(id: number | null) {
    form.recipe_version_id = id;
    if (id === null) return;
    const v = await getRecipeVersion(id);
    if (form.volume === "" && v.target_volume_l !== null) form.volume = String(v.target_volume_l);
  }

  async function onRecipe(e: Event) {
    const value = (e.currentTarget as HTMLSelectElement).value;
    error = null;
    try {
      await pickRecipe(value === "" ? null : Number(value));
    } catch (err) {
      error = toApiError(err);
    }
  }

  async function onVersion(e: Event) {
    error = null;
    try {
      await pickVersion(Number((e.currentTarget as HTMLSelectElement).value));
    } catch (err) {
      error = toApiError(err);
    }
  }

  async function onType() {
    try {
      await suggest();
    } catch (e) {
      error = toApiError(e);
    }
  }

  async function create() {
    const input = toBatchInput(form);
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    busy = true;
    error = null;
    try {
      const id = await createBatch(input.value);
      location.hash = href({ name: "batch", id });
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="ref">
  <p class="crumbs"><a href={href({ name: "batches" })}>Batches</a> /</p>
  <h2>New batch</h2>

  <label>
    <span>Recipe</span>
    <select value={recipeId ?? ""} onchange={onRecipe}>
      <option value="">No recipe</option>
      {#each recipes as r (r.id)}
        <option value={r.id}>{r.name}</option>
      {/each}
    </select>
  </label>
  {#if recipe}
    <label>
      <span>Version</span>
      <select value={form.recipe_version_id} onchange={onVersion}>
        {#each versions as v (v.id)}
          <option value={v.id}>v{v.version_no}{v.locked_at !== null ? " 🔒" : ""}</option>
        {/each}
      </select>
      {#if tab && tab.locked_at === null}
        <small>Creating the batch locks v{tab.version_no}: edits after that go to a fork.</small>
      {/if}
    </label>
  {/if}
  <label>
    <span>Beverage</span>
    <select bind:value={form.beverage_type} onchange={onType} disabled={recipe !== null}>
      {#each BEVERAGE_TYPES as t (t)}
        <option value={t}>{t}</option>
      {/each}
    </select>
    {#if recipe}<small>From the recipe.</small>{/if}
  </label>
  <label>
    <span>Code</span>
    <input type="text" bind:value={form.code} oninput={() => (codeTyped = true)} />
  </label>
  <label>
    <span>Vessel</span>
    <select bind:value={form.vessel_id}>
      <option value={null}>None</option>
      {#each vessels as v (v.id)}
        <option value={v.id}>{v.name}</option>
      {/each}
    </select>
    {#if vessel}
      <small
        >{vessel.default_streams.length === 0
          ? "No sensors mapped to this vessel: the pitch binds nothing."
          : `The pitch binds its ${vessel.default_streams.length} sensor stream${vessel.default_streams.length === 1 ? "" : "s"}.`}</small
      >
    {/if}
  </label>
  <label>
    <span>Volume (L)</span>
    <input type="text" inputmode="decimal" bind:value={form.volume} />
  </label>
  <label>
    <span>OG ({GRAVITY_UNIT[gravity]})</span>
    <input type="text" inputmode="decimal" bind:value={form.og} />
    <small>Optional: measure it at pitch instead.</small>
  </label>
  <label>
    <span>Notes</span>
    <input type="text" bind:value={form.notes} />
  </label>

  <div class="actions">
    <button onclick={create} disabled={busy || form.code.trim() === ""}>Create</button>
    <a href={href({ name: "batches" })}>Cancel</a>
  </div>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  section {
    max-width: 48rem;
  }

  .crumbs {
    margin: 0;
    color: var(--text-faint);
  }

  .crumbs a,
  .actions a {
    color: var(--accent-text);
  }
</style>
