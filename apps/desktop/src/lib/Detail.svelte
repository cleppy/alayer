<script lang="ts">
  import type { TaskView } from "./api";
  import { bytes, fraction, kindLabel, speed, stateLabel } from "./format";

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

  const width = $derived(
    task.state === "completed"
      ? "100%"
      : `${Math.round(fraction(task.downloaded_bytes, task.total_bytes) * 1000) / 10}%`,
  );
  const active = $derived(task.state === "downloading" || task.state === "queued");
  const canResume = $derived(
    task.state === "paused" || task.state === "failed" || task.state === "conflict",
  );
</script>

<aside class="detail">
  <div class="banner-stripe"></div>
  <div class="detail-body">
    <div class="tags">
      <span class="tag">{kindLabel(task.kind)}</span>
      <span class="tag">{stateLabel(task.state)}</span>
    </div>
    <h2>{task.file_name}</h2>
    <p class="meta">
      {bytes(task.downloaded_bytes)} / {bytes(task.total_bytes)}
      {#if rate > 0}
        · {speed(rate)}
      {/if}
    </p>
    {#if task.failure}
      <p class="banner">{task.failure}</p>
    {/if}
    <button
      type="button"
      class="dl"
      onclick={() => (active ? onPause(task.id) : canResume ? onResume(task.id) : onCancel(task.id))}
    >
      <span class="dl-fill" style:width={width}></span>
      <span class="dl-label">
        {#if active}
          Duraklat
        {:else if canResume}
          Sürdür
        {:else if task.state === "completed"}
          Kaldır
        {:else}
          {stateLabel(task.state)}
        {/if}
      </span>
    </button>
    <div class="detail-actions">
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
  </div>
</aside>
