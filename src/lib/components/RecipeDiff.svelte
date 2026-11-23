<script lang="ts">
  import { diffRecipeVersions, getRecipe, toApiError, type ApiError, type RecipeDiff } from "$lib/api";
  import { describeChange, formatTarget } from "$lib/recipe";
  import { href } from "$lib/route";

  /** `from` and `to` are version numbers, as the route carries them. */
  let { id, from, to }: { id: number; from: number; to: number } = $props();

  let name = $state("");
  let diff = $state<RecipeDiff | null>(null);
  let error = $state<ApiError | null>(null);

  const LABELS = { added: "Added", removed: "Removed", changed: "Changed" } as const;

  $effect(() => {
    const [recipeId, a, b] = [id, from, to];
    void (async () => {
      try {
        const detail = await getRecipe(recipeId);
        name = detail.recipe.name;
        const ids = (n: number) => detail.versions.find((v) => v.version_no === n)?.id;
        const [fromId, toId] = [ids(a), ids(b)];
        if (fromId === undefined || toId === undefined) {
          error = { code: "route", message: `No v${fromId === undefined ? a : b} on this recipe` };
          return;
        }
        diff = await diffRecipeVersions(fromId, toId);
        error = null;
      } catch (e) {
        error = toApiError(e);
      }
    })();
  });
</script>

<section class="ref">
  <p class="crumbs">
    <a href={href({ name: "recipes" })}>Recipes</a> ›
    <a href={href({ name: "recipe", id, version: to })}>{name}</a>
  </p>
  <h2>v{from} → v{to}</h2>

  {#if diff}
    {#if diff.targets.length === 0 && diff.items.length === 0}
      <p class="empty">No differences.</p>
    {/if}

    {#if diff.targets.length > 0}
      <h3>Targets</h3>
      <table>
        <tbody>
          {#each diff.targets as t (t.field)}
            {@const f = formatTarget(t, diff.gravity_unit)}
            <tr>
              <th>{f.label}</th>
              <td>{f.from}</td>
              <td>→</td>
              <td>{f.to}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    {#if diff.items.length > 0}
      <h3>Lines</h3>
      <table>
        <tbody>
          {#each diff.items as c, i (i)}
            <tr class={c.change}>
              <td class="change">{LABELS[c.change]}</td>
              <td>{c.ingredient}</td>
              <td class="stage">{c.stage}</td>
              <td>{describeChange(c)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}

  {#if error}
    <p role="alert">{error.message}</p>
  {/if}
</section>

<style>
  .crumbs {
    margin: 0;
    color: var(--text-faint);
  }

  .crumbs a {
    color: var(--accent-text);
  }

  table {
    border-collapse: collapse;
  }

  th,
  td {
    padding: 0.25rem 1.25rem 0.25rem 0;
    text-align: left;
    font-variant-numeric: tabular-nums;
  }

  th,
  .stage {
    color: var(--text-muted);
    font-weight: normal;
  }

  .change {
    font-size: 0.85rem;
  }

  .added .change {
    color: var(--ok);
  }

  .removed .change {
    color: var(--danger);
  }

  .changed .change {
    color: var(--warn);
  }
</style>
