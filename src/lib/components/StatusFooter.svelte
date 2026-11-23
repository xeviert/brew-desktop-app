<script lang="ts">
  import type { ApiError, DbStatus, SourceStatus, SourceSummary, SyncPhase } from "$lib/api";
  import { age } from "$lib/format";

  let {
    status,
    error,
    syncs,
    sources,
  }: {
    status: DbStatus | null;
    error: ApiError | null;
    syncs: Record<number, SourceStatus>;
    sources: SourceSummary[];
  } = $props();

  const rows = $derived(
    status ? Object.values(status.row_counts).reduce((a, b) => a + b, 0) : 0,
  );

  const all = $derived(Object.values(syncs));

  // The worst state wins. A footer that reported "syncing" while another source had
  // been failing for an hour would be technically true and useless. Unreachable ranks
  // below a failure: a host being off is expected, a source breaking is not.
  const phase = $derived<SyncPhase | null>(
    all.length === 0
      ? null
      : all.some((s) => s.phase === "error")
        ? "error"
        : all.some((s) => s.phase === "unreachable")
          ? "unreachable"
          : all.some((s) => s.phase === "syncing")
            ? "syncing"
            : "idle",
  );

  // Named when every unreachable source is a journal: then it is one Pi, not a guess.
  const brewdDown = $derived.by(() => {
    const down = all.filter((s) => s.phase === "unreachable");
    const kinds = new Map(sources.map((s) => [s.id, s.kind]));
    return down.length > 0 && down.every((s) => kinds.get(s.source_id) === "brewd_journal");
  });

  const labels: Record<SyncPhase, string> = {
    idle: "synced",
    syncing: "syncing…",
    error: "sync failing",
    unreachable: "source unreachable",
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
      <span class="sync {phase}">{phase === "unreachable" && brewdDown ? "brewd unreachable" : labels[phase]}</span>
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
    background: var(--surface);
    border-top: 1px solid var(--border);
    color: var(--text-muted);
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
    color: var(--danger);
  }

  .sync.idle {
    color: #5ba05b;
  }

  .sync.syncing {
    color: #5f9ae0;
  }

  .sync.error {
    color: var(--danger);
  }

  .sync.unreachable {
    color: var(--warn);
  }
</style>
