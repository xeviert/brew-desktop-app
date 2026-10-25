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
  import type { GravityDisplay } from "$lib/units";
  import PitchCalc from "./PitchCalc.svelte";
  import SugarCalc from "./SugarCalc.svelte";
  import YanCalc from "./YanCalc.svelte";

  // `version` prefills gravity and volume: the recipe screen's "Calculators for vN".
  let { version, gravity }: { version: number | null; gravity: GravityDisplay } = $props();

  let defaults = $state<CalcDefaults | null>(null);
  let nutrients = $state<CalcPreset[]>([]);
  let strains = $state<YeastStrain[]>([]);
  let prefill = $state({ gravity: "", volume: "" });
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
        // The user's own nutrients first: they are what is on the shelf.
        nutrients = [
          ...ingredients
            .filter((i) => i.category === "nutrient" && i.mg_yan_per_g !== null)
            .map((i) => ({ label: i.name, value: i.mg_yan_per_g! })),
          ...presets.map((p) => ({ label: p.label, value: p.mg_yan_per_g })),
        ];
        strains = s;
        defaults = d;
        if (version !== null) {
          const v = await getRecipeVersion(version);
          const detail = await getRecipe(v.recipe_id);
          prefill = fromVersion(v);
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
  <SugarCalc {gravity} og={prefill.gravity} />
  <YanCalc
    {gravity}
    og={prefill.gravity}
    volume={prefill.volume}
    demands={defaults.yan_demand}
    {nutrients}
    doses={defaults.tosna_doses.length}
  />
  <PitchCalc
    {gravity}
    og={prefill.gravity}
    volume={prefill.volume}
    rates={defaults.pitch_rates}
    cellsPerGram={defaults.dry_cells_per_gram}
    {strains}
  />
{/if}

<style>
  .from {
    color: #9aa0a6;
  }

  .from a {
    color: #7ba7d4;
  }
</style>
