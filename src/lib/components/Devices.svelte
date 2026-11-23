<script lang="ts">
  import type { DeviceHealth, SourceStatus } from "$lib/api";
  import { age } from "$lib/format";
  import {
    formatRssi,
    formatVolts,
    silence,
    silenceText,
    sparklinePath,
    sparklinePoints,
  } from "$lib/home";

  // Shared by the home screen and the batch card: a device is the same device on both.
  let { devices, syncs }: { devices: DeviceHealth[]; syncs: Record<number, SourceStatus> } = $props();

  const W = 84;
  const H = 18;

  function range(points: [number, number][]): string {
    const vs = points.map((p) => p[1]);
    return `battery, last 7 days: ${formatVolts(Math.min(...vs))} – ${formatVolts(Math.max(...vs))}`;
  }
</script>

<!-- A grid, not a flex row per device: a device without a battery keeps its columns empty
     so the rows line up. -->
<ul>
  {#each devices as d (d.source_id)}
    {@const quiet = silence(d.stale, syncs[d.source_id])}
    <li>
      <span class="name">{d.name}</span>
      <span class:warn={d.battery?.warn} title="battery">
        {d.battery ? formatVolts(d.battery.value) : ""}
      </span>
      <span>
        {#if d.battery_7d.length > 1}
          {@const [endX, endY] = sparklinePoints(d.battery_7d, W - 4, H - 4).at(-1)!}
          <svg width={W} height={H} viewBox="0 0 {W} {H}" role="img" aria-label={range(d.battery_7d)}>
            <title>{range(d.battery_7d)}</title>
            <rect width={W} height={H} rx="2" />
            <g transform="translate(2 2)">
              <path d={sparklinePath(d.battery_7d, W - 4, H - 4)} />
              <circle cx={endX} cy={endY} r="1.75" />
            </g>
          </svg>
        {/if}
      </span>
      <span class:warn={d.rssi?.warn} title="signal">{d.rssi ? formatRssi(d.rssi.value) : ""}</span>
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
    display: grid;
    grid-template-columns: max-content 4rem 84px 5rem 1fr;
    gap: 0.25rem 0.75rem;
    align-items: center;
    list-style: none;
    margin: 0;
    padding: 0;
    font-variant-numeric: tabular-nums;
  }

  li {
    display: contents;
  }

  .name {
    min-width: 7rem;
  }

  svg {
    display: block;
  }

  svg rect {
    fill: #262a31;
  }

  svg path {
    fill: none;
    stroke: var(--accent-text);
    stroke-width: 1.25;
  }

  svg circle {
    fill: var(--accent-text);
  }

  .muted {
    color: var(--text-faint);
  }

  .warn,
  .silent {
    color: var(--warn);
  }

  .unreachable,
  .sync_error {
    color: var(--text-muted);
  }
</style>
