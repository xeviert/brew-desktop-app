<script lang="ts">
  import { calcPitch, toApiError, type CalcPreset, type PitchResult, type YeastStrain } from "$lib/api";
  import { GRAVITY_UNIT } from "$lib/batch";
  import { billions, formatCells, formatDose, toPitchInput, type PitchForm } from "$lib/calc";
  import { strainLabel } from "$lib/reference";
  import type { GravityDisplay } from "$lib/units";

  let {
    gravity,
    og,
    volume,
    rates,
    cellsPerGram,
    strains,
  }: {
    gravity: GravityDisplay;
    og: string;
    volume: string;
    rates: CalcPreset[];
    cellsPerGram: number;
    strains: YeastStrain[];
  } = $props();

  let form = $state<PitchForm>({ gravity: "", volume: "", rate: "", dry: true, cellsPerGram: "", viability: "" });
  let strainId = $state<number | null>(null);
  let result = $state<PitchResult | null>(null);
  let error = $state<string | null>(null);
  let seq = 0;

  $effect.pre(() => {
    form.gravity = og;
    form.volume = volume;
  });
  $effect.pre(() => {
    form.cellsPerGram = billions(cellsPerGram);
  });

  /** The strain's form decides whether grams apply; nothing else is read off it. */
  function pickStrain(e: Event) {
    const value = (e.currentTarget as HTMLSelectElement).value;
    strainId = value === "" ? null : Number(value);
    const strain = strains.find((s) => s.id === strainId);
    if (strain) form.dry = strain.form === "dry";
  }

  $effect(() => {
    const input = toPitchInput(form);
    const n = ++seq;
    result = null;
    error = input && !input.ok ? input.error : null;
    if (!input?.ok) return;
    calcPitch(input.value).then(
      (r) => n === seq && (result = r),
      (e) => n === seq && (error = toApiError(e).message),
    );
  });
</script>

<section class="ref calc">
  <h3>Pitch rate</h3>
  <label>
    <span>Gravity ({GRAVITY_UNIT[gravity]})</span>
    <input bind:value={form.gravity} inputmode="decimal" />
  </label>
  <label>
    <span>Volume (L)</span>
    <input bind:value={form.volume} inputmode="decimal" />
  </label>
  <label>
    <span>Rate</span>
    <span class="pair">
      <select bind:value={form.rate}>
        <option value="">Pick…</option>
        {#each rates as r (r.label)}
          <option value={String(r.value)}>{r.label} ({r.value})</option>
        {/each}
      </select>
      <input bind:value={form.rate} inputmode="decimal" aria-label="Pitch rate" />
    </span>
    <small>Million cells per mL per °P.</small>
  </label>
  {#if strains.length}
    <label>
      <span>Strain</span>
      <select value={strainId ?? ""} onchange={pickStrain}>
        <option value="">None</option>
        {#each strains as s (s.id)}
          <option value={s.id}>{strainLabel(s)} · {s.form}</option>
        {/each}
      </select>
    </label>
  {/if}
  <label>
    <span>Dry yeast</span>
    <input type="checkbox" bind:checked={form.dry} />
  </label>
  {#if form.dry}
    <label>
      <span>Cells per gram (×10⁹)</span>
      <input bind:value={form.cellsPerGram} inputmode="decimal" />
      <small>Prefilled with the packet-spec floor; a fresh packet assays higher.</small>
    </label>
    <label>
      <span>Viability (%)</span>
      <input bind:value={form.viability} inputmode="decimal" />
      <small>Packet age unknown? Type 100 and treat the answer as a minimum.</small>
    </label>
  {/if}

  {#if result}
    <p class="answer">
      <strong>{formatCells(result.cells)}</strong>
      {#if result.dry_g !== null}· <strong>{formatDose(result.dry_g)}</strong> of dry yeast{/if}
    </p>
    {#if form.dry && result.dry_g === null}
      <p class="muted">Type a viability for grams.</p>
    {/if}
  {/if}
  {#if error}<p role="alert">{error}</p>{/if}
</section>
