<script lang="ts">
  import { onMount } from "svelte";
  import Settings from "$lib/components/Settings.svelte";
  import StatusFooter from "$lib/components/StatusFooter.svelte";
  import {
    dbStatus,
    getConfig,
    toApiError,
    type ApiError,
    type Config,
    type DbStatus,
  } from "$lib/api";

  let config = $state<Config | null>(null);
  let status = $state<DbStatus | null>(null);
  let error = $state<ApiError | null>(null);

  // `onMount`, not `$effect`: these are one-shot loads with no reactive inputs. The
  // previous `$effect` worked only because it read no state, which is an invariant
  // nobody would notice breaking.
  onMount(async () => {
    try {
      [config, status] = await Promise.all([getConfig(), dbStatus()]);
    } catch (e) {
      error = toApiError(e);
    }
  });
</script>

<main>
  <h1>Brew</h1>

  {#if config}
    <Settings {config} onsaved={(c) => (config = c)} />
  {:else if !error}
    <p>Loading…</p>
  {/if}
</main>

<StatusFooter {status} {error} />
