<script lang="ts">
  import { onMount } from "svelte";
  import {
    calcDefaults,
    getRecipe,
    getRecipeVersion,
    listIngredients,
    listYeastStrains,
    toApiError,
    yanPresets,
    type ApiError,
    type CalcDefaults,
    type CalcPreset,
    type YeastStrain,
  } from "$lib/api";
  import { fromVersion } from "$lib/calc";
  import { href } from "$lib/route";
  import { GRAVITY_UNIT } from "$lib/batch";
  import type { GravityDisplay } from "$lib/units";
  import PitchCalc from "./PitchCalc.svelte";
  import SugarCalc from "./SugarCalc.svelte";
  import YanCalc from "./YanCalc.svelte";

  // `version` prefills gravity and volume: the recipe screen's "Calculators for vN".
  let { version, gravity }: { version: number | null; gravity: GravityDisplay } = $props();

  let defaults = $state<CalcDefaults | null>(null);
  /** The user's nutrients with a YAN figure, then `yan_presets`. */
  let shelf = $state<CalcPreset[]>([]);
  let datasheet = $state<CalcPreset[]>([]);
  let strains = $state<YeastStrain[]>([]);
  // One gravity and volume for the nutrient and pitch panels, rather than one each.
  let must = $state({ gravity: "", volume: "" });
  let source = $state<{ recipeId: number; name: string; versionNo: number } | null>(null);
  let error = $state<ApiError | null>(null);

  onMount(() => {
    void (async () => {
      try {
        const [d, presets, ingredients, s] = await Promise.all([
          calcDefaults(),
          yanPresets(),
          listIngredients(false),
          listYeastStrains(false),
        ]);
        shelf = ingredients
          .filter((i) => i.category === "nutrient" && i.mg_yan_per_g !== null)
          .map((i) => ({ label: i.name, value: i.mg_yan_per_g! }));
        datasheet = presets.map((p) => ({ label: p.label, value: p.mg_yan_per_g }));
        strains = s;
        defaults = d;
        if (version !== null) {
          const v = await getRecipeVersion(version);
          const detail = await getRecipe(v.recipe_id);
          must = fromVersion(v);
          source = { recipeId: v.recipe_id, name: detail.recipe.name, versionNo: v.version_no };
        }
      } catch (e) {
        error = toApiError(e);
      }
    })();
  });
</script>

<section class="ref">
  <h2>Calculators</h2>
  {#if source}
    <p class="from">
      Prefilled from
      <a href={href({ name: "recipe", id: source.recipeId, version: source.versionNo })}
        >{source.name} v{source.versionNo}</a
      >'s targets.
    </p>
  {/if}
  {#if error}<p role="alert">{error.message}</p>{/if}
</section>

{#if defaults}
  <section class="ref must">
    <label>
      <span>Gravity ({GRAVITY_UNIT[gravity]})</span>
      <input bind:value={must.gravity} inputmode="decimal" />
    </label>
    <label>
      <span>Volume (L)</span>
      <input bind:value={must.volume} inputmode="decimal" />
      <small>The must's, used by the nutrient and pitch calculators.</small>
    </label>
  </section>

  <div class="grid">
    <SugarCalc {gravity} og={must.gravity} onog={(og) => (must.gravity = og)} />
    <YanCalc
      og={must.gravity}
      volume={must.volume}
      demands={defaults.yan_demand}
      {shelf}
      {datasheet}
      doses={defaults.tosna_doses.length}
    />
    <PitchCalc
      og={must.gravity}
      volume={must.volume}
      rates={defaults.pitch_rates}
      cellsPerGram={defaults.dry_cells_per_gram}
      {strains}
    />
  </div>
{/if}

<style>
  .from {
    color: var(--text-muted);
  }

  .from a {
    color: var(--accent-text);
  }

  .must {
    margin-bottom: 1rem;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(24rem, 1fr));
    gap: 1rem;
    align-items: start;
  }
</style>
