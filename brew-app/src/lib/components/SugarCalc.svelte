<script lang="ts">
  import { calcOgFromSugar, calcSugarForOg, toApiError, type SugarResult } from "$lib/api";
  import { GRAVITY_UNIT } from "$lib/batch";
  import { toSugarArgs, type SugarForm } from "$lib/calc";
  import { formatGravityIn } from "$lib/dashboard";
  import { formatGravity, formatMass, type GravityDisplay } from "$lib/units";

  let { gravity, og }: { gravity: GravityDisplay; og: string } = $props();

  let form = $state<SugarForm>({ mode: "target", og: "", sugar: "", water: "" });
  let result = $state<SugarResult | null>(null);
  let error = $state<string | null>(null);
  let seq = 0;

  $effect.pre(() => {
    form.og = og;
  });

  $effect(() => {
    const args = toSugarArgs(form);
    const n = ++seq;
    result = null;
    error = args && !args.ok ? args.error : null;
    if (!args?.ok) return;
    const a = args.value;
    const pending = a.mode === "target" ? calcSugarForOg(a.og, a.water) : calcOgFromSugar(a.sugarG, a.water);
    pending.then(
      (r) => n === seq && (result = r),
      (e) => n === seq && (error = toApiError(e).message),
    );
  });
</script>

<section class="ref calc">
  <h3>Sugar wash</h3>
  <div class="actions">
    <label class="radio"><input type="radio" bind:group={form.mode} value="target" /> Sugar for a target OG</label>
    <label class="radio"><input type="radio" bind:group={form.mode} value="og" /> OG from a weighed sugar</label>
  </div>
  {#if form.mode === "target"}
    <label>
      <span>Target OG ({GRAVITY_UNIT[gravity]})</span>
      <input bind:value={form.og} inputmode="decimal" />
    </label>
  {:else}
    <label>
      <span>Sucrose (kg)</span>
      <input bind:value={form.sugar} inputmode="decimal" />
    </label>
  {/if}
  <label>
    <span>Water added (L)</span>
    <input bind:value={form.water} inputmode="decimal" />
    <small>Not the finished volume: dissolved sugar adds its own.</small>
  </label>

  {#if result}
    <p class="answer">
      {#if form.mode === "target"}
        <strong>{formatMass(result.sugar_g, 2)}</strong> of sucrose
      {:else}
        OG <strong>{formatGravityIn(result.og, GRAVITY_UNIT[gravity])}</strong>
        {#if gravity !== "sg"}<span class="muted">({formatGravity(result.og_sg)})</span>{/if}
      {/if}
    </p>
  {/if}
  {#if error}<p role="alert">{error}</p>{/if}
</section>
