<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { open } from "@tauri-apps/plugin-dialog";
  import Detail from "./lib/Detail.svelte";
  import Strip from "./lib/Strip.svelte";
  import {
    addMedia,
    addTorrent,
    addUrl,
    cancel,
    inApp,
    listTasks,
    pause,
    prepareDownload,
    resume,
    subscribe,
    type FormatChoice,
    type TaskView,
    type UiEvent,
  } from "./lib/api";

  let tasks = $state<TaskView[]>([]);
  let rates = $state<Record<string, number>>({});
  let url = $state("");
  let error = $state("");
  let busy = $state(false);
  let dragging = $state(false);
  let looking = $state(false);
  let pendingUrl = $state("");
  let menuTitle = $state("");
  let choices = $state<FormatChoice[]>([]);

  const videos = $derived(choices.filter((c) => c.group === "video"));
  const audios = $derived(choices.filter((c) => c.group === "audio"));
  let version = $state<"split" | "plain">(
    localStorage.getItem("layer-ui") === "plain" ? "plain" : "split",
  );
  let filter = $state("all");
  let selectedId = $state("");

  const shown = $derived(
    tasks.filter((task) => {
      if (filter === "video") return task.kind === "media";
      if (filter === "torrent") return task.kind === "torrent";
      if (filter === "file") return task.kind === "http";
      return true;
    }),
  );
  const selected = $derived(shown.find((task) => task.id === selectedId) ?? shown[0]);

  function setVersion(next: "split" | "plain") {
    version = next;
    localStorage.setItem("layer-ui", next);
  }

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
    looking = true;
    choices = [];
    error = "";
    try {
      const prepared = await prepareDownload(value);
      if (prepared.action === "choose") {
        pendingUrl = value;
        menuTitle = prepared.title;
        choices = prepared.choices;
        looking = false;
        return;
      }
      await addUrl(value);
      url = "";
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
      looking = false;
    }
  }

  function dismissMenu() {
    choices = [];
    pendingUrl = "";
    menuTitle = "";
  }

  async function pick(choice: FormatChoice) {
    const target = pendingUrl;
    const title = menuTitle || "video";
    dismissMenu();
    url = "";
    error = "";
    try {
      await addMedia(target, title, choice.format, choice.audio_format);
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
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

<main class="shell" class:dragging>
  <header class="top">
    <p class="mark">layer</p>
    <div class="versions" aria-label="Görünüm">
      <button type="button" class:on={version === "split"} onclick={() => setVersion("split")}>1 Bölünmüş</button>
      <button type="button" class:on={version === "plain"} onclick={() => setVersion("plain")}>2 Sade</button>
    </div>
  </header>
  <form class="composer" onsubmit={submit}>
    <label class="sr" for="url">İndirme linki veya magnet</label>
    <input
      id="url"
      bind:value={url}
      placeholder="https://…  veya  magnet:?xt=…"
      autocomplete="off"
      spellcheck="false"
      disabled={!inApp || busy}
    />
    <button class="go" type="submit" disabled={!inApp || busy || url.trim() === ""}>İndir</button>
    <button class="file" type="button" onclick={pickTorrent} disabled={!inApp}>Torrent</button>
  </form>
  <div class="filters" aria-label="Süzgeç">
    <button type="button" class:on={filter === "all"} onclick={() => (filter = "all")}>Tümü</button>
    <button type="button" class:on={filter === "video"} onclick={() => (filter = "video")}>Video</button>
    <button type="button" class:on={filter === "torrent"} onclick={() => (filter = "torrent")}>Torrent</button>
    <button type="button" class:on={filter === "file"} onclick={() => (filter = "file")}>Dosya</button>
  </div>

  {#if looking}
    <p class="empty">Formatlar bakılıyor…</p>
  {/if}
  {#if choices.length > 0}
    <section class="deck" aria-label="Kalite">
      <div class="deck-head">
        <h2>{menuTitle}</h2>
        <button type="button" class="quiet" onclick={dismissMenu}>Vazgeç</button>
      </div>
      {#if videos.length > 0}
        <p class="deck-label">Görüntü · mp4</p>
        <div class="keys">
          {#each videos as choice (choice.id)}
            <button type="button" class="key" onclick={() => pick(choice)}>{choice.label}</button>
          {/each}
        </div>
      {/if}
      {#if audios.length > 0}
        <p class="deck-label">Ses</p>
        <div class="keys">
          {#each audios as choice (choice.id)}
            <button type="button" class="key audio" onclick={() => pick(choice)}>{choice.label}</button>
          {/each}
        </div>
      {/if}
    </section>
  {/if}

  {#if error}
    <p class="banner" role="alert">{error}</p>
  {/if}
  {#if !inApp}
    <p class="banner">Bu ekran uygulama penceresinde açılır.</p>
  {:else if tasks.length === 0}
    <p class="empty">Kuyruk boş. Link yapıştır, magnet yapıştır, ya da bir .torrent bırak.</p>
  {/if}

  <div class="layout" class:plain={version === "plain"}>
    <section class="list" aria-label="İndirme kuyruğu">
      {#each shown as task (task.id)}
        <Strip
          {task}
          rate={rates[task.id] ?? 0}
          selected={selected?.id === task.id}
          onOpen={(id) => (selectedId = id)}
          onPause={(id) => run(pause, id)}
          onResume={(id) => run(resume, id)}
          onCancel={(id) => run(cancel, id)}
        />
      {/each}
    </section>
    {#if version === "split" && selected}
      <Detail
        task={selected}
        rate={rates[selected.id] ?? 0}
        onPause={(id) => run(pause, id)}
        onResume={(id) => run(resume, id)}
        onCancel={(id) => run(cancel, id)}
      />
    {/if}
  </div>

  {#if dragging}
    <div class="drop">Torrent dosyasını bırak</div>
  {/if}
</main>
