<script lang="ts">
  import type { TaskView } from "./api";
  import { bytes, fraction, kindLabel, speed, stateLabel } from "./format";

  let {
    task,
    rate = 0,
    selected = false,
    onOpen,
    onPause,
    onResume,
    onCancel,
  }: {
    task: TaskView;
    rate?: number;
    selected?: boolean;
    onOpen: (id: string) => void;
    onPause: (id: string) => void;
    onResume: (id: string) => void;
    onCancel: (id: string) => void;
  } = $props();

  const width = $derived(
    task.state === "completed"
      ? "100%"
      : `${Math.round(fraction(task.downloaded_bytes, task.total_bytes) * 1000) / 10}%`,
  );
  const active = $derived(task.state === "downloading" || task.state === "queued");
  const canResume = $derived(
    task.state === "paused" || task.state === "failed" || task.state === "conflict",
  );
  const label = $derived(
    task.state === "completed"
      ? `Bitti · ${bytes(task.downloaded_bytes)}`
      : task.state === "downloading"
        ? rate > 0
          ? `${stateLabel(task.state)} · ${speed(rate)}`
          : "İniyor"
        : stateLabel(task.state),
  );
</script>

<article class="card" class:on={selected}>
  <button type="button" class="card-hit" onclick={() => onOpen(task.id)}>
    <h2>{task.file_name}</h2>
    <div class="tags">
      <span class="tag">{kindLabel(task.kind)}</span>
      <span class="tag">{stateLabel(task.state)}</span>
    </div>
    <p class="meta">{bytes(task.downloaded_bytes)} / {bytes(task.total_bytes)}</p>
  </button>
  <div class="dl" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={task.state === "completed" ? 100 : Math.round(fraction(task.downloaded_bytes, task.total_bytes) * 100)}>
    <span class="dl-fill" style:width={width}></span>
    <span class="dl-label">{label}</span>
  </div>
  <div class="row-actions">
    {#if active}
      <button type="button" onclick={() => onPause(task.id)}>Duraklat</button>
    {/if}
    {#if canResume}
      <button type="button" onclick={() => onResume(task.id)}>Sürdür</button>
    {/if}
    <button type="button" onclick={() => onCancel(task.id)}>
      {task.state === "completed" ? "Kaldır" : "İptal"}
    </button>
  </div>
</article>
