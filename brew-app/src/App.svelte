<script lang="ts">
  import { onMount } from "svelte";
  import Settings from "$lib/components/Settings.svelte";
  import Sources from "$lib/components/Sources.svelte";
  import StatusFooter from "$lib/components/StatusFooter.svelte";
  import {
    dbStatus,
    getConfig,
    listSources,
    toApiError,
    type ApiError,
    type Config,
    type DbStatus,
    type SourceSummary,
  } from "$lib/api";

  let config = $state<Config | null>(null);
  let status = $state<DbStatus | null>(null);
  let sources = $state<SourceSummary[]>([]);
  let error = $state<ApiError | null>(null);

  // `onMount`, not `$effect`: these are one-shot loads with no reactive inputs. The
  // previous `$effect` worked only because it read no state, which is an invariant
  // nobody would notice breaking.
  //
  // `allSettled`, not `all`: these two share a screen and nothing else. Settings come
  // from a TOML file and `db_status` is the only one of the pair that can fail, so
  // `all` meant a transient SQLite error hid the settings pane — which the database
  // has no part in. Phase 2g adds more calls here, so the coupling would widen.
  onMount(async () => {
    const [loaded, queried, listed] = await Promise.allSettled([
      getConfig(),
      dbStatus(),
      listSources(),
    ]);

    if (loaded.status === "fulfilled") config = loaded.value;
    else error = toApiError(loaded.reason);

    if (listed.status === "fulfilled") sources = listed.value;

    if (queried.status === "fulfilled") status = queried.value;
    // One slot, so the first failure wins: the footer is a status line, not an error
    // console, and the config failure is the one that blanks the page.
    else error ??= toApiError(queried.reason);
  });
</script>

<main>
  <h1>Brew</h1>

  {#if config}
    <Settings {config} onsaved={(c) => (config = c)} />
    <Sources {sources} onchanged={(s) => (sources = s)} />
  {:else if error}
    <!-- Without this, a failed `get_config` renders an empty page under the heading
         and one red line in the footer. -->
    <p role="alert">Settings unavailable: {error.message}</p>
  {:else}
    <p>Loading…</p>
  {/if}
</main>

<StatusFooter {status} {error} />
