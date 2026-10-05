import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export const inApp =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export type TaskView = {
  id: string;
  kind: string;
  state: string;
  url: string;
  file_name: string;
  dest_dir: string;
  downloaded_bytes: number;
  total_bytes: number | null;
  failure: string | null;
  unresumable: boolean;
};

export type UiEvent =
  | { type: "added"; task_id: string; kind: string }
  | { type: "state"; task_id: string; state: string; failure: string | null }
  | {
      type: "progress";
      task_id: string;
      downloaded_bytes: number;
      total_bytes: number | null;
      speed_bps: number;
    }
  | { type: "removed"; task_id: string }
  | { type: "updated"; task_id: string };

export function listTasks(): Promise<TaskView[]> {
  return invoke("list_tasks");
}

export function addUrl(url: string): Promise<string> {
  return invoke("add_url", { url });
}

export function pause(id: string): Promise<void> {
  return invoke("pause", { id });
}

export function resume(id: string): Promise<void> {
  return invoke("resume", { id });
}

export function cancel(id: string): Promise<void> {
  return invoke("cancel", { id });
}

export function subscribe(onEvent: (event: UiEvent) => void): Promise<UnlistenFn> {
  return listen<UiEvent>("layer", (msg) => onEvent(msg.payload));
}
