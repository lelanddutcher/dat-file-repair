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
  /** Forced theme for screenshots (DATFIX_DEV_THEME), else null. */
  theme: "light" | "dark" | null;
}

export interface DatSummary {
  path: string;
  fileName: string;
  sizeBytes: number;
  camera: string;
  /** True when `camera` is the exact model read from the file. */
  cameraKnown: boolean;
  contents: string;
  estimatedSeconds: number | null;
  frameRate: string | null;
  videoFrames: number;
  cutOff: boolean;
}

export type SettingsSource = "clip" | "clipVideoOnly" | "builtin";

export interface CandidateView {
  path: string;
  fileName: string;
  verdict: "fits" | "videoOnly" | "skipped";
  reason: string | null;
  detail: string | null;
}

export interface ReferenceMatch {
  ok: boolean;
  source: SettingsSource;
  path: string | null;
  /** One plain sentence, e.g. "Using built-in settings for Canon EOS R5 Mark II, 4K 119.88p." */
  summary: string;
  /** Why no clip was used, when the built-in settings were picked automatically. */
  searchNote: string | null;
  rebuilt: string[];
  reasons: string[];
  details: string;
  checked: number;
  candidates: CandidateView[];
}

export interface PartialView {
  path: string;
  bytes: number;
}

export interface RepairOutcome {
  output: string;
  settings: string;
  rebuilt: string[];
  camera: string | null;
  durationSeconds: number;
  videoFrames: number;
  audioTracks: number;
  bytesWritten: number;
  notes: string[];
  /** Recording start written into the file, UTC seconds since 1970. */
  creationTime: number | null;
  /** Where that time came from, in plain words. */
  dateSource: string;
  /** Display rotation written into the file, degrees clockwise, or null when upright. */
  rotationDegrees: number | null;
  /** Reference-clip metadata left out of the file. */
  withheldMetadata: string[];
}

/** The "Advanced" options of one file. */
export interface RepairChoices {
  /** 0, 90, 180 or 270 degrees clockwise; null keeps the reference clip's orientation. */
  rotation: number | null;
  keepReferenceMetadata: boolean;
}

/** What a memory card said about a recording copied off it (engine `RecordingHint`). Opaque to the page. */
export type RecordingHint = Record<string, unknown>;

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
  /** A web page that helps (for example the issue form), or null. */
  link?: string | null;
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

// ---------------------------------------------------------------------------------------------
// Recover from a memory card (src-tauri/src/engine/card.rs)
// ---------------------------------------------------------------------------------------------

export type CardProgress =
  | { type: "stage"; label: string }
  | { type: "step"; done: number; total: number }
  | { type: "found"; what: "recording" | "clip" | "fragment"; offset: number; length: number };

export interface RecordingView {
  /** Byte position on the card; identifies the recording. */
  offset: number;
  sizeBytes: number;
  camera: string | null;
  durationSeconds: number | null;
  videoFrames: number;
  frameRate: string | null;
  fileName: string | null;
  cardPath: string | null;
  badge: "deleted" | "empty" | null;
  pieces: number;
  mayBeIncomplete: string | null;
  overlapsClip: boolean;
  references: number;
  details: string;
}

export interface ClipView {
  offset: number;
  sizeBytes: number;
  camera: string | null;
  durationSeconds: number | null;
  fileName: string | null;
  deleted: boolean;
  complete: boolean;
}

export interface CardScanView {
  image: string;
  imageName: string;
  sizeBytes: number;
  isDevice: boolean;
  seconds: number;
  fileSystem: string | null;
  recordings: RecordingView[];
  fragments: RecordingView[];
  clips: ClipView[];
  notes: string[];
  details: string;
}

export interface PreparedCard {
  recordings: { offset: number; dat: string | null; hint: RecordingHint; error: AppError | null }[];
  clipsDir: string | null;
  clips: number;
}

export interface CardDevice {
  name: string;
  mountPoint: string;
  device: string;
  rawDevice: string;
  sizeBytes: number;
  fileSystem: string;
  looksLikeCameraCard: boolean;
  readable: boolean;
}

