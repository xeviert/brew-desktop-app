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
    const parts: string[] = [i.default_unit];
    if (i.yeast_strain) parts.push(i.yeast_strain);
    if (i.category === "nutrient") parts.push(i.mg_yan_per_g === null ? "no YAN figure" : `${i.mg_yan_per_g} mg YAN/g`);
    return parts.join(" · ");
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
    {#if groups.length === 0}
      <p class="empty">No ingredients yet.</p>
    {/if}
    {#each groups as g (g.value)}
      <h3>{g.label}</h3>
      {#each g.items as i (i.id)}
        <div class="row" class:archived={i.archived_at !== null}>
          <button class="link name" onclick={() => toggle(i.id)} aria-expanded={open.has(i.id)}>
            {open.has(i.id) ? "▾" : "▸"} {i.name}
          </button>
          <span class="detail">{detail(i)}</span>
          <span class="detail">{i.lot_count} lot{i.lot_count === 1 ? "" : "s"}</span>
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

  .row {
    display: grid;
    grid-template-columns: 14rem 1fr 5rem auto;
    align-items: baseline;
    gap: 1rem;
    padding: 0.25rem 0;
    border-bottom: 1px solid #2a2e36;
  }

  .row.archived,
  .row.archived .detail {
    color: #6f757d;
  }

  .name {
    text-align: left;
    text-decoration: none;
    color: inherit;
  }

  .detail {
    color: #9aa0a6;
  }

  .editor {
    padding: 0.75rem 0 0.25rem 1.5rem;
  }

  /* Shared by the child forms, which are all inside this section. */
  .ref :global(.empty) {
    color: #6f757d;
  }

  .ref :global(label) {
    display: grid;
    grid-template-columns: 10rem 1fr;
    align-items: center;
    gap: 0.25rem 1rem;
    margin-bottom: 0.6rem;
    max-width: 36rem;
  }

  .ref :global(label > span) {
    color: #9aa0a6;
  }

  .ref :global(label small) {
    grid-column: 2;
    color: #6f757d;
  }

  .ref :global(input),
  .ref :global(select) {
    padding: 0.35rem 0.5rem;
    background: #1e2127;
    color: inherit;
    border: 1px solid #333842;
    border-radius: 3px;
    font: inherit;
  }

  .ref :global(input[type="checkbox"]) {
    padding: 0;
  }

  .ref :global(.actions) {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin: 0.75rem 0;
  }

  .ref :global(button) {
    padding: 0.4rem 1rem;
    background: #2d5d8a;
    color: #fff;
    border: none;
    border-radius: 3px;
    font: inherit;
    cursor: pointer;
  }

  .ref :global(button:disabled) {
    background: #2a2e36;
    color: #6f757d;
    cursor: default;
  }

  .ref :global(button.link) {
    padding: 0;
    background: none;
    color: #7ba7d4;
  }

  .ref :global(.row-actions) {
    display: flex;
    gap: 0.75rem;
  }

  .ref :global(p[role="alert"]) {
    color: #e06c5f;
  }
</style>
