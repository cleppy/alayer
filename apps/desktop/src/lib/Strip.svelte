<script lang="ts">
  import type { TaskView } from "./api";
  import { cubeCount, fraction, kindLabel, megabytes, speed, stateLabel } from "./format";

  let {
    task,
    rate = 0,
    onPause,
    onResume,
    onCancel,
  }: {
    task: TaskView;
    rate?: number;
    onPause: (id: string) => void;
    onResume: (id: string) => void;
    onCancel: (id: string) => void;
  } = $props();

  const ratio = $derived(
    task.state === "completed" ? 1 : fraction(task.downloaded_bytes, task.total_bytes),
  );
  const count = $derived(cubeCount(task.total_bytes));
  const filled = $derived(Math.round(ratio * count));
  const left = $derived(
    task.total_bytes == null
      ? null
      : Math.max(0, task.total_bytes - task.downloaded_bytes),
  );
  const pct = $derived(
    task.state === "completed"
      ? "100%"
      : task.total_bytes
        ? `${Math.min(100, Math.round(ratio * 100))}%`
        : "—",
  );
  const active = $derived(task.state === "downloading" || task.state === "queued");
  const canResume = $derived(
    task.state === "paused" || task.state === "failed" || task.state === "conflict",
  );
</script>

<article class="card">
  <div class="card-top">
    <h2>{task.file_name}</h2>
    <span class="tag">{kindLabel(task.kind)}</span>
  </div>
  <p class="stat">
    {megabytes(left)} left / {megabytes(task.total_bytes)}
    <span>{pct}</span>
    {#if rate > 0}
      <span class="dim">{speed(rate)}</span>
    {/if}
  </p>
  <div class="cubes" aria-hidden="true">
    {#each Array(count) as _, i (i)}
      <span class="cube" class:on={i < filled} class:now={active && i === filled}></span>
    {/each}
  </div>
  <p class="meta">{stateLabel(task.state)}{#if task.failure} · {task.failure}{/if}</p>
  <div class="row-actions">
    {#if active}
      <button type="button" onclick={() => onPause(task.id)}>Pause</button>
    {/if}
    {#if canResume}
      <button type="button" onclick={() => onResume(task.id)}>Resume</button>
    {/if}
    <button type="button" onclick={() => onCancel(task.id)}>
      {task.state === "completed" ? "Remove" : "Cancel"}
    </button>
  </div>
</article>