const cardChannel = (onProgress: (p: CardProgress) => void) => new Channel<CardProgress>(onProgress);

export const cardApi = {
  listCards: () => invoke<CardDevice[]>("list_cards"),
  imageCard: (job: string, device: string, outDir: string, onProgress: (p: CardProgress) => void) =>
    invoke<string>("image_card", { job, device, outDir, progress: cardChannel(onProgress) }),
  scanCard: (job: string, image: string, onProgress: (p: CardProgress) => void) =>
    invoke<CardScanView>("scan_card", { job, image, progress: cardChannel(onProgress) }),
  prepareRepair: (job: string, offsets: number[], outDir: string, onProgress: (p: CardProgress) => void) =>
    invoke<PreparedCard>("card_prepare_repair", { job, offsets, outDir, progress: cardChannel(onProgress) }),
  save: (job: string, kind: "clip" | "fragment", offset: number, outDir: string, onProgress: (p: CardProgress) => void) =>
    invoke<string>("card_save", { job, kind, offset, outDir, progress: cardChannel(onProgress) }),
};

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
  checkBuiltin: (job: string, dat: string) => invoke<ReferenceMatch>("check_builtin", { job, dat }),
  suggestOutput: (dat: string, dir: string | null, taken: string[]) =>
    invoke<string>("suggest_output", { dat, dir, taken }),
  /** `reference` null = built-in settings. */
  repairDat: (
    job: string,
    dat: string,
    reference: string | null,
    output: string,
    choices: RepairChoices,
    hint: RecordingHint | null,
    onProgress: (p: Progress) => void,
  ) => invoke<RepairOutcome>("repair_dat", { job, dat, reference, output, choices, hint, progress: channel(onProgress) }),
  findPartials: (dirs: string[]) => invoke<PartialView[]>("find_partials", { dirs }),
  removePartials: (paths: string[]) => invoke<number>("remove_partials", { paths }),
  cancelJob: (job: string) => invoke<void>("cancel_job", { job }),
  devAutorun: () => invoke<DevAutorun | null>("dev_autorun"),
  devReport: (line: string) => invoke<void>("dev_report", { line }),
};

/** Test hook settings (see `dev_autorun` in commands.rs). */
export interface DevAutorun {
  dats: string[];
  /** A clip, a folder to search, or "builtin". */
  reference?: string;
  /** Press "Delete them" when unfinished files from a killed repair are found. */
  cleanPartials?: boolean;
  outDir?: string;
  /** Press Repair automatically (default true). */
  repair?: boolean;
  /** Press Cancel this many milliseconds after Repair. */
  cancelAfterMs?: number;
  /** Scroll the page down this many pixels once ready (for screenshots). */
  scrollY?: number;
  /** "Advanced" options for every file: rotation in degrees, keep the reference's metadata. */
  rotation?: number;
  keepReferenceMetadata?: boolean;
  /** Open the "Advanced" section of every file (for screenshots). */
  openAdvanced?: boolean;
  /** Wait this long on each screen, after announcing it, so a script can take a screenshot. */
  pauseMs?: number;
  /** Drive the "Recover from a memory card" flow instead (see card.ts). */
  card?: DevCardRun;
}

export interface DevCardRun {
  /** A card image to scan. */
  image?: string;
  /** Or: a disk ("disk6") to image into `imageDir` first, or to scan directly with `direct`. */
  device?: string;
  imageDir?: string;
  direct?: boolean;
  /** Drop this (0-byte) .DAT first and enter the flow from its error. */
  fromEmptyDat?: string;
  /** Folder for the copies and repaired videos. */
  outDir?: string;
  /** Press "Repair all" (default true). */
  repair?: boolean;
  /** Press Cancel this many milliseconds after the scan starts. */
  cancelAfterMs?: number;
  /** Wait this long on each screen, after announcing it, so a script can take a screenshot. */
  pauseMs?: number;
}
