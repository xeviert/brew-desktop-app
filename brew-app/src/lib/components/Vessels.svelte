<script lang="ts">
  import { onMount } from "svelte";
  import {
    createVessel,
    listStreams,
    listVessels,
    setVesselStreams,
    toApiError,
    type ApiError,
    type Stream,
    type Vessel,
    type VesselKind,
  } from "$lib/api";

  let vessels = $state<Vessel[]>([]);
  let streams = $state<Stream[]>([]);
  let error = $state<ApiError | null>(null);
  let busy = $state(false);

  let name = $state("");
  let kind = $state<VesselKind>("fermenter");
  let capacity = $state<number | null>(null);

  // Manual streams are left out: they belong to a batch, not to the vessel it is in,
  // and packaging deliberately leaves them bound.
  const sensors = $derived(streams.filter((s) => s.source_kind !== "manual"));

  async function load() {
    try {
      [vessels, streams] = await Promise.all([listVessels(), listStreams()]);
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

<section>
  <h2>Vessels</h2>

  {#if vessels.length === 0}
    <p class="empty">No vessels yet.</p>
  {:else}
    {#each vessels as vessel (vessel.id)}
      <div class="vessel">
        <h3>
          {vessel.name}
          <small>{vessel.kind}{vessel.capacity_l !== null ? ` · ${vessel.capacity_l} L` : ""}</small>
        </h3>
        {#if sensors.length === 0}
          <p class="empty">No sensor streams yet — they appear once a source has synced.</p>
        {:else}
          <p class="hint">Bound to a batch from its pitch until it is packaged.</p>
          {#each sensors as stream (stream.id)}
            <label class="check">
              <input
                type="checkbox"
                checked={vessel.default_streams.includes(stream.id)}
                onchange={(e) => toggle(vessel, stream.id, e.currentTarget.checked)}
              />
              <span>{stream.source_name}</span>
              <span class="metric">{stream.metric}</span>
            </label>
          {/each}
        {/if}
      </div>
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
      <option value="fermenter">Fermenter</option>
      <option value="brite">Brite tank</option>
      <option value="carboy">Carboy</option>
      <option value="keg">Keg</option>
      <option value="kettle">Kettle</option>
      <option value="other">Other</option>
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
