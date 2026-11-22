<script lang="ts">
  import { untrack } from "svelte";
  import {
    addPackaging,
    batchPackaging,
    deletePackaging,
    toApiError,
    updatePackaging,
    type ApiError,
    type Packaging,
    type PackagingLine,
  } from "$lib/api";
  import { fromLocalDateTime, localDateTime } from "$lib/batch";
  import { confirmAction } from "$lib/confirm";
  import PackagingFields from "$lib/components/PackagingFields.svelte";
  import { lineText, lossText, packagingForm, toPackagingInput, type PackagingForm } from "$lib/packaging";
  import { formatMoney, formatTimestamp, formatVolume } from "$lib/units";

  // What left the vessel: each line a `package` event on the timeline, so `onchanged`
  // redraws it. The batch's cost is shared over what was packaged, loss included.
  let {
    batchId,
    readonly,
    revision,
    onchanged,
  }: { batchId: number; readonly: boolean; revision: number; onchanged: () => void } = $props();

  let data = $state.raw<Packaging | null>(null);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);
  /** The line being edited: its id, or `"new"`. */
  let draftFor = $state<number | "new" | null>(null);
  let form = $state<PackagingForm>(packagingForm(null));
  let at = $state("");

  const lines = $derived(data?.lines ?? []);
  const costOf = (id: number) => data?.cost?.lines.find((c) => c.packaging_id === id) ?? null;
  const loss = $derived(data ? lossText(data) : null);

  $effect(() => {
    void revision;
    untrack(() => void load());
  });

  async function load() {
    try {
      data = await batchPackaging(batchId);
      error = null;
    } catch (e) {
      error = toApiError(e);
    }
  }

  function startDraft(line: PackagingLine | null) {
    draftFor = line?.id ?? "new";
    form = packagingForm(line);
    at = localDateTime(line?.at ?? Date.now() / 1000);
    error = null;
  }

  async function attempt(write: () => Promise<Packaging>) {
    busy = true;
    error = null;
    try {
      data = await write();
      onchanged();
      return true;
    } catch (e) {
      error = toApiError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    const input = toPackagingInput(form);
    if (!input.ok) {
      error = { code: "form", message: input.error };
      return;
    }
    const t = fromLocalDateTime(at);
    if (t === null) {
      error = { code: "form", message: "Not readable: when" };
      return;
    }
    const id = draftFor;
    const ok = await attempt(() =>
      id === "new" ? addPackaging(batchId, t, input.value) : updatePackaging(id as number, t, input.value),
    );
    if (ok) draftFor = null;
  }

  async function remove(line: PackagingLine) {
    if (!(await confirmAction(`Delete the ${lineText(line)} line, and its event on the timeline?`, "Delete"))) return;
    void attempt(() => deletePackaging(line.id));
  }
</script>

<section class="ref packaging" aria-labelledby="packaging-title">
  <nav class="bar">
    <h3 id="packaging-title">Packaging</h3>
    {#if !readonly && data?.pitched_at != null && draftFor === null}
      <button class="link" onclick={() => startDraft(null)}>Add a line</button>
    {/if}
  </nav>

  {#if data && lines.length === 0 && draftFor !== "new"}
    <p class="empty">
      {#if data.pitched_at === null}
        Nothing to package before the pitch.
      {:else}
        Nothing packaged yet. "Mark packaged" takes the lines with the move; a line can also go in on its own.
      {/if}
    </p>
  {/if}

  {#if draftFor === "new"}
    {@render entry()}
  {/if}

  {#if lines.length > 0}
    <table>
      <thead>
        <tr><th>When</th><th>Line</th><th>Volume</th><th>Cost</th><th></th></tr>
      </thead>
      <tbody>
        {#each lines as line (line.id)}
          {@const cost = costOf(line.id)}
          <tr class:current={draftFor === line.id}>
            <td>{formatTimestamp(line.at)}</td>
            <td>
              {lineText(line)}
              {#if line.notes}<small>{line.notes}</small>{/if}
            </td>
            <td>{formatVolume(line.volume_l)}</td>
            <td>
              {#if cost?.cost}
                {formatMoney(cost.cost)}
                {#if cost.cost_each}<small>{formatMoney(cost.cost_each)} each</small>{/if}
              {:else}
                <span class="muted">—</span>
              {/if}
            </td>
            <td>
              {#if !readonly && draftFor === null}
                <span class="row-actions">
                  <button class="link" disabled={busy} onclick={() => startDraft(line)}>Edit</button>
                  <button class="link" disabled={busy} onclick={() => remove(line)}>Delete</button>
                </span>
              {/if}
            </td>
          </tr>
          {#if draftFor === line.id}
            <tr class="form-row"><td colspan="5">{@render entry()}</td></tr>
          {/if}
        {/each}
      </tbody>
    </table>
  {/if}

  {#if error && draftFor === null}
    <p role="alert">{error.message}</p>
  {/if}

  {#if data && lines.length > 0}
    <p class="summary">
      Packaged <strong>{formatVolume(data.packaged_l)}</strong>
      {#if data.volume_l !== null}of {formatVolume(data.volume_l)}{/if}
      {#if loss}· <span class:flag={data.loss_l !== null && data.loss_l < 0}>{loss}</span>{/if}
      {#if data.cost_error}
        · <span class="flag">{data.cost_error.message}</span>
      {:else if data.cost?.per_packaged_l}
        · {formatMoney(data.cost.per_packaged_l)}/L packaged
      {/if}
    </p>
  {/if}
</section>

{#snippet entry()}
  <form class="entry" onsubmit={save}>
    <PackagingFields bind:form />
    <label class="when">
      <span>When</span>
      <input type="datetime-local" bind:value={at} />
    </label>
    {#if error}
      <p role="alert">{error.message}</p>
    {/if}
    <div class="actions">
      <button type="submit" disabled={busy}>{draftFor === "new" ? "Add line" : "Save line"}</button>
      <button type="button" class="link" onclick={() => ((draftFor = null), (error = null))}>Cancel</button>
    </div>
  </form>
{/snippet}

<style>
  .packaging {
    max-width: 52rem;
  }

  .bar {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  h3 {
    margin: 0 0 0.5rem;
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }

  th,
  td {
    text-align: left;
    padding: 0.25rem 0.5rem 0.25rem 0;
    border-bottom: 1px solid #2a2e36;
    vertical-align: baseline;
  }

  thead th {
    color: #9aa0a6;
    font-weight: normal;
  }

  tr.current td {
    border-bottom: none;
  }

  tr.form-row > td {
    padding: 0 0 0.5rem;
  }

  small,
  .muted {
    color: #6f757d;
  }

  td small {
    display: block;
  }

  td:nth-child(n + 3) {
    white-space: nowrap;
  }

  .flag {
    color: #e0a35f;
  }

  .entry {
    padding: 0.75rem;
    margin: 0.25rem 0 0.75rem;
    background: #1e2127;
    border-left: 3px solid #2d5d8a;
  }

  .entry .when {
    grid-template-columns: 1fr;
    gap: 0.25rem;
    margin: 0.5rem 0 0;
    max-width: 16rem;
  }

  .entry .actions {
    margin-bottom: 0;
  }

  .summary {
    margin: 0.75rem 0 0;
  }
</style>
