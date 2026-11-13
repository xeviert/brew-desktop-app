<script lang="ts">
  import { calcYan, toApiError, type CalcPreset, type YanResult } from "$lib/api";
  import { formatDose, formatPpm, toYanInput, type YanForm } from "$lib/calc";

  let {
    og,
    volume,
    demands,
    shelf,
    datasheet,
    doses,
  }: {
    og: string;
    volume: string;
    demands: CalcPreset[];
    /** Nutrient ingredients that carry a YAN figure. */
    shelf: CalcPreset[];
    /** `yan_presets`. */
    datasheet: CalcPreset[];
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

  const nutrient = $derived(
    [...shelf, ...datasheet].find((p) => String(p.value) === form.mgYanPerG)?.label ?? "",
  );
  const missing = $derived(
    (
      [
        ["gravity", form.gravity],
        ["volume", form.volume],
        ["demand", form.demand],
        ["nutrient", form.mgYanPerG],
      ] as const
    )
      .filter(([, v]) => v.trim() === "")
      .map(([name]) => name),
  );

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
    <span>Demand</span>
    <span class="pair">
      <select bind:value={form.demand}>
        <option value="">Pick…</option>
        {#each demands as d (d.label)}
          <option value={String(d.value)}>{d.label}</option>
        {/each}
      </select>
      <input bind:value={form.demand} inputmode="decimal" aria-label="Demand factor" />
    </span>
    <small>The yeast's nitrogen demand, mg N per g of sugar. Its datasheet says low, medium or high.</small>
  </label>
  <label>
    <span>Nutrient</span>
    <span class="pair">
      <select bind:value={form.mgYanPerG}>
        <option value="">Pick…</option>
        {#if shelf.length}
          <optgroup label="Your ingredients">
            {#each shelf as p (p.label)}
              <option value={String(p.value)}>{p.label}</option>
            {/each}
          </optgroup>
        {/if}
        <optgroup label="Datasheet figures">
          {#each datasheet as p (p.label)}
            <option value={String(p.value)}>{p.label}</option>
          {/each}
        </optgroup>
      </select>
      <input bind:value={form.mgYanPerG} inputmode="decimal" aria-label="mg YAN per g" />
    </span>
    <small>mg YAN per gram of product.</small>
  </label>
  <label>
    <span>Doses</span>
    <input bind:value={form.doses} inputmode="numeric" />
    <small>Split into this many equal additions.</small>
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
  {#if error}
    <p role="alert">{error}</p>
  {:else if !result && missing.length}
    <p class="muted">Needs {missing.join(", ")}.</p>
  {/if}
</section>
