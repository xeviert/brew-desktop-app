<script lang="ts">
  import { onMount } from "svelte";
  import BatchNew from "$lib/components/BatchNew.svelte";
  import BatchView from "$lib/components/BatchView.svelte";
  import BatchesView from "$lib/components/BatchesView.svelte";
  import CompareView from "$lib/components/CompareView.svelte";
  import HomeView from "$lib/components/HomeView.svelte";
  import IngredientsView from "$lib/components/IngredientsView.svelte";
  import RecipeDiff from "$lib/components/RecipeDiff.svelte";
  import RecipeView from "$lib/components/RecipeView.svelte";
  import RecipesView from "$lib/components/RecipesView.svelte";
  import QueryView from "$lib/components/QueryView.svelte";
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
  import { href, parse } from "$lib/route";

  let config = $state<Config | null>(null);
  let status = $state<DbStatus | null>(null);
  let sources = $state<SourceSummary[]>([]);
  let error = $state<ApiError | null>(null);

  // Keyed by source id rather than merged into `sources`: the two have different
  // lifetimes. A summary is a database row; a status is a running task, and a source
  // added while the scheduler was down has the first and not the second.
  let syncs = $state<Record<number, SourceStatus>>({});

  // The last status per source that wasn't mid-poll. A poll's `syncing` replaces the
  // row wholesale, so "brewd unreachable" would flicker to "sensor silent" on every
  // retry; staleness is judged against how the last poll ended.
  let settled = $state<Record<number, SourceStatus>>({});

  let route = $state(parse(location.hash));

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
      if (s.phase !== "syncing") settled = { ...settled, [s.source_id]: s };
    });

    const onHash = () => (route = parse(location.hash));
    window.addEventListener("hashchange", onHash);

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
        const done = Object.fromEntries(Object.entries(seeded).filter(([, s]) => s.phase !== "syncing"));
        settled = { ...done, ...settled };
      }

      if (queried.status === "fulfilled") status = queried.value;
      // One slot, so the first failure wins: the footer is a status line, not an error
      // console, and the config failure is the one that blanks the page.
      else error ??= toApiError(queried.reason);
    })();

    return () => {
      unlisten();
      window.removeEventListener("hashchange", onHash);
    };
  });
</script>

<main>
  <header>
    <h1>Brew</h1>
    <nav>
      <a href={href({ name: "home" })} class:active={route.name === "home"}>Home</a>
      <a
        href={href({ name: "batches" })}
        class:active={route.name === "batches" || route.name === "batch-new" || route.name === "batch"}>Batches</a
      >
      <a href={href({ name: "compare", ids: [] })} class:active={route.name === "compare"}>Compare</a>
      <a href={href({ name: "query", id: null })} class:active={route.name === "query"}>Queries</a>
      <a href={href({ name: "recipes" })} class:active={route.name === "recipes" || route.name === "recipe" || route.name === "recipe-diff"}
        >Recipes</a
      >
      <a href={href({ name: "ingredients", tab: "ingredients" })} class:active={route.name === "ingredients"}
        >Ingredients</a
      >
      <a href={href({ name: "settings" })} class:active={route.name === "settings"}>Settings</a>
    </nav>
  </header>

  {#if config}
    {#if route.name === "home"}
      <HomeView ui={config.ui} syncs={settled} />
    {:else if route.name === "batches"}
      <BatchesView />
    {:else if route.name === "batch-new"}
      {#key route.version}
        <BatchNew version={route.version} gravity={config.ui.gravity_display} />
      {/key}
    {:else if route.name === "batch"}
      <BatchView {config} batchId={route.id} syncs={settled} />
    {:else if route.name === "compare"}
      <CompareView ids={route.ids} ui={config.ui} />
    {:else if route.name === "query"}
      <QueryView id={route.id} preset={route.preset} ui={config.ui} />
    {:else if route.name === "ingredients"}
      <IngredientsView tab={route.tab} {config} />
    {:else if route.name === "recipes"}
      <RecipesView />
    {:else if route.name === "recipe"}
      <RecipeView id={route.id} version={route.version} />
    {:else if route.name === "recipe-diff"}
      <RecipeDiff id={route.id} from={route.from} to={route.to} />
    {:else}
      <Settings {config} onsaved={(c) => (config = c)} />
      <Sources {sources} {syncs} onchanged={(s) => (sources = s)} />
      <Vessels />
    {/if}
  {:else if error}
    <!-- Without this, a failed `get_config` renders an empty page under the heading
         and one red line in the footer. -->
    <p role="alert">Settings unavailable: {error.message}</p>
  {:else}
    <p>Loading…</p>
  {/if}
</main>

<StatusFooter {status} {error} {syncs} {sources} />

<style>
  header {
    display: flex;
    align-items: baseline;
    gap: 1.5rem;
  }

  nav {
    display: flex;
    gap: 1rem;
  }

  nav a {
    color: #9aa0a6;
    text-decoration: none;
  }

  nav a.active {
    color: inherit;
    border-bottom: 2px solid #2d5d8a;
  }
</style>
