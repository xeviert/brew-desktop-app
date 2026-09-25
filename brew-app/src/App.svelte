<script lang="ts">
  import { onMount } from "svelte";
  import Settings from "$lib/components/Settings.svelte";
  import Sources from "$lib/components/Sources.svelte";
  import StatusFooter from "$lib/components/StatusFooter.svelte";
  import Vessels from "$lib/components/Vessels.svelte";
  import {
    dbStatus,
    getConfig,
    listSources,
    onSyncStatus,
    syncStatus,
    toApiError,
    type ApiError,
    type Config,
    type DbStatus,
    type SourceStatus,
    type SourceSummary,
  } from "$lib/api";

  let config = $state<Config | null>(null);
  let status = $state<DbStatus | null>(null);
  let sources = $state<SourceSummary[]>([]);
  let error = $state<ApiError | null>(null);

  // Keyed by source id rather than merged into `sources`: the two have different
  // lifetimes. A summary is a database row; a status is a running task, and a source
  // added while the scheduler was down has the first and not the second.
  let syncs = $state<Record<number, SourceStatus>>({});

  // `onMount`, not `$effect`: these are one-shot loads with no reactive inputs. The
  // previous `$effect` worked only because it read no state, which is an invariant
  // nobody would notice breaking.
  //
  // `allSettled`, not `all`: these share a screen and nothing else. Settings come from
  // a TOML file and `db_status` is the only one that can fail, so `all` meant a
  // transient SQLite error hid the settings pane — which the database has no part in.
  onMount(() => {
    // Subscribed before the first fetch, not after: a poll that finishes in between
    // would otherwise be missed, and the row would sit at its launch state until the
    // next tick.
    const unlisten = onSyncStatus((s) => {
      syncs = { ...syncs, [s.source_id]: s };
    });

    void (async () => {
      const [loaded, queried, listed, synced] = await Promise.allSettled([
        getConfig(),
        dbStatus(),
        listSources(),
        syncStatus(),
      ]);

      if (loaded.status === "fulfilled") config = loaded.value;
      else error = toApiError(loaded.reason);

      if (listed.status === "fulfilled") sources = listed.value;

      if (synced.status === "fulfilled") {
        // Seeded *under* anything the subscription already delivered: the snapshot is
        // older than any event that arrived while it was in flight.
        const seeded: Record<number, SourceStatus> = {};
        for (const s of synced.value) seeded[s.source_id] = s;
        syncs = { ...seeded, ...syncs };
      }

      if (queried.status === "fulfilled") status = queried.value;
      // One slot, so the first failure wins: the footer is a status line, not an error
      // console, and the config failure is the one that blanks the page.
      else error ??= toApiError(queried.reason);
    })();

    return unlisten;
  });
</script>

<main>
  <h1>Brew</h1>

  {#if config}
    <Settings {config} onsaved={(c) => (config = c)} />
    <Sources {sources} {syncs} onchanged={(s) => (sources = s)} />
    <Vessels />
  {:else if error}
    <!-- Without this, a failed `get_config` renders an empty page under the heading
         and one red line in the footer. -->
    <p role="alert">Settings unavailable: {error.message}</p>
  {:else}
    <p>Loading…</p>
  {/if}
</main>

<StatusFooter {status} {error} {syncs} {sources} />
