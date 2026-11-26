// Number formatting for people, not programmers.

/** "22 min 20 s", "1 h 3 min", "2.3 s". */
export function duration(seconds: number): string {
  if (!isFinite(seconds) || seconds < 0) return "unknown";
  if (seconds < 10) return `${seconds.toFixed(1)} s`;
  const s = Math.round(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rest = s % 60;
  if (h > 0) return `${h} h ${m} min`;
  if (m > 0) return rest ? `${m} min ${rest} s` : `${m} min`;
  return `${rest} s`;
}

/** Decimal units, like Finder and Explorer's "size on disk" labels: "64 MB", "37.8 GB". */
export function bytes(n: number): string {
  if (n < 1e3) return `${n} bytes`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n;
  let i = -1;
  do {
    v /= 1000;
    i++;
  } while (v >= 1000 && i < units.length - 1);
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

/** "about 3 min left", "a few seconds left". */
export function eta(seconds: number): string {
  if (!isFinite(seconds)) return "";
  if (seconds < 10) return "a few seconds left";
  return `about ${duration(seconds)} left`;
}

/** Last part of a path, for either slash. */
export function baseName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

/** Folder part of a path. */
export function dirName(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : path;
}
