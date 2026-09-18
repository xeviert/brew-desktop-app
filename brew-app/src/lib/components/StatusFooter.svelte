<script lang="ts">
  import type { ApiError, DbStatus } from "$lib/api";

  // Phase 2g adds per-source sync state here; the layout is built to grow a second
  // group rather than to be replaced.
  let { status, error }: { status: DbStatus | null; error: ApiError | null } = $props();

  const rows = $derived(
    status ? Object.values(status.row_counts).reduce((a, b) => a + b, 0) : 0,
  );
</script>

<footer>
  {#if error}
    <span class="err" role="alert">{error.code}: {error.message}</span>
  {:else if status}
    <span>schema v{status.schema_version}</span>
    <span>{rows} rows</span>
    <span>{(status.size_bytes / 1024).toFixed(1)} KiB</span>
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
</style>
