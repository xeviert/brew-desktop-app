<script lang="ts">
  import type { ApiError, DbStatus, SourceStatus, SyncPhase } from "$lib/api";
  import { age } from "$lib/format";

  let {
    status,
    error,
    syncs,
  }: {
    status: DbStatus | null;
    error: ApiError | null;
    syncs: Record<number, SourceStatus>;
  } = $props();

  const rows = $derived(
    status ? Object.values(status.row_counts).reduce((a, b) => a + b, 0) : 0,
  );

  const all = $derived(Object.values(syncs));

  // The worst state wins. A footer that reported "syncing" while another source had
  // been failing for an hour would be technically true and useless.
  const phase = $derived<SyncPhase | null>(
    all.length === 0
      ? null
      : all.some((s) => s.phase === "error")
        ? "error"
        : all.some((s) => s.phase === "syncing")
          ? "syncing"
          : "idle",
  );

  const labels: Record<SyncPhase, string> = {
    idle: "synced",
    syncing: "syncing…",
    error: "sync failing",
  };

  /** Newest reading across every source: the one number that answers "is data coming in". */
  const newest = $derived(
    all.reduce<number | null>(
      (max, s) => (s.newest_reading !== null && (max === null || s.newest_reading > max)
        ? s.newest_reading
        : max),
      null,
    ),
  );
</script>

<footer>
  {#if error}
    <span class="err" role="alert">{error.code}: {error.message}</span>
  {:else if status}
    <span>schema v{status.schema_version}</span>
    <span>{rows} rows</span>
    <span>{(status.size_bytes / 1024).toFixed(1)} KiB</span>
    {#if phase}
      <span class="sync {phase}">{labels[phase]}</span>
      <span>{all.length} source{all.length === 1 ? "" : "s"}</span>
      <span title="age of the newest reading">data {age(newest)}</span>
    {/if}
    <span class="path" title={status.path}>{status.path}</span>
  {:else}
    <span>Opening database…</span>
  {/if}
</footer>

<style>
  footer {
    position: sticky;
    bottom: 0;
    display: flex;
    gap: 1rem;
    align-items: baseline;
    padding: 0.4rem 1.5rem;
    background: #1b1e24;
    border-top: 1px solid #2a2e36;
    color: #9aa0a6;
    font-variant-numeric: tabular-nums;
  }

  .path {
    margin-left: auto;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    direction: rtl;
  }

  .err {
    color: #e06c5f;
  }

  .sync.idle {
    color: #5ba05b;
  }

  .sync.syncing {
    color: #5f9ae0;
  }

  .sync.error {
    color: #e06c5f;
  }
</style>
