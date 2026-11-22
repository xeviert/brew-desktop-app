<script lang="ts">
  import { ackAlert, snoozeAlert, toApiError, type AlertRow, type ApiError } from "$lib/api";
  import { alertState, alertTitle, isUrgent } from "$lib/alerts";
  import { age } from "$lib/format";
  import { href } from "$lib/route";

  // Ack and snooze go through `brewd`, which logs them; the change comes back with the
  // next alert round, which `onchanged` is told to expect.
  let {
    alerts,
    showBatch = false,
    onchanged,
  }: { alerts: AlertRow[]; showBatch?: boolean; onchanged: () => void } = $props();

  let busy = $state<number | null>(null);
  let error = $state<ApiError | null>(null);

  const now = $derived(alerts.length ? Date.now() / 1000 : 0);

  async function act(id: number, f: () => Promise<void>) {
    busy = id;
    error = null;
    try {
      await f();
      onchanged();
    } catch (e) {
      error = toApiError(e);
    } finally {
      busy = null;
    }
  }
</script>

<ul class="alerts">
  {#each alerts as a (a.id)}
    {@const open = a.cleared_at === null}
    <li class:urgent={open && isUrgent(a)} class:closed={!open}>
      <div class="head">
        <strong>{alertTitle(a)}</strong>
        {#if showBatch}
          <a href={href({ name: "batch", id: a.batch_id })}>{a.code}</a>
        {/if}
        <span class="muted">{age(a.raised_at)}</span>
      </div>
      <p>{a.detail}</p>
      <div class="foot">
        <span class="muted">{alertState(a, now)}</span>
        {#if open && a.acked_at === null}
          <span class="actions" role="group" aria-label="Respond to {alertTitle(a)}">
            <button type="button" disabled={busy !== null} onclick={() => act(a.id, () => ackAlert(a.id))}>
              Acknowledge
            </button>
            <button type="button" disabled={busy !== null} onclick={() => act(a.id, () => snoozeAlert(a.id, 4))}>
              Snooze 4 h
            </button>
            <button type="button" disabled={busy !== null} onclick={() => act(a.id, () => snoozeAlert(a.id, 24))}>
              Snooze 1 day
            </button>
          </span>
        {/if}
      </div>
    </li>
  {/each}
</ul>

{#if error}
  <p role="alert">
    {error.message}
    {#if error.code === "alert.token_unset" || error.code === "alert.write_refused"}
      — set brewd's token in <a href={href({ name: "settings" })}>Settings</a>.
    {/if}
  </p>
{/if}

<style>
  .alerts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.5rem;
  }

  li {
    padding: 0.5rem 0.75rem;
    background: #1e2127;
    border: 1px solid #333842;
    border-left: 3px solid #4a7aa8;
  }

  li.urgent {
    border-left-color: #e0a35f;
  }

  li.closed {
    border-left-color: #333842;
    opacity: 0.75;
  }

  .head,
  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem 0.75rem;
  }

  .head a,
  p[role="alert"] a {
    color: #7ba7d4;
  }

  .head .muted {
    margin-left: auto;
  }

  p {
    margin: 0.25rem 0;
  }

  .muted {
    color: #6f757d;
  }

  .actions {
    display: flex;
    gap: 0.4rem;
    margin-left: auto;
  }

  button {
    padding: 0.2rem 0.6rem;
    background: #2a2e36;
    color: #e6e6e6;
    border: 1px solid #333842;
    border-radius: 3px;
    font: inherit;
    cursor: pointer;
  }

  button:disabled {
    color: #6f757d;
    cursor: default;
  }

  p[role="alert"] {
    color: #e06c5f;
  }
</style>
