<script lang="ts">
  import { onMount } from "svelte";
  import {
    archiveVessel,
    createVessel,
    listStreams,
    listVessels,
    restoreVessel,
    setStreamMetric,
    setVesselStreams,
    toApiError,
    updateVessel,
    type ApiError,
    type Stream,
    type Vessel,
    type VesselKind,
  } from "$lib/api";
  import { hygieneText } from "$lib/cleaning";
  import VesselCleaning from "$lib/components/VesselCleaning.svelte";
  import { confirmAction } from "$lib/confirm";
  import { metricName } from "$lib/home";

  let vessels = $state<Vessel[]>([]);
  let streams = $state<Stream[]>([]);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);

  let name = $state("");
  let kind = $state<VesselKind>("fermenter");
  let capacity = $state<number | null>(null);

  let showArchived = $state(false);
  let editing = $state<{ id: number; name: string; kind: VesselKind; capacity: number | null; notes: string } | null>(
    null,
  );
  /** Vessels whose cleaning log is open. */
  let logs = $state<Record<number, boolean>>({});

  const KINDS: [VesselKind, string][] = [
    ["fermenter", "Fermenter"],
    ["brite", "Brite tank"],
    ["carboy", "Carboy"],
    ["keg", "Keg"],
    ["kettle", "Kettle"],
    ["other", "Other"],
  ];

  // Manual streams are left out: they belong to a batch, not to the vessel it is in,
  // and packaging deliberately leaves them bound.
  const sensors = $derived(streams.filter((s) => s.source_kind !== "manual"));

  // Mirrors `tier1::TEMPERATURE_METRICS`: a probe's column name says where it was
  // expected to hang, not where it does.
  const PLACES: [string, string][] = [
    ["temp_c", "Fermenter (the beer)"],
    ["fridge_temp_c", "Fridge air"],
    ["room_temp_c", "Room"],
    ["gravity_temp_c", "Hydrometer's own"],
  ];
  const thermometers = $derived(sensors.filter((s) => PLACES.some(([m]) => m === s.metric)));

  async function place(stream: Stream, metric: string) {
    error = null;
    try {
      await setStreamMetric(stream.id, metric);
      stream.metric = metric;
    } catch (e) {
      error = toApiError(e);
      await load();
    }
  }

  async function load() {
    try {
      [vessels, streams] = await Promise.all([listVessels(showArchived), listStreams()]);
    } catch (e) {
      error = toApiError(e);
    }
  }

  onMount(() => {
    void load();
  });

  async function create() {
    busy = true;
    error = null;
    try {
      await createVessel(name.trim(), kind, capacity);
      name = "";
      capacity = null;
      await load();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  async function write(action: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await action();
      await load();
      return true;
    } catch (e) {
      error = toApiError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function saveEdit(event: SubmitEvent) {
    event.preventDefault();
    const e = editing;
    if (!e) return;
    const notes = e.notes.trim() === "" ? null : e.notes.trim();
    if (await write(() => updateVessel(e.id, e.name, e.kind, e.capacity, notes))) editing = null;
  }

  async function archive(vessel: Vessel) {
    const ok = await confirmAction(
      `Archive ${vessel.name}? It leaves the pickers; batches that used it still show it.`,
      "Archive",
    );
    if (ok) void write(() => archiveVessel(vessel.id));
  }

  async function toggle(vessel: Vessel, streamId: number, on: boolean) {
    const next = on
      ? [...vessel.default_streams, streamId]
      : vessel.default_streams.filter((id) => id !== streamId);
    error = null;
    try {
      await setVesselStreams(vessel.id, next);
      vessel.default_streams = next;
    } catch (e) {
      error = toApiError(e);
    }
  }
</script>

<section id="vessels">
  <h2>Vessels</h2>
  <p class="hint">
    Your fermenters, carboys and kegs. Tick the sensors that live in a vessel; whichever batch is in it gets their
    readings, from its pitch until it's packaged. Log each clean, and the pitch warns of a vessel not sanitised since
    its last batch.
  </p>
  <label class="check show-archived">
    <input type="checkbox" bind:checked={showArchived} onchange={() => void load()} />
    <span>Show archived</span>
  </label>

  {#if vessels.length === 0}
    <p class="empty">No vessels yet.</p>
  {:else}
    {#each vessels as vessel (vessel.id)}
      {@const archived = vessel.archived_at !== null}
      {@const state = hygieneText(vessel.hygiene)}
      <div class="vessel" class:archived>
        <h3>
          {vessel.name}
          <small
            >{vessel.kind}{vessel.capacity_l !== null ? ` · ${vessel.capacity_l} L` : ""}{archived
              ? " · archived"
              : ""}</small
          >
        </h3>
        {#if vessel.notes}<p class="hint">{vessel.notes}</p>{/if}
        <p class="hygiene">
          <span class:warn={state.warn}>{state.text}</span>
          <button class="link" onclick={() => (logs[vessel.id] = !logs[vessel.id])}
            >{logs[vessel.id] ? "Hide cleaning log" : "Cleaning log"}</button
          >
          {#if archived}
            <button class="link" disabled={busy} onclick={() => write(() => restoreVessel(vessel.id))}>Restore</button>
          {:else}
            <button
              class="link"
              disabled={busy}
              onclick={() =>
                (editing = {
                  id: vessel.id,
                  name: vessel.name,
                  kind: vessel.kind,
                  capacity: vessel.capacity_l,
                  notes: vessel.notes ?? "",
                })}>Edit</button
            >
            <button class="link" disabled={busy} onclick={() => archive(vessel)}>Archive</button>
          {/if}
        </p>
        {#if logs[vessel.id]}
          <VesselCleaning vesselId={vessel.id} readonly={archived} onchanged={() => void load()} />
        {/if}
        {#if editing?.id === vessel.id}
          <form class="edit" onsubmit={saveEdit}>
            <label>
              <span>Name</span>
              <input type="text" required bind:value={editing.name} />
            </label>
            <label>
              <span>Kind</span>
              <select bind:value={editing.kind}>
                {#each KINDS as [value, label] (value)}
                  <option {value}>{label}</option>
                {/each}
              </select>
            </label>
            <label>
              <span>Capacity (L)</span>
              <input type="number" min="0.5" step="0.5" bind:value={editing.capacity} />
            </label>
            <label>
              <span>Notes</span>
              <input type="text" bind:value={editing.notes} />
            </label>
            <div class="actions">
              <button type="submit" disabled={busy}>Save</button>
              <button type="button" class="link" onclick={() => (editing = null)}>Cancel</button>
            </div>
          </form>
        {/if}
        {#if archived}
          <!-- An archived vessel takes no new pitch, so its sensor defaults are moot. -->
        {:else if sensors.length === 0}
          <p class="empty">No sensor streams yet — they appear once a source has synced.</p>
        {:else}
          {#each sensors as stream (stream.id)}
            <label class="check">
              <input
                type="checkbox"
                checked={vessel.default_streams.includes(stream.id)}
                onchange={(e) => toggle(vessel, stream.id, e.currentTarget.checked)}
              />
              <span>{stream.source_name}</span>
              <span class="metric">{metricName(stream.metric)}</span>
            </label>
          {/each}
        {/if}
      </div>
    {/each}
  {/if}

  {#if thermometers.length > 0}
    <h3>Thermometers</h3>
    <p class="hint">
      Where each one measures. A probe hung in the fridge air reads the fridge, not the beer, whatever the device
      calls it. Past readings move with it.
    </p>
    {#each thermometers as stream (stream.id)}
      <label>
        <span>{stream.source_name} <small class="metric">{stream.external_id}</small></span>
        <select value={stream.metric} onchange={(e) => place(stream, e.currentTarget.value)}>
          {#each PLACES as [metric, label] (metric)}
            <option value={metric}>{label}</option>
          {/each}
        </select>
      </label>
    {/each}
  {/if}

  <h3>Add a vessel</h3>
  <label>
    <span>Name</span>
    <input type="text" bind:value={name} placeholder="FV1" />
  </label>
  <label>
    <span>Kind</span>
    <select bind:value={kind}>
      {#each KINDS as [value, label] (value)}
        <option {value}>{label}</option>
      {/each}
    </select>
  </label>
  <label>
    <span>Capacity (L)</span>
    <input type="number" min="0.5" step="0.5" bind:value={capacity} />
  </label>
  <div class="actions">
    <button onclick={create} disabled={busy || name.trim() === ""}>Add</button>
  </div>

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  section {
    max-width: 40rem;
    margin-top: 2rem;
  }

  .vessel {
    padding: 0.4rem 0 0.75rem;
    border-bottom: 1px solid #2a2e36;
  }

  .vessel.archived h3 {
    color: #9aa0a6;
  }

  .hygiene {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    align-items: baseline;
    margin: 0 0 0.5rem;
  }

  .warn {
    color: #e0a35f;
  }

  .edit {
    padding: 0.6rem 0.75rem;
    margin: 0.4rem 0 0.75rem;
    background: #1e2127;
    border-left: 3px solid #2d5d8a;
  }

  .show-archived {
    grid-template-columns: auto 1fr;
  }

  button.link {
    padding: 0;
    background: none;
    color: #7ba7d4;
  }

  button.link:disabled {
    color: #4a5160;
    background: none;
  }

  h3 small {
    margin-left: 0.5rem;
    color: #6f757d;
    font-weight: normal;
  }

  .empty,
  .hint {
    color: #6f757d;
  }

  label {
    display: grid;
    grid-template-columns: 10rem 1fr;
    align-items: center;
    gap: 0.5rem 1rem;
    margin-bottom: 0.75rem;
  }

  label span {
    color: #9aa0a6;
  }

  label.check {
    grid-template-columns: auto 10rem 1fr;
    margin-bottom: 0.25rem;
  }

  .metric {
    font-family: monospace;
  }

  input,
  select {
    padding: 0.35rem 0.5rem;
    background: #1e2127;
    color: inherit;
    border: 1px solid #333842;
    border-radius: 3px;
    font: inherit;
  }

  .actions {
    display: flex;
    gap: 0.75rem;
    margin: 1rem 0;
  }

  button {
    padding: 0.4rem 1rem;
    background: #2d5d8a;
    color: #fff;
    border: none;
    border-radius: 3px;
    font: inherit;
    cursor: pointer;
  }

  button:disabled {
    background: #2a2e36;
    color: #6f757d;
    cursor: default;
  }

  p[role="alert"] {
    color: #e06c5f;
  }
</style>
