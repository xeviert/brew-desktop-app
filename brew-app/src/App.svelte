<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  type DbStatus = {
    path: string;
    size_bytes: number;
    schema_version: number;
    row_counts: Record<string, number>;
  };

  let status = $state<DbStatus | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    invoke<DbStatus>("db_status")
      .then((s) => (status = s))
      .catch((e) => (error = typeof e === "string" ? e : JSON.stringify(e)));
  });
</script>

<main>
  <h1>Brew</h1>

  {#if error}
    <p role="alert">{error}</p>
  {:else if status}
    <dl>
      <dt>Database</dt>
      <dd>{status.path}</dd>
      <dt>Schema version</dt>
      <dd>{status.schema_version}</dd>
      <dt>Size</dt>
      <dd>{(status.size_bytes / 1024).toFixed(1)} KiB</dd>
      {#each Object.entries(status.row_counts) as [table, count]}
        <dt>{table}</dt>
        <dd>{count}</dd>
      {/each}
    </dl>
  {:else}
    <p>Opening database…</p>
  {/if}
</main>
