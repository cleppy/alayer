<script lang="ts">
  import type { TaskView } from "./api";
  import { bytes, fraction, speed, stateLabel } from "./format";

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

  const pct = $derived(fraction(task.downloaded_bytes, task.total_bytes));
  const width = $derived(`${Math.round(pct * 1000) / 10}%`);
  const active = $derived(task.state === "downloading" || task.state === "queued");
  const canResume = $derived(
    task.state === "paused" || task.state === "failed" || task.state === "conflict",
  );
  const resumeLabel = $derived(task.state === "failed" ? "Yeniden dene" : "Sürdür");
</script>

<article class="strip" style="--fill: {width}" aria-label="{task.file_name}, {stateLabel(task.state)}">
  <div class="sprocket" aria-hidden="true"></div>
  <div class="frame">
    <div class="exposure"></div>
    <div class="copy light">
      <p class="name">{task.file_name}</p>
      <p class="meta">
        {stateLabel(task.state)}
        · {bytes(task.downloaded_bytes)} / {bytes(task.total_bytes)}
        {#if task.state === "downloading" && rate > 0}
          · {speed(rate)}
        {/if}
        {#if task.unresumable && active}
          · durunca baştan
        {/if}
      </p>
      {#if task.failure}
        <p class="fail">{task.failure}</p>
      {/if}
    </div>
    <div class="copy dark" aria-hidden="true">
      <p class="name">{task.file_name}</p>
      <p class="meta">
        {stateLabel(task.state)}
        · {bytes(task.downloaded_bytes)} / {bytes(task.total_bytes)}
        {#if task.state === "downloading" && rate > 0}
          · {speed(rate)}
        {/if}
      </p>
      {#if task.failure}
        <p class="fail">{task.failure}</p>
      {/if}
    </div>
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
