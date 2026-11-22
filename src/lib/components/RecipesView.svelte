<script lang="ts">
  import {
    archiveRecipe,
    createRecipe,
    listRecipes,
    restoreRecipe,
    toApiError,
    type ApiError,
    type BeverageType,
    type RecipeSummary,
  } from "$lib/api";
  import { BEVERAGE_TYPES } from "$lib/recipe";
  import { href } from "$lib/route";

  let recipes = $state<RecipeSummary[]>([]);
  let showArchived = $state(false);
  let error = $state<ApiError | null>(null);
  let adding = $state(false);
  let busy = $state(false);
  let name = $state("");
  let type = $state<BeverageType>("seltzer");

  const groups = $derived(
    BEVERAGE_TYPES.map((t) => ({ type: t, items: recipes.filter((r) => r.beverage_type === t) })).filter(
      (g) => g.items.length > 0,
    ),
  );

  async function load() {
    try {
      recipes = await listRecipes(showArchived);
    } catch (e) {
      error = toApiError(e);
    }
  }

  // Runs on mount, and again when the toggle flips.
  $effect(() => {
    void load();
  });

  async function create() {
    busy = true;
    error = null;
    try {
      const created = await createRecipe({ name: name.trim(), beverage_type: type, notes: null });
      // A new recipe's next step is its v1: straight there.
      location.hash = href({ name: "recipe", id: created.recipe_id, version: null });
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  function cancel() {
    adding = false;
    error = null;
  }

  async function setArchived(recipe: RecipeSummary, archived: boolean) {
    error = null;
    try {
      await (archived ? archiveRecipe(recipe.id) : restoreRecipe(recipe.id));
      await load();
    } catch (e) {
      error = toApiError(e);
    }
  }
</script>

<section class="ref">
  <nav class="bar">
    <h2>Recipes</h2>
    <label class="check">
      <input type="checkbox" bind:checked={showArchived} />
      <span>Show archived</span>
    </label>
  </nav>

  {#if groups.length === 0}
    <p class="empty">No recipes yet.</p>
  {/if}
  {#each groups as g (g.type)}
    <h3>{g.type}</h3>
    {#each g.items as r (r.id)}
      <div class="row" class:archived={r.archived_at !== null}>
        <a href={href({ name: "recipe", id: r.id, version: null })}>{r.name}</a>
        <span class="detail">
          {r.latest_version_no === null ? "no versions" : `v${r.latest_version_no}`}
          {r.version_count > 1 ? `· ${r.version_count} versions` : ""}
        </span>
        <span class="row-actions">
          {#if r.archived_at === null}
            <button class="link" onclick={() => setArchived(r, true)}>Archive</button>
          {:else}
            <button class="link" onclick={() => setArchived(r, false)}>Restore</button>
          {/if}
        </span>
      </div>
    {/each}
  {/each}

  {#if adding}
    <h3>New recipe</h3>
    <label>
      <span>Name</span>
      <input type="text" bind:value={name} placeholder="Citron-Verveine" />
    </label>
    <label>
      <span>Beverage</span>
      <select bind:value={type}>
        {#each BEVERAGE_TYPES as t (t)}
          <option value={t}>{t}</option>
        {/each}
      </select>
    </label>
    <div class="actions">
      <button onclick={create} disabled={busy || name.trim() === ""}>Create</button>
      <button class="link" onclick={cancel}>Cancel</button>
    </div>
  {:else}
    <div class="actions">
      <button onclick={() => (adding = true)}>Add a recipe</button>
    </div>
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  section {
    max-width: 48rem;
  }

  .bar {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  .bar .check {
    display: flex;
    gap: 0.4rem;
    margin: 0 0 0 auto;
  }

  h3 {
    margin: 1.25rem 0 0.25rem;
    color: #9aa0a6;
    font-weight: normal;
    font-size: 0.95rem;
    text-transform: capitalize;
  }

  .row {
    display: grid;
    grid-template-columns: 16rem 1fr auto;
    align-items: baseline;
    gap: 1rem;
    padding: 0.3rem 0;
    border-bottom: 1px solid #2a2e36;
  }

  .row a {
    color: inherit;
  }

  .row.archived a,
  .detail {
    color: #6f757d;
  }
</style>
