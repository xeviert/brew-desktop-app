<script lang="ts">
  import {
    archiveYeastStrain,
    createYeastStrain,
    restoreYeastStrain,
    toApiError,
    updateYeastStrain,
    type ApiError,
    type YeastStrain,
  } from "$lib/api";
  import { YEAST_FORMS, strainForm, toStrainInput, type StrainForm } from "$lib/reference";
  import { formatTemp, type TempDisplay } from "$lib/units";

  let {
    strains,
    temp,
    onchanged,
  }: {
    strains: YeastStrain[];
    temp: TempDisplay;
    onchanged: () => Promise<void>;
  } = $props();

  let editing = $state<number | "new" | null>(null);
  // svelte-ignore state_referenced_locally -- a seed; `edit` replaces it.
  let form = $state<StrainForm>(strainForm(null, temp));
  let error = $state<ApiError | null>(null);
  let busy = $state(false);

  const pct = (f: number | null) => (f === null ? "?" : `${Number((f * 100).toFixed(1))}`);

  function band(s: YeastStrain): string {
    if (s.attenuation_min === null && s.attenuation_max === null) return "—";
    return `${pct(s.attenuation_min)}–${pct(s.attenuation_max)} %`;
  }

  function temps(s: YeastStrain): string {
    if (s.temp_min_c === null && s.temp_max_c === null) return "—";
    const t = (c: number | null) => (c === null ? "?" : formatTemp(c, temp, 0));
    return `${t(s.temp_min_c)} – ${t(s.temp_max_c)}`;
  }

  function edit(strain: YeastStrain | null) {
    error = null;
    form = strainForm(strain, temp);
    editing = strain?.id ?? "new";
  }

  // A refusal belongs to the form it came from; closing the form drops it.
  function cancel() {
    editing = null;
    error = null;
  }

  async function save() {
    const input = toStrainInput($state.snapshot(form), temp);
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    busy = true;
    error = null;
    try {
      if (editing === "new") await createYeastStrain(input.value);
      else if (editing !== null) await updateYeastStrain(editing, input.value);
      editing = null;
      await onchanged();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  async function setArchived(strain: YeastStrain, archived: boolean) {
    error = null;
    try {
      await (archived ? archiveYeastStrain(strain.id) : restoreYeastStrain(strain.id));
      await onchanged();
    } catch (e) {
      error = toApiError(e);
    }
  }
</script>

{#if strains.length === 0}
  <p class="empty">No yeast strains yet.</p>
{:else}
  <table>
    <thead>
      <tr>
        <th>Strain</th>
        <th>Lab</th>
        <th>Form</th>
        <th>Attenuation</th>
        <th>Temperature</th>
        <th>Tolerance</th>
        <th></th>
      </tr>
    </thead>
    <tbody>
      {#each strains as s (s.id)}
        <tr class:archived={s.archived_at !== null}>
          <td>{s.name}</td>
          <td>{s.lab ?? "—"}</td>
          <td>{s.form}</td>
          <td title="The finish detector's expected-FG band needs both ends">{band(s)}</td>
          <td>{temps(s)}</td>
          <td>{s.alcohol_tolerance === null ? "—" : `${pct(s.alcohol_tolerance)} %`}</td>
          <td class="row-actions">
            <button class="link" onclick={() => edit(s)}>Edit</button>
            {#if s.archived_at === null}
              <button class="link" onclick={() => setArchived(s, true)}>Archive</button>
            {:else}
              <button class="link" onclick={() => setArchived(s, false)}>Restore</button>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

{#if editing !== null}
  <h3>{editing === "new" ? "New strain" : "Edit strain"}</h3>
  <label>
    <span>Name</span>
    <input type="text" bind:value={form.name} placeholder="EC-1118" />
  </label>
  <label>
    <span>Lab</span>
    <input type="text" bind:value={form.lab} placeholder="Lallemand" />
  </label>
  <label>
    <span>Form</span>
    <select bind:value={form.form}>
      {#each YEAST_FORMS as f (f)}
        <option value={f}>{f}</option>
      {/each}
    </select>
  </label>
  <label>
    <span>Attenuation (%)</span>
    <div class="pair">
      <input type="text" inputmode="decimal" placeholder="min" bind:value={form.attenuation_min} />
      <input type="text" inputmode="decimal" placeholder="max" bind:value={form.attenuation_max} />
    </div>
    <small>as the packet says, in percent — both ends give the finish detector its band</small>
  </label>
  <label>
    <span>Temperature (°{temp})</span>
    <div class="pair">
      <input type="text" inputmode="decimal" placeholder="min" bind:value={form.temp_min} />
      <input type="text" inputmode="decimal" placeholder="max" bind:value={form.temp_max} />
    </div>
  </label>
  <label>
    <span>Alcohol tolerance (%)</span>
    <input type="text" inputmode="decimal" bind:value={form.alcohol_tolerance} />
  </label>
  <label>
    <span>Notes</span>
    <input type="text" bind:value={form.notes} />
  </label>
  <div class="actions">
    <button onclick={save} disabled={busy || form.name.trim() === ""}>Save</button>
    <button class="link" onclick={cancel}>Cancel</button>
  </div>
{:else}
  <div class="actions">
    <button onclick={() => edit(null)}>Add a strain</button>
  </div>
{/if}

{#if error}
  <p role="alert">{error.message}</p>
{/if}

<style>
  table {
    border-collapse: collapse;
  }

  th,
  td {
    padding: 0.3rem 1rem 0.3rem 0;
    text-align: left;
    font-variant-numeric: tabular-nums;
  }

  th {
    color: var(--text-faint);
    font-weight: normal;
  }

  tr.archived td {
    color: var(--text-faint);
  }

  .row-actions {
    display: flex;
    gap: 0.75rem;
  }

  .pair {
    display: flex;
    gap: 0.5rem;
  }

  .pair input {
    width: 5rem;
  }
</style>
