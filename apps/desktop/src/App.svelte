<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { open } from "@tauri-apps/plugin-dialog";
  import Strip from "./lib/Strip.svelte";
  import {
    addTorrent,
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
  let dragging = $state(false);

  async function refresh() {
    const next = await listTasks();
    tasks = next.map((task) => {
      const prev = tasks.find((item) => item.id === task.id);
      if (!prev) return task;
      if (task.downloaded_bytes < prev.downloaded_bytes) {
        return {
          ...task,
          downloaded_bytes: prev.downloaded_bytes,
          total_bytes: task.total_bytes ?? prev.total_bytes,
        };
      }
      return task;
    });
  }

  function apply(event: UiEvent) {
    if (event.type === "progress") {
      rates = { ...rates, [event.task_id]: event.speed_bps };
      tasks = tasks.map((task) =>
        task.id === event.task_id
          ? {
              ...task,
              downloaded_bytes: Math.max(task.downloaded_bytes, event.downloaded_bytes),
              total_bytes: event.total_bytes ?? task.total_bytes,
              state: task.state === "queued" ? "downloading" : task.state,
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

  async function addPath(path: string) {
    error = "";
    try {
      if (path.toLowerCase().endsWith(".torrent")) {
        await addTorrent(path);
      } else {
        error = "Bırakılan dosya .torrent değil.";
        return;
      }
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  onMount(() => {
    if (!inApp) return;
    refresh().catch((err) => {
      error = String(err);
    });
    let stop = false;
    let unlisten: (() => void) | undefined;
    let undrop: (() => void) | undefined;
    subscribe(apply).then((fn) => {
      if (stop) fn();
      else unlisten = fn;
    });
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "drop") {
          dragging = false;
          for (const path of event.payload.paths) addPath(path);
        } else if (event.payload.type === "leave") {
          dragging = false;
        } else {
          dragging = true;
        }
      })
      .then((fn) => {
        if (stop) fn();
        else undrop = fn;
      });
    return () => {
      stop = true;
      unlisten?.();
      undrop?.();
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

  async function pickTorrent() {
    error = "";
    const selected = await open({
      multiple: false,
      filters: [{ name: "Torrent", extensions: ["torrent"] }],
    });
    if (typeof selected === "string") await addPath(selected);
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

<main class:dragging>
  <header>
    <div class="brand">
      <p class="mark">layer</p>
      <p class="lede">Link, magnet veya torrent. İlerleme burada durur.</p>
    </div>
    <form onsubmit={submit}>
      <label class="sr" for="url">İndirme linki veya magnet</label>
      <input
        id="url"
        bind:value={url}
        placeholder="https://…  veya  magnet:?xt=…"
        autocomplete="off"
        spellcheck="false"
        disabled={!inApp || busy}
      />
      <button type="submit" disabled={!inApp || busy || url.trim() === ""}>İndir</button>
      <button type="button" class="file" onclick={pickTorrent} disabled={!inApp}>Torrent dosyası</button>
    </form>
  </header>

  {#if error}
    <p class="banner" role="alert">{error}</p>
  {/if}
  {#if !inApp}
    <p class="banner">Bu ekran uygulama penceresinde açılır.</p>
  {:else if tasks.length === 0}
    <p class="empty">Kuyruk boş. Link yapıştır, magnet yapıştır, ya da bir .torrent bırak.</p>
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

  {#if dragging}
    <div class="drop">Torrent dosyasını bırak</div>
  {/if}
</main>
