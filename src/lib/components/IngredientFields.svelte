<script lang="ts">
  import {
    createYeastStrain,
    toApiError,
    type ApiError,
    type YanPreset,
    type YeastStrain,
  } from "$lib/api";
  import { CATEGORIES, UNITS, strainLabel, type IngredientForm } from "$lib/reference";

  let {
    form = $bindable(),
    strains,
    presets,
    onstrain,
  }: {
    form: IngredientForm;
    strains: YeastStrain[];
    presets: YanPreset[];
    /** A strain was created inline; the caller reloads its list. */
    onstrain: () => Promise<void>;
  } = $props();

  // A yeast must name its strain, so a strain that doesn't exist yet is made here
  // rather than sending the user to the other tab and back.
  let adding = $state(false);
  let strainName = $state("");
  let strainLab = $state("");
  let error = $state<ApiError | null>(null);

  // Archived strains stay pickable only if this ingredient already points at one.
  const pickable = $derived(strains.filter((s) => s.archived_at === null || s.id === form.yeast_strain_id));

  async function addStrain() {
    error = null;
    try {
      const id = await createYeastStrain({
        name: strainName.trim(),
        lab: strainLab.trim() || null,
        form: "dry",
        temp_min_c: null,
        temp_max_c: null,
        attenuation_min: null,
        attenuation_max: null,
        alcohol_tolerance: null,
        notes: null,
      });
      await onstrain();
      form.yeast_strain_id = id;
      adding = false;
      strainName = "";
      strainLab = "";
    } catch (e) {
      error = toApiError(e);
    }
  }
</script>

<label>
  <span>Name</span>
  <input type="text" bind:value={form.name} />
</label>
<label>
  <span>Category</span>
  <select bind:value={form.category}>
    {#each CATEGORIES as c (c.value)}
      <option value={c.value}>{c.label}</option>
    {/each}
  </select>
</label>
<label>
  <span>Unit</span>
  <select bind:value={form.default_unit}>
    {#each UNITS as u (u)}
      <option value={u}>{u}</option>
    {/each}
  </select>
  <small>what lots and recipes are typed in; ea for packets and tablets</small>
</label>

{#if form.category === "yeast"}
  <label>
    <span>Strain</span>
    <select bind:value={form.yeast_strain_id}>
      <option value={null}>— pick one —</option>
      {#each pickable as s (s.id)}
        <option value={s.id}>{strainLabel(s)}</option>
      {/each}
    </select>
    {#if !adding}
      <small><button class="link" onclick={() => (adding = true)}>New strain…</button></small>
    {/if}
  </label>
  {#if adding}
    <div class="inline">
      <input type="text" placeholder="Strain, e.g. EC-1118" bind:value={strainName} />
      <input type="text" placeholder="Lab (optional)" bind:value={strainLab} />
      <button onclick={addStrain} disabled={strainName.trim() === ""}>Create</button>
      <button class="link" onclick={() => (adding = false)}>Cancel</button>
    </div>
    <p class="hint">Band and temperatures can be filled in on the Yeast strains tab.</p>
  {/if}
{/if}

{#if form.category === "nutrient"}
  <label>
    <span>mg YAN per g</span>
    <input type="text" inputmode="decimal" bind:value={form.mg_yan_per_g} />
    <small>
      {#each presets as p, i (p.label)}{i > 0 ? " · " : ""}<button
          class="link"
          onclick={() => (form.mg_yan_per_g = String(p.mg_yan_per_g))}>{p.label} {p.mg_yan_per_g}</button
        >{/each}
      — without it, a batch's nitrogen total is unknown
    </small>
  </label>
{/if}

<label>
  <span>Notes</span>
  <input type="text" bind:value={form.notes} />
</label>

{#if error}
  <p role="alert">{error.message}</p>
{/if}

<style>
  .inline {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    margin: 0 0 0.5rem 11rem;
  }

  .hint {
    margin: 0 0 0.75rem 11rem;
    color: var(--text-faint);
  }
</style>
