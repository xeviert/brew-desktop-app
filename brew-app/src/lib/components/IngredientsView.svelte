<script lang="ts">
  import { onMount } from "svelte";
  import {
    archiveIngredient,
    createIngredient,
    listIngredients,
    listYeastStrains,
    restoreIngredient,
    toApiError,
    updateIngredient,
    yanPresets,
    type ApiError,
    type Config,
    type Ingredient,
    type YanPreset,
    type YeastStrain,
  } from "$lib/api";
  import { CATEGORIES, ingredientForm, toIngredientInput, type IngredientForm } from "$lib/reference";
  import { href, type IngredientsTab } from "$lib/route";
  import IngredientFields from "./IngredientFields.svelte";
  import LotsPanel from "./LotsPanel.svelte";
  import StrainsTab from "./StrainsTab.svelte";

  let { tab, config }: { tab: IngredientsTab; config: Config } = $props();

  let ingredients = $state<Ingredient[]>([]);
  let strains = $state<YeastStrain[]>([]);
  let presets = $state<YanPreset[]>([]);
  let showArchived = $state(false);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);

  /** The ingredient being edited, `"new"` for the add form, or nothing. */
  let editing = $state<number | "new" | null>(null);
  let form = $state<IngredientForm>(ingredientForm(null));
  let open = $state<Set<number>>(new Set());

  const groups = $derived(
    CATEGORIES.map((c) => ({ ...c, items: ingredients.filter((i) => i.category === c.value) })).filter(
      (g) => g.items.length > 0,
    ),
  );

  async function loadIngredients() {
    try {
      ingredients = await listIngredients(showArchived);
    } catch (e) {
      error = toApiError(e);
    }
  }

  // Every strain, archived too: a yeast pitched last year still names its strain.
  async function loadStrains() {
    try {
      strains = await listYeastStrains(true);
    } catch (e) {
      error = toApiError(e);
    }
  }

  onMount(() => {
    void loadStrains();
    void (async () => {
      try {
        presets = await yanPresets();
      } catch (e) {
        error = toApiError(e);
      }
    })();
  });

  $effect(() => {
    void loadIngredients();
  });

  function edit(ingredient: Ingredient | null) {
    error = null;
    form = ingredientForm(ingredient);
    editing = ingredient?.id ?? "new";
  }

  // A refusal belongs to the form it came from; closing the form drops it.
  function cancel() {
    editing = null;
    error = null;
  }

  async function save() {
    const input = toIngredientInput($state.snapshot(form));
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    busy = true;
    error = null;
    try {
      if (editing === "new") {
        const id = await createIngredient(input.value);
        // A new ingredient's next step is nearly always its first lot.
        open = new Set([...open, id]);
      } else if (editing !== null) {
        await updateIngredient(editing, input.value);
      }
      editing = null;
      await loadIngredients();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  async function setArchived(ingredient: Ingredient, archived: boolean) {
    error = null;
    try {
      await (archived ? archiveIngredient(ingredient.id) : restoreIngredient(ingredient.id));
      await loadIngredients();
    } catch (e) {
      error = toApiError(e);
    }
  }

  function toggle(id: number) {
    const next = new Set(open);
    if (!next.delete(id)) next.add(id);
    open = next;
  }

  function detail(i: Ingredient): string {
    if (i.yeast_strain) return i.yeast_strain;
    if (i.category === "nutrient") return i.mg_yan_per_g === null ? "no YAN figure" : `${i.mg_yan_per_g} mg YAN/g`;
    return "";
  }
</script>

<section class="ref">
  <nav class="tabs">
    <a href={href({ name: "ingredients", tab: "ingredients" })} class:active={tab === "ingredients"}>Ingredients</a>
    <a href={href({ name: "ingredients", tab: "strains" })} class:active={tab === "strains"}>Yeast strains</a>
    <label class="check">
      <input type="checkbox" bind:checked={showArchived} />
      <span>Show archived</span>
    </label>
  </nav>

  {#if tab === "strains"}
    <StrainsTab
      strains={showArchived ? strains : strains.filter((s) => s.archived_at === null)}
      temp={config.ui.temp_display}
      onchanged={loadStrains}
    />
  {:else}
    <p class="intro">
      An <strong>ingredient</strong> is what a recipe calls for. A <strong>lot</strong> is one purchase of it — how
      much, what it cost, where from. A batch's additions are drawn from lots, so an ingredient needs a lot in stock
      before a batch can use it. Mark a lot used up once it's gone.
    </p>

    {#if editing === "new"}
      <h3>New ingredient</h3>
      <div class="editor">
        <IngredientFields bind:form {strains} {presets} onstrain={loadStrains} />
        <div class="actions">
          <button onclick={save} disabled={busy || form.name.trim() === ""}>Add</button>
          <button class="link" onclick={cancel}>Cancel</button>
        </div>
      </div>
    {:else}
      <div class="actions">
        <button onclick={() => edit(null)}>Add an ingredient</button>
      </div>
    {/if}

    {#if groups.length === 0}
      <p class="empty">No ingredients yet.</p>
    {:else}
      <div class="row head">
        <span>Ingredient</span>
        <span>Unit</span>
        <span>Strain / YAN</span>
        <span>In stock</span>
        <span></span>
      </div>
    {/if}
    {#each groups as g (g.value)}
      <h3>{g.label}</h3>
      {#each g.items as i (i.id)}
        <div class="row" class:archived={i.archived_at !== null} class:expanded={open.has(i.id)}>
          <button class="link name" onclick={() => toggle(i.id)} aria-expanded={open.has(i.id)}>
            {open.has(i.id) ? "▾" : "▸"} {i.name}
          </button>
          <span class="detail">{i.default_unit}</span>
          <span class="detail">{detail(i)}</span>
          <button
            class="link stock"
            class:none={i.lot_count === 0 && i.archived_at === null}
            onclick={() => toggle(i.id)}
            title={i.lot_count === 0 ? "Batches can't use it until it has a lot" : "Show its lots"}
          >
            {i.lot_count === 0 ? "none" : `${i.lot_count} lot${i.lot_count === 1 ? "" : "s"}`}
          </button>
          <span class="row-actions">
            <button class="link" onclick={() => edit(i)}>Edit</button>
            {#if i.archived_at === null}
              <button class="link" onclick={() => setArchived(i, true)}>Archive</button>
            {:else}
              <button class="link" onclick={() => setArchived(i, false)}>Restore</button>
            {/if}
          </span>
        </div>
        {#if editing === i.id}
          <div class="editor">
            <IngredientFields bind:form {strains} {presets} onstrain={loadStrains} />
            <div class="actions">
              <button onclick={save} disabled={busy || form.name.trim() === ""}>Save</button>
              <button class="link" onclick={cancel}>Cancel</button>
            </div>
          </div>
        {/if}
        {#if open.has(i.id)}
          <LotsPanel
            ingredient={i}
            currency={config.money.default_currency}
            {showArchived}
            onchanged={loadIngredients}
          />
        {/if}
      {/each}
    {/each}
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  section {
    max-width: 56rem;
  }

  .tabs {
    display: flex;
    align-items: baseline;
    gap: 1rem;
    margin-bottom: 1rem;
  }

  .tabs a {
    color: #9aa0a6;
    text-decoration: none;
  }

  .tabs a.active {
    color: inherit;
    border-bottom: 2px solid #2d5d8a;
  }

  .tabs .check {
    display: flex;
    gap: 0.4rem;
    margin: 0 0 0 auto;
  }

  h3 {
    margin: 1.25rem 0 0.25rem;
    color: #9aa0a6;
    font-weight: normal;
    font-size: 0.95rem;
  }

  .intro {
    max-width: 44rem;
    margin: 0 0 0.5rem;
    color: #9aa0a6;
    line-height: 1.45;
  }

  .intro strong {
    color: #e6e6e6;
    font-weight: normal;
  }

  .row {
    display: grid;
    grid-template-columns: 14rem 3rem 1fr 5rem 7rem;
    align-items: baseline;
    gap: 1rem;
    padding: 0.25rem 0;
    border-bottom: 1px solid #2a2e36;
  }

  .row.head {
    margin-top: 1rem;
    color: #6f757d;
    font-size: 0.85rem;
  }

  .row.archived,
  .row.archived .detail {
    color: #6f757d;
  }

  .row.expanded {
    border-bottom-color: transparent;
  }

  .name {
    text-align: left;
    text-decoration: none;
    color: inherit;
  }

  .detail {
    color: #9aa0a6;
  }

  .ref button.stock {
    text-align: left;
    color: #9aa0a6;
  }

  .ref button.stock.none {
    color: #d8a657;
  }

  .editor {
    padding: 0.75rem 0 0.25rem 1.5rem;
  }
</style>
