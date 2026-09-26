<script lang="ts">
  import type { DeviceHealth, SourceStatus } from "$lib/api";
  import { age } from "$lib/format";
  import { formatRssi, formatVolts, silence, silenceText, sparklinePath } from "$lib/home";

  // Shared by the home screen and the batch card: a device is the same device on both.
  let { devices, syncs }: { devices: DeviceHealth[]; syncs: Record<number, SourceStatus> } = $props();

  const W = 84;
  const H = 18;
</script>

<ul>
  {#each devices as d (d.source_id)}
    {@const quiet = silence(d.stale, syncs[d.source_id])}
    <li>
      <span class="name">{d.name}</span>
      {#if d.battery}
        <span class:warn={d.battery.warn} title="battery">{formatVolts(d.battery.value)}</span>
      {/if}
      {#if d.battery_7d.length > 1}
        <svg width={W} height={H} viewBox="0 0 {W} {H}" aria-label="battery, last 7 days">
          <path d={sparklinePath(d.battery_7d, W, H - 2)} transform="translate(0 1)" />
        </svg>
      {/if}
      {#if d.rssi}
        <span class:warn={d.rssi.warn} title="signal">{formatRssi(d.rssi.value)}</span>
      {/if}
      {#if quiet === "fresh"}
        <span class="muted">{age(d.newest)}</span>
      {:else}
        <span class={quiet}>{silenceText(quiet, d.guess)}, last {age(d.newest)}</span>
      {/if}
    </li>
  {/each}
</ul>

<style>
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  li {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    font-variant-numeric: tabular-nums;
  }

  .name {
    min-width: 7rem;
  }

  svg path {
    fill: none;
    stroke: #7ba7d4;
    stroke-width: 1.25;
  }

  .muted {
    color: #6f757d;
  }

  .warn,
  .silent {
    color: #e0a35f;
  }

  .unreachable,
  .sync_error {
    color: #9aa0a6;
  }
</style>
