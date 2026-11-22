<script lang="ts">
  import { FORMATS, impliedVolume, type PackagingForm } from "$lib/packaging";

  // One packaging line's fields, inside a `.ref` form: the Packaging tab's and the
  // "Mark packaged" panel's. The size placeholder follows the format.
  let { form = $bindable() }: { form: PackagingForm } = $props();

  const implied = $derived(impliedVolume(form));
  const sizeHint = $derived(FORMATS.find((f) => f.value === form.format)?.size ?? "");
</script>

<div class="line-fields">
  <label>
    <span>Format</span>
    <select bind:value={form.format}>
      {#each FORMATS as f (f.value)}
        <option value={f.value}>{f.label}</option>
      {/each}
    </select>
  </label>
  <label>
    <span>Size (L)</span>
    <input type="text" inputmode="decimal" bind:value={form.size} placeholder={sizeHint} />
  </label>
  <label>
    <span>Count</span>
    <input type="text" inputmode="numeric" bind:value={form.count} />
  </label>
  <label>
    <span>Volume (L)</span>
    <input
      type="text"
      inputmode="decimal"
      bind:value={form.volume}
      placeholder={implied === null ? "" : String(implied)}
    />
    <small>{implied === null ? "What went in." : "Blank is size × count."}</small>
  </label>
  <label class="wide">
    <span>Notes</span>
    <input type="text" bind:value={form.notes} placeholder="primed 6 g/L" />
  </label>
</div>

<style>
  .line-fields {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 0.5rem 1rem;
  }

  .line-fields label {
    grid-template-columns: 1fr;
    gap: 0.25rem;
    margin: 0;
    max-width: none;
  }

  .line-fields label small {
    grid-column: 1;
  }

  .line-fields input,
  .line-fields select {
    min-width: 0;
  }

  .wide {
    grid-column: 1 / -1;
  }
</style>
