// Typed wrappers around the Rust commands in src-tauri/src/commands.rs.
// The shapes mirror the structs in src-tauri/src/engine.rs (serde camelCase).

import { Channel, invoke } from "@tauri-apps/api/core";

export interface Capabilities {
  referenceFree: boolean;
}

export interface AppInfo {
  name: string;
  version: string;
  os: "macos" | "windows" | "linux" | string;
  capabilities: Capabilities;
}

export interface DatSummary {
  path: string;
  fileName: string;
  sizeBytes: number;
  camera: string;
  contents: string;
  estimatedSeconds: number | null;
  videoFrames: number;
  cutOff: boolean;
}

export interface ReferenceMatch {
  path: string;
  ok: boolean;
  reasons: string[];
  checked: number;
}

export interface RepairOutcome {
  output: string;
  reference: string;
  durationSeconds: number;
  videoFrames: number;
  audioTracks: number;
  bytesWritten: number;
  notes: string[];
}

export type Stage = "indexing" | "checkingReference" | "analyzingVideo" | "writing";

export type Progress =
  | { type: "stage"; stage: Stage; label: string }
  | { type: "step"; stage: Stage; done: number; total: number; unit: "bytes" | "frames" }
  | { type: "rejected"; path: string; reason: string }
  | { type: "note"; message: string };

/** Errors from Rust arrive as this object (see `AppError` in engine.rs). */
export interface AppError {
  kind: string;
  title: string;
  message: string;
  nextStep: string;
  reasons: string[];
  details: string;
}

/** Turns anything thrown by `invoke` into an AppError. */
export function asAppError(e: unknown): AppError {
  if (e && typeof e === "object" && "kind" in e && "title" in e) return e as AppError;
  const text = e instanceof Error ? e.message : String(e);
  return {
    kind: "internal",
    title: "Something went wrong inside DATfix",
    message: "An unexpected error stopped the job.",
    nextStep: "Please use Copy details and report this as a bug.",
    reasons: [],
    details: text,
  };
}

const channel = (onProgress?: (p: Progress) => void) =>
  new Channel<Progress>(onProgress ?? (() => {}));

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  sortPaths: (paths: string[]) => invoke<{ dats: string[]; clips: string[] }>("sort_paths", { paths }),
  inspectDat: (job: string, dat: string, onProgress?: (p: Progress) => void) =>
    invoke<DatSummary>("inspect_dat", { job, dat, progress: channel(onProgress) }),
  checkReference: (job: string, dat: string, clip: string) =>
    invoke<ReferenceMatch>("check_reference", { job, dat, clip }),
  findReference: (job: string, dat: string, dir: string | null, onProgress?: (p: Progress) => void) =>
    invoke<ReferenceMatch>("find_reference", { job, dat, dir, progress: channel(onProgress) }),
  suggestOutput: (dat: string, dir: string | null, taken: string[]) =>
    invoke<string>("suggest_output", { dat, dir, taken }),
  repairDat: (job: string, dat: string, reference: string, output: string, onProgress: (p: Progress) => void) =>
    invoke<RepairOutcome>("repair_dat", { job, dat, reference, output, progress: channel(onProgress) }),
  cancelJob: (job: string) => invoke<void>("cancel_job", { job }),
  devAutorun: () => invoke<DevAutorun | null>("dev_autorun"),
  devReport: (line: string) => invoke<void>("dev_report", { line }),
};

/** Test hook settings (see `dev_autorun` in commands.rs). */
export interface DevAutorun {
  dats: string[];
  /** A clip, or a folder to search. */
  reference?: string;
  outDir?: string;
  /** Press Repair automatically (default true). */
  repair?: boolean;
  /** Press Cancel this many milliseconds after Repair. */
  cancelAfterMs?: number;
}
