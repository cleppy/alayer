<script lang="ts">
  import type { TaskView } from "./api";
  import { bytes, fraction, kindLabel, percent, speed, stateLabel } from "./format";

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

  const width = $derived(`${Math.round(fraction(task.downloaded_bytes, task.total_bytes) * 1000) / 10}%`);
  const active = $derived(task.state === "downloading" || task.state === "queued");
  const canResume = $derived(
    task.state === "paused" || task.state === "failed" || task.state === "conflict",
  );
  const resumeLabel = $derived(task.state === "failed" || task.state === "conflict" ? "Yeniden dene" : "Sürdür");
  const waiting = $derived(task.state === "downloading" && task.downloaded_bytes === 0);
</script>

<article class="row" class:done={task.state === "completed"} class:bad={task.state === "failed" || task.state === "conflict"} data-kind={task.kind}>
  <p class="pct" aria-hidden="true">{percent(task.downloaded_bytes, task.total_bytes, task.state)}</p>
  <div class="body">
    <div class="topline">
      <h2>{task.file_name}</h2>
      <span class="chip">{kindLabel(task.kind)}</span>
      <span class="state">{waiting ? "çözülüyor" : stateLabel(task.state)}</span>
    </div>
    <div class="meter" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(fraction(task.downloaded_bytes, task.total_bytes) * 100)} aria-label={task.file_name}>
      <span style="width: {width}"></span>
    </div>
    <p class="meta">
      {bytes(task.downloaded_bytes)} / {bytes(task.total_bytes)}
      {#if task.state === "downloading" && rate > 0}
        · {speed(rate)}
      {/if}
      {#if task.failure}
        · {task.failure}
      {/if}
    </p>
  </div>
  <div class="actions">
    {#if active}
      <button type="button" onclick={() => onPause(task.id)}>Duraklat</button>
    {/if}
    {#if canResume}
      <button type="button" onclick={() => onResume(task.id)}>{resumeLabel}</button>
    {/if}
    <button type="button" class="quiet" onclick={() => onCancel(task.id)}>
      {task.state === "completed" ? "Kaldır" : "İptal"}
    </button>
  </div>
</article>
