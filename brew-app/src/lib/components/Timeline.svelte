<script lang="ts">
  import { untrack } from "svelte";
  import EventEditor from "$lib/components/EventEditor.svelte";
  import QuickEntry from "$lib/components/QuickEntry.svelte";
  import {
    batchTimeline,
    deleteEvent,
    deleteMeasurement,
    setBatchFg,
    setBatchOg,
    toApiError,
    type ApiError,
    type BatchEvent,
    type Config,
    type Timeline,
    type TimelineItem,
  } from "$lib/api";
  import { confirmAction } from "$lib/confirm";
  import { entryDetail, eventLabel, formatValue, moments, sincePitch } from "$lib/timeline";
  import { formatTimestamp } from "$lib/units";

  // 3d/3e's one list: what was done to the batch and what was measured, newest first.
  // `revision` refetches; `onchanged` tells the host a write landed, so the card and
  // curve redraw too.
  let {
    batchId,
    config,
    readonly,
    revision,
    onchanged,
  }: { batchId: number; config: Config; readonly: boolean; revision: number; onchanged: () => void } = $props();

  let timeline = $state.raw<Timeline | null>(null);
  let error = $state<ApiError | null>(null);
  let open = $state<"reading" | "event" | null>(null);
  let editing = $state<BatchEvent | null>(null);
  let busy = $state(false);

  const groups = $derived(timeline ? moments(timeline.items) : []);

  async function load() {
    try {
      timeline = await batchTimeline(batchId);
    } catch (e) {
      error = toApiError(e);
    }
  }

  $effect(() => {
    void revision;
    void config.ui.gravity_display;
    untrack(() => void load());
  });

  function saved() {
    open = null;
    editing = null;
    error = null;
    onchanged();
  }

  async function attempt(write: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await write();
      onchanged();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = false;
    }
  }

  async function remove(item: TimelineItem) {
    const what =
      item.type === "measurement"
        ? `this ${formatValue(item, timeline?.gravity_unit ?? "SG", config.ui.temp_display)} reading`
        : `this ${eventLabel(item.kind).toLowerCase()}`;
    if (!(await confirmAction(`Delete ${what}? It comes off the curve too.`, "Delete"))) return;
    void attempt(() => (item.type === "measurement" ? deleteMeasurement(item.id) : deleteEvent(item.id)));
  }
</script>

<section class="timeline">
  <nav class="bar">
    <h3>Timeline</h3>
    {#if !readonly && open === null && editing === null}
      <button class="link" onclick={() => (open = "reading")}>Add reading</button>
      <button class="link" onclick={() => (open = "event")}>Add event</button>
    {/if}
  </nav>

  {#if open === "reading"}
    <QuickEntry {batchId} {config} onrecorded={saved} oncancel={() => (open = null)} />
  {:else if open === "event"}
    <EventEditor {batchId} event={null} onsaved={saved} oncancel={() => (open = null)} />
  {:else if editing}
    {#key editing.id}
      <EventEditor {batchId} event={editing} onsaved={saved} oncancel={() => (editing = null)} />
    {/key}
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}

  {#if timeline && groups.length === 0}
    <p class="empty">Nothing logged yet.</p>
  {/if}
  {#if timeline}
    {@const unit = timeline.gravity_unit}
    <ol>
      {#each groups as g (g.ts)}
        <li>
          <div class="when">
            {formatTimestamp(g.ts)}
            <span class="muted">{sincePitch(g.ts, timeline.pitched_at)}</span>
          </div>
          {#each g.measurements as m (m.id)}
            <div class="line">
              <span class="value">{formatValue(m, unit, config.ui.temp_display)}</span>
              <span class="muted">{entryDetail(m, config.ui.temp_display)}</span>
              {#if !readonly}
                <span class="actions">
                  {#if m.metric === "sg"}
                    <button class="link" disabled={busy} onclick={() => attempt(() => setBatchOg(batchId, m.value))}
                      >Use as OG</button
                    >
                    <button class="link" disabled={busy} onclick={() => attempt(() => setBatchFg(batchId, m.value))}
                      >Use as FG</button
                    >
                  {/if}
                  <button class="link" disabled={busy} onclick={() => remove(m)}>Delete</button>
                </span>
              {/if}
            </div>
          {/each}
          {#each g.events as e (e.id)}
            <div class="line">
              <span class="value">{eventLabel(e.kind)}</span>
              <span>{e.note ?? ""}</span>
              {#if !readonly && e.kind !== "pitch"}
                <span class="actions">
                  {#if e.kind !== "step_done" && e.kind !== "reminder_done"}
                    <button class="link" disabled={busy} onclick={() => ((open = null), (editing = e))}>Edit</button>
                  {/if}
                  <button class="link" disabled={busy} onclick={() => remove(e)}>Delete</button>
                </span>
              {/if}
            </div>
          {/each}
        </li>
      {/each}
    </ol>
  {/if}
</section>

<style>
  .timeline {
    max-width: 48rem;
    margin-top: 1.5rem;
  }

  .bar {
    display: flex;
    align-items: baseline;
    gap: 1rem;
  }

  h3 {
    margin: 0 0 0.5rem;
  }

  button.link {
    padding: 0;
    background: none;
    border: none;
    color: #7ba7d4;
    font: inherit;
    cursor: pointer;
  }

  button.link:disabled {
    color: #6f757d;
    cursor: default;
  }

  ol {
    list-style: none;
    padding: 0;
    margin: 0;
    border-left: 2px solid #333842;
  }

  li {
    padding: 0.4rem 0 0.6rem 1rem;
  }

  .when {
    font-size: 0.9rem;
    color: #9aa0a6;
  }

  .line {
    display: flex;
    gap: 0.75rem;
    align-items: baseline;
    padding: 0.1rem 0;
  }

  .value {
    min-width: 8rem;
  }

  .muted,
  .empty {
    color: #6f757d;
  }

  .actions {
    margin-left: auto;
    display: flex;
    gap: 0.75rem;
  }

  p[role="alert"] {
    color: #e06c5f;
  }
</style>
