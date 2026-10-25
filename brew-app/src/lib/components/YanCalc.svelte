<script lang="ts">
  import { calcYan, toApiError, type CalcPreset, type YanResult } from "$lib/api";
  import { GRAVITY_UNIT } from "$lib/batch";
  import { formatDose, formatPpm, toYanInput, type YanForm } from "$lib/calc";
  import type { GravityDisplay } from "$lib/units";

  let {
    gravity,
    og,
    volume,
    demands,
    nutrients,
    doses,
  }: {
    gravity: GravityDisplay;
    og: string;
    volume: string;
    demands: CalcPreset[];
    /** `yan_presets` and the nutrient ingredients that carry a YAN figure. */
    nutrients: CalcPreset[];
    doses: number;
  } = $props();

  let form = $state<YanForm>({ gravity: "", volume: "", demand: "", mgYanPerG: "", doses: "" });
  let result = $state<YanResult | null>(null);
  let error = $state<string | null>(null);
  let seq = 0;

  $effect.pre(() => {
    form.gravity = og;
    form.volume = volume;
  });
  $effect.pre(() => {
    form.doses = String(doses);
  });

  const nutrient = $derived(nutrients.find((p) => String(p.value) === form.mgYanPerG)?.label ?? "");

  $effect(() => {
    const input = toYanInput(form);
    const n = ++seq;
    result = null;
    error = input && !input.ok ? input.error : null;
    if (!input?.ok) return;
    calcYan(input.value).then(
      (r) => n === seq && (result = r),
      (e) => n === seq && (error = toApiError(e).message),
    );
  });
</script>

<section class="ref calc">
  <h3>Nutrient (YAN)</h3>
  <label>
    <span>Gravity ({GRAVITY_UNIT[gravity]})</span>
    <input bind:value={form.gravity} inputmode="decimal" />
  </label>
  <label>
    <span>Volume (L)</span>
    <input bind:value={form.volume} inputmode="decimal" />
  </label>
  <label>
    <span>Demand</span>
    <span class="pair">
      <select bind:value={form.demand}>
        <option value="">Pick…</option>
        {#each demands as d (d.label)}
          <option value={String(d.value)}>{d.label} ({d.value})</option>
        {/each}
      </select>
      <input bind:value={form.demand} inputmode="decimal" aria-label="Demand factor" />
    </span>
    <small>The yeast's nitrogen demand, mg N per g of sugar. Nothing defaults it.</small>
  </label>
  <label>
    <span>Nutrient</span>
    <span class="pair">
      <select bind:value={form.mgYanPerG}>
        <option value="">Pick…</option>
        {#each nutrients as p (p.label)}
          <option value={String(p.value)}>{p.label} ({p.value})</option>
        {/each}
      </select>
      <input bind:value={form.mgYanPerG} inputmode="decimal" aria-label="mg YAN per g" />
    </span>
    <small>mg YAN per gram of product.</small>
  </label>
  <label>
    <span>Doses</span>
    <input bind:value={form.doses} inputmode="numeric" />
    <small>Equal splits. When each goes in is the recipe's line offsets.</small>
  </label>

  {#if result}
    <p class="answer">
      Target {formatPpm(result.target_ppm)} · {(result.total_mg / 1000).toFixed(2)} g N ·
      <strong>{formatDose(result.product_g)}</strong>
      {nutrient ? `of ${nutrient}` : "of product"}
    </p>
    {#if result.dose_g.length > 1}
      <ol class="doses">
        {#each result.dose_g as g, i (i)}<li>{formatDose(g)}</li>{/each}
      </ol>
    {/if}
    <p class="muted">Sugar {result.sugar_g_per_l.toFixed(0)} g/L</p>
  {/if}
  {#if error}<p role="alert">{error}</p>{/if}
</section>
