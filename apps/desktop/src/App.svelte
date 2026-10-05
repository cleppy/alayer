<script lang="ts">
  import { onMount } from "svelte";
  import Strip from "./lib/Strip.svelte";
  import {
    addUrl,
    cancel,
    inApp,
    listTasks,
    pause,
    resume,
    subscribe,
    type TaskView,
    type UiEvent,
  } from "./lib/api";

  let tasks = $state<TaskView[]>([]);
  let rates = $state<Record<string, number>>({});
  let url = $state("");
  let error = $state("");
  let busy = $state(false);

  async function refresh() {
    tasks = await listTasks();
  }

  function apply(event: UiEvent) {
    if (event.type === "progress") {
      rates = { ...rates, [event.task_id]: event.speed_bps };
      tasks = tasks.map((task) =>
        task.id === event.task_id
          ? {
              ...task,
              downloaded_bytes: event.downloaded_bytes,
              total_bytes: event.total_bytes ?? task.total_bytes,
            }
          : task,
      );
      return;
    }
    if (event.type === "removed") {
      tasks = tasks.filter((task) => task.id !== event.task_id);
      return;
    }
    refresh().catch((err) => {
      error = String(err);
    });
  }

  onMount(() => {
    if (!inApp) return;
    refresh().catch((err) => {
      error = String(err);
    });
    let stop = false;
    let unlisten: (() => void) | undefined;
    subscribe(apply).then((fn) => {
      if (stop) fn();
      else unlisten = fn;
    });
    return () => {
      stop = true;
      unlisten?.();
    };
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    const value = url.trim();
    if (!value || busy) return;
    busy = true;
    error = "";
    try {
      await addUrl(value);
      url = "";
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function run(op: (id: string) => Promise<void>, id: string) {
    error = "";
    try {
      await op(id);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }
</script>

<main>
  <header>
    <p class="mark">layer</p>
    <form onsubmit={submit}>
      <label class="sr" for="url">İndirme linki</label>
      <input
        id="url"
        name="url"
        bind:value={url}
        placeholder="https://… veya magnet:"
        autocomplete="off"
        spellcheck="false"
        disabled={!inApp || busy}
      />
      <button type="submit" disabled={!inApp || busy || url.trim() === ""}>İndir</button>
    </form>
  </header>

  {#if error}
    <p class="banner" role="alert">{error}</p>
  {/if}

  {#if !inApp}
    <p class="banner">Bu ekran uygulama penceresinde açılır. <code>pnpm tauri dev</code></p>
  {:else if tasks.length === 0}
    <p class="empty">Kuyruk boş. Linki yukarı yapıştır.</p>
  {/if}

  <section class="queue" aria-label="İndirme kuyruğu">
    {#each tasks as task (task.id)}
      <Strip
        {task}
        rate={rates[task.id] ?? 0}
        onPause={(id) => run(pause, id)}
        onResume={(id) => run(resume, id)}
        onCancel={(id) => run(cancel, id)}
      />
    {/each}
  </section>
</main>
