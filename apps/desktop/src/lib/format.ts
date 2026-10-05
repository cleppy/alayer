const UNITS = ["B", "KB", "MB", "GB", "TB"];

export function bytes(n: number | null | undefined): string {
  if (n == null || Number.isNaN(n)) return "—";
  let v = n;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i += 1;
  }
  const digits = i === 0 || v >= 10 ? 0 : 1;
  return `${v.toFixed(digits)} ${UNITS[i]}`;
}

export function speed(bps: number): string {
  if (!bps || bps < 1) return "";
  return `${bytes(bps)}/s`;
}

const STATE: Record<string, string> = {
  queued: "sırada",
  downloading: "iniyor",
  paused: "durdu",
  completed: "bitti",
  failed: "hata",
  conflict: "çakıştı",
};

export function stateLabel(state: string): string {
  return STATE[state] ?? state;
}

export function fraction(done: number, total: number | null): number {
  if (!total || total <= 0) return done > 0 ? 0.08 : 0;
  return Math.max(0, Math.min(1, done / total));
}
