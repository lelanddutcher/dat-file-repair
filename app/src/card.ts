// "Recover from a memory card": the screens that scan a card image (or the card) for recordings
// the file system lost, then hand the copies to the normal repair flow in main.ts.
//
// Screens, in order:
//   choose   -> how to read the card (an image is the safe, recommended way)
//   imaging  -> making an image of the card (optional)
//   scanning -> progress and a live list of what was found
//   results  -> one card per recording; "Repair all"
//   copying  -> copying the recordings off the card, then main.ts repairs them
//
// The card or image is only ever read. Copies go to a folder the person chooses.

import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { api, asAppError, cardApi, type AppError, type CardDevice, type CardProgress, type CardScanView, type ClipView, type PreparedCard, type RecordingView } from "./api";
import { h, type Child } from "./dom";
import * as fmt from "./format";

/** What main.ts lends the card screens. */
export interface Host {
  render(): void;
  scheduleRender(): void;
  announce(text: string): void;
  toast(text: string): void;
  errorBox(err: AppError): HTMLElement;
  detailsBox(text: string): HTMLElement;
  progressBar(fraction: number, label: string): HTMLElement;
  spinner(): HTMLElement;
  os(): string;
  /** Leaves the card screens. */
  close(): void;
  /** Repairs the copied recordings with the normal flow. */
  repair(prepared: PreparedCard, outDir: string, scan: CardScanView): Promise<void>;
  /** Test hook: announce a screen and wait so a script can take a screenshot. */
  devScreen(name: string): Promise<void>;
}

type Screen = "choose" | "imaging" | "scanning" | "results" | "copying";

interface Job {
  id: string;
  label: string;
  done: number;
  total: number;
  start: number;
  /** Bytes per second, smoothed. */
  rate?: number;
  stopRequested?: boolean;
}

export const card = {
  screen: "choose" as Screen,
  /** The .DAT that brought the person here (a 0-byte file), if any. */
  lookingFor: null as string | null,
  /** A path on the card, to preselect it in the list. */
  hintPath: null as string | null,
  /** "make" = image the card first, "direct" = scan the device itself. */
  mode: null as "make" | "direct" | null,
  cards: null as CardDevice[] | null,
  device: null as string | null,
  imageDir: null as string | null,
  job: null as Job | null,
  found: [] as { what: string; offset: number; length: number }[],
  scan: null as CardScanView | null,
  error: null as AppError | null,
  /** Recordings ticked for "Repair all", by offset. */
  chosen: new Set<number>(),
  outDir: null as string | null,
  /** Per-item save results for clips and fragments, by offset. */
  saved: new Map<number, string>(),
  saving: null as number | null,
};

let host: Host;
let nextId = 1;
const newJob = (prefix: string, label: string): Job => ({ id: `${prefix}-${nextId++}-${Date.now()}`, label, done: 0, total: 0, start: performance.now() });

export function initCard(h: Host) {
  host = h;
}

/** Opens the card screens. */
export function openCard(opts: { lookingFor?: string; hintPath?: string; image?: string } = {}) {
  Object.assign(card, {
    screen: "choose",
    lookingFor: opts.lookingFor ?? null,
    hintPath: opts.hintPath ?? null,
    mode: null,
    device: null,
    job: null,
    found: [],
    scan: null,
    error: null,
    saved: new Map(),
  });
  card.chosen = new Set();
  host.render();
  window.scrollTo(0, 0);
  void loadCards();
  if (opts.image) void scanImage(opts.image);
}

// ---------------------------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------------------------

async function loadCards() {
  card.cards = null;
  host.render();
  try {
    card.cards = await cardApi.listCards();
  } catch {
    card.cards = [];
  }
  const hint = card.hintPath;
  const match = hint ? card.cards.find((c) => hint.startsWith(c.mountPoint + "/")) : undefined;
  if (!card.device || !card.cards.some((c) => c.device === card.device)) {
    card.device = match?.device ?? (card.cards.length === 1 ? card.cards[0].device : null);
  }
  host.render();
}

async function chooseImage() {
  const picked = await open({
    title: "Choose a card image",
    filters: [{ name: "Card image", extensions: ["img", "dmg", "dd", "cdr", "iso", "bin", "raw", "IMG", "DMG", "DD"] }],
  });
  if (typeof picked === "string") await scanImage(picked);
}

async function chooseImageDir() {
  const picked = await open({ directory: true, title: "Choose where to save the card image" });
  if (typeof picked === "string") {
    card.imageDir = picked;
    host.render();
  }
}

export async function makeImage() {
  if (!card.device || !card.imageDir) return;
  const job = newJob("image", "Copying the whole card into an image file");
  card.job = job;
  card.error = null;
  card.screen = "imaging";
  host.render();
  try {
    const image = await cardApi.imageCard(job.id, card.device, card.imageDir, (p) => onProgress(job, p));
    host.announce("The card image is ready. Searching it now.");
    await scanImage(image);
  } catch (e) {
    card.error = asAppError(e);
    card.screen = "choose";
    card.job = null;
    host.render();
  }
}

export async function scanDirect() {
  const c = card.cards?.find((x) => x.device === card.device);
  if (c) await scanImage(c.rawDevice);
}

export async function scanImage(image: string) {
  const job = newJob("scan", "Starting");
  card.job = job;
  card.found = [];
  card.error = null;
  card.scan = null;
  card.screen = "scanning";
  host.render();
  try {
    const scan = await cardApi.scanCard(job.id, image, (p) => onProgress(job, p));
    card.scan = scan;
    // Every recording is ticked; a dropped 0-byte file's match is listed first.
    card.chosen = new Set(scan.recordings.map((r) => r.offset));
    const want = card.lookingFor?.toLowerCase();
    if (want) scan.recordings.sort((a, b) => Number(b.fileName?.toLowerCase() === want) - Number(a.fileName?.toLowerCase() === want));
    card.screen = "results";
    card.job = null;
    host.announce(foundSentence(scan));
  } catch (e) {
    card.error = asAppError(e);
    card.screen = "choose";
    card.job = null;
  }
  host.render();
  window.scrollTo(0, 0);
}

function onProgress(job: Job, p: CardProgress) {
  const now = performance.now();
  if (p.type === "stage") {
    job.label = p.label;
    job.done = 0;
    job.total = 0;
    job.start = now;
    job.rate = undefined;
  } else if (p.type === "step") {
    job.done = p.done;
    job.total = p.total;
    const elapsed = (now - job.start) / 1000;
    if (elapsed > 1 && p.done > 0) {
      const instant = p.done / elapsed;
      job.rate = job.rate ? job.rate * 0.7 + instant * 0.3 : instant;
    }
  } else {
    card.found.push({ what: p.what, offset: p.offset, length: p.length });
  }
  host.scheduleRender();
}

export async function cancelJob() {
  const job = card.job;
  if (!job) return;
  job.stopRequested = true;
  host.render();
  await api.cancelJob(job.id);
}

async function chooseOutDir(): Promise<boolean> {
  const picked = await open({ directory: true, title: "Choose where to save the recovered videos" });
  if (typeof picked !== "string") return false;
  card.outDir = picked;
  host.render();
  return true;
}

export async function repairAll() {
  const scan = card.scan;
  if (!scan || card.chosen.size === 0) return;
  if (!card.outDir && !(await chooseOutDir())) return;
  const outDir = card.outDir!;
  const job = newJob("copy", "Copying from the card");
  card.job = job;
  card.error = null;
  card.screen = "copying";
  host.render();
  window.scrollTo(0, 0);
  try {
    const offsets = scan.recordings.filter((r) => card.chosen.has(r.offset)).map((r) => r.offset);
    const prepared = await cardApi.prepareRepair(job.id, offsets, outDir, (p) => onProgress(job, p));
    card.job = null;
    await host.repair(prepared, outDir, scan);
  } catch (e) {
    card.error = asAppError(e);
    card.screen = "results";
    card.job = null;
    host.render();
  }
}

async function saveOne(kind: "clip" | "fragment", offset: number) {
  if (!card.outDir && !(await chooseOutDir())) return;
  card.saving = offset;
  host.render();
  try {
    const path = await cardApi.save(`save-${offset}-${Date.now()}`, kind, offset, card.outDir!, () => {});
    card.saved.set(offset, path);
    host.announce(`Saved ${fmt.baseName(path)}.`);
  } catch (e) {
    host.toast(asAppError(e).message);
  }
  card.saving = null;
  host.render();
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

export function renderCard(): HTMLElement[] {
  const back = h("div", { class: "row back-row" }, [
    h(
      "button",
      { id: "card-back", class: "link", type: "button", disabled: !!card.job, onclick: () => host.close() },
      "← Back to repairing .DAT files",
    ),
  ]);
  const body =
    card.screen === "choose"
      ? renderChoose()
      : card.screen === "results" && card.scan
        ? renderResults(card.scan)
        : renderWorking();
  return [back, ...body];
}

function renderChoose(): HTMLElement[] {
  const mac = host.os() === "macos";
  const option = (id: string, title: string, text: string, opts: { recommended?: boolean; selected?: boolean; onclick: () => void }) =>
    h("button", { id, class: `option${opts.selected ? " selected" : ""}`, type: "button", "aria-pressed": opts.selected ? "true" : undefined, onclick: opts.onclick }, [
      h("span", { class: "option-title" }, [title, opts.recommended ? h("span", { class: "badge good" }, "Recommended") : null]),
      h("span", { class: "option-text" }, text),
    ]);
  const main = h("section", { class: "card", "aria-labelledby": "card-h" }, [
    h("h2", { id: "card-h" }, "Recover from a memory card"),
    card.lookingFor
      ? h("p", { class: "looking" }, [
          "Looking for ",
          h("span", { class: "file-name" }, card.lookingFor),
          ". The camera created this file but never saved the video into it. The video itself may still be on the card.",
        ])
      : null,
    h("p", { class: "explain" }, [
      h("strong", {}, "Stop using the card now: "),
      "recording, formatting or letting the computer repair it can overwrite the video you want back. Scanning a copy of the card (a disk image) is safest, because the card is then read only once and never changed.",
    ]),
    card.error?.kind === "cancelled"
      ? h("p", { class: "notice small", role: "status" }, "The search was stopped. Nothing was changed on the card.")
      : card.error
        ? host.errorBox(card.error)
        : null,
    h("div", { class: "options" }, [
      option("card-image", "Choose a card image", "A .img, .dmg or .dd file made from the card.", { recommended: true, onclick: () => void chooseImage() }),
      option("card-make", "Make an image of the card", "DATfix copies the whole card into one file on your computer, then searches the copy. Nothing is written to the card.", {
        selected: card.mode === "make",
        onclick: () => {
          card.mode = card.mode === "make" ? null : "make";
          host.render();
        },
      }),
      card.mode === "make" ? renderMake(mac) : null,
      option("card-direct", "Scan the card directly (advanced)", "Faster, but the card is read again each time you scan. If the card is failing, a second read can lose more.", {
        selected: card.mode === "direct",
        onclick: () => {
          card.mode = card.mode === "direct" ? null : "direct";
          host.render();
        },
      }),
      card.mode === "direct" ? renderDirect(mac) : null,
    ]),
    h("details", { class: "small" }, [
      h("summary", {}, "Other ways to make a card image"),
      h("ol", { class: "steps" }, [
        h("li", {}, "Mac, Disk Utility: choose View > Show All Devices, select the card itself (the top line, not the volume under it), then File > New Image > Image from “…”. Choose the format “DVD/CD master” or “read/write”. Compressed images do not work."),
        h("li", {}, "Mac, Terminal: diskutil list (find the card, for example disk4), then sudo dd if=/dev/rdisk4 of=card.img bs=4m"),
        h("li", {}, "Windows: a raw imaging tool such as HDD Raw Copy or the dd in Git Bash. Linux: sudo dd if=/dev/sdX of=card.img bs=4M conv=noerror,sync, or ddrescue for a failing card."),
      ]),
    ]),
  ]);
  return [main];
}

function renderCardList(mac: boolean): Child {
  if (!mac) return h("p", { class: "muted small" }, "DATfix can list cards on a Mac only. Make an image with another tool (see “Other ways” below), then choose it above.");
  if (card.cards === null) return h("p", { class: "muted small" }, [host.spinner(), "Looking for memory cards…"]);
  if (card.cards.length === 0)
    return h("div", {}, [
      h("p", { class: "small" }, "No memory card was found. Insert the card into the card reader, then look again."),
      h("button", { id: "card-reload", type: "button", onclick: () => void loadCards() }, "Look again"),
    ]);
  return h("div", { class: "card-list", role: "radiogroup", "aria-label": "Memory cards" }, [
    ...card.cards.map((c) =>
      h("label", { class: `card-choice${card.device === c.device ? " selected" : ""}` }, [
        h("input", {
          type: "radio",
          name: "card-device",
          id: `dev-${c.device}`,
          checked: card.device === c.device,
          onchange: () => {
            card.device = c.device;
            host.render();
          },
        }),
        h("span", {}, [
          h("span", { class: "file-name" }, c.name),
          h("span", { class: "muted small" }, ` · ${fmt.bytes(c.sizeBytes)}${c.fileSystem ? ` · ${c.fileSystem}` : ""}${c.looksLikeCameraCard ? " · camera card" : ""}`),
        ]),
      ]),
    ),
    h("button", { id: "card-reload", class: "link small", type: "button", onclick: () => void loadCards() }, "Look again"),
  ]);
}

function renderMake(mac: boolean): HTMLElement {
  const dev = card.cards?.find((c) => c.device === card.device);
  return h("div", { class: "option-panel" }, [
    renderCardList(mac),
    mac
      ? h("div", {}, [
          h("p", { class: "small" }, dev ? `The image needs ${fmt.bytes(dev.sizeBytes)} of free space.` : "Choose the card above."),
          h("div", { class: "row" }, [
            h("button", { id: "image-dir", type: "button", onclick: () => void chooseImageDir() }, card.imageDir ? "Change folder…" : "Choose where to save the image…"),
            card.imageDir ? h("span", { class: "path", title: card.imageDir }, card.imageDir) : null,
          ]),
          dev && !dev.readable ? h("p", { class: "muted small" }, "Your Mac will ask for an administrator password, because only administrators can read a whole card.") : null,
          h("button", { id: "image-go", class: "primary", type: "button", disabled: !dev || !card.imageDir, onclick: () => void makeImage() }, "Make the image and search it"),
        ])
      : null,
  ]);
}

function renderDirect(mac: boolean): HTMLElement {
  const dev = card.cards?.find((c) => c.device === card.device);
  return h("div", { class: "option-panel" }, [
    renderCardList(mac),
    mac && dev && !dev.readable
      ? h("p", { class: "muted small" }, "Your Mac only lets administrators read a whole card. If the scan is refused, choose “Make an image of the card” instead.")
      : null,
    mac ? h("button", { id: "direct-go", class: "primary", type: "button", disabled: !dev, onclick: () => void scanDirect() }, "Search the card") : null,
  ]);
}

/** Imaging, scanning and copying: progress, live findings, Cancel. */
function renderWorking(): HTMLElement[] {
  const job = card.job;
  const title = card.screen === "imaging" ? "Making an image of the card" : card.screen === "copying" ? "Copying the recordings" : "Searching the card";
  const lede =
    card.screen === "imaging"
      ? "The card is only read. Keep it in the reader until this finishes."
      : card.screen === "copying"
        ? "Each recording is copied off the card into the folder you chose. DATfix then repairs the copies."
        : "DATfix reads the whole card once, looking for Canon recordings by their content. Nothing is written to the card.";
  const frac = job?.total ? job.done / job.total : 0;
  const amount = job?.total ? `${fmt.bytes(job.done)} of ${fmt.bytes(job.total)}` : "";
  const speed = job?.rate ? `${fmt.bytes(job.rate)}/s` : "";
  const left = job?.rate && job.total ? fmt.eta((job.total - job.done) / job.rate) : "";
  const found = card.found;
  const recs = found.filter((f) => f.what === "recording").length;
  const clips = found.filter((f) => f.what === "clip").length;
  const frags = found.filter((f) => f.what === "fragment").length;
  return [
    h("section", { class: "card", "aria-labelledby": "work-h" }, [
      h("h2", { id: "work-h" }, title),
      h("p", { class: "muted" }, lede),
      h("div", { class: "run" }, [
        h("p", { class: "stage" }, job?.label ?? "Starting"),
        host.progressBar(frac, job?.label ?? title),
        h("p", { class: "muted small" }, [amount, speed, left].filter(Boolean).join(" · ") || " "),
      ]),
      card.screen === "scanning"
        ? h("div", { class: "found", "aria-live": "polite" }, [
            h("p", { class: "small" }, found.length ? `Found so far: ${plural(recs, "recording")}, ${plural(clips, "finished clip")}${frags ? `, ${plural(frags, "piece")} without a start` : ""}` : "Nothing found yet."),
            found.length
              ? h(
                  "ul",
                  { class: "found-list small" },
                  found.slice(-8).map((f) =>
                    h("li", {}, `${f.what === "recording" ? "Recording" : f.what === "clip" ? "Finished clip" : "Piece without a start"} · ${fmt.bytes(f.length)} · at ${fmt.bytes(f.offset)} on the card`),
                  ),
                )
              : null,
          ])
        : null,
    ]),
    h("section", { class: "card action" }, [
      h(
        "button",
        { id: "card-cancel", class: "danger big-btn", type: "button", disabled: !job || job.stopRequested, onclick: () => void cancelJob() },
        job?.stopRequested ? "Stopping…" : "Cancel",
      ),
    ]),
  ];
}

function plural(n: number, word: string) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function foundSentence(s: CardScanView): string {
  const n = s.recordings.length;
  if (n === 0) return "No lost recordings were found on this card.";
  return n === 1 ? "Found 1 recording that can be recovered." : `Found ${n} recordings that can be recovered.`;
}

function renderResults(s: CardScanView): HTMLElement[] {
  const n = s.recordings.length;
  const chosen = s.recordings.filter((r) => card.chosen.has(r.offset)).length;
  const head = h("section", { class: "card", "aria-labelledby": "res-h" }, [
    h("h2", { id: "res-h" }, foundSentence(s)),
    h("p", { class: "muted small" }, [
      `Searched ${s.imageName} (${fmt.bytes(s.sizeBytes)}) in ${fmt.duration(s.seconds)}.`,
      s.fileSystem ? ` Card format: ${s.fileSystem}.` : "",
    ]),
    n === 0
      ? h(
          "p",
          {},
          "The recording may have been overwritten, or the camera may not leave a recovery index (EOS R and M50 Mark II do not). If you have another copy of the card, try that.",
        )
      : null,
    ...s.notes.map((t) => h("p", { class: "notice small" }, t)),
    card.error ? host.errorBox(card.error) : null,
    n > 0 ? h("ul", { class: "items", "aria-label": "Recordings found" }, s.recordings.map((r) => renderRecording(r))) : null,
  ]);
  const extras = h("section", { class: "card", "aria-label": "Other things found" }, [
    renderClips(s.clips),
    renderFragments(s.fragments),
    host.detailsBox(s.details),
    h("div", { class: "row" }, [
      h("button", { id: "card-again", type: "button", onclick: () => openCard({ lookingFor: card.lookingFor ?? undefined, hintPath: card.hintPath ?? undefined }) }, "Search another card"),
    ]),
  ]);
  const action =
    n > 0
      ? h("section", { class: "card action", "aria-label": "Repair" }, [
          h("div", { class: "row out-row" }, [
            h("span", { class: "small" }, card.outDir ? "Save in: " : "Choose where to save the recovered videos."),
            card.outDir ? h("span", { class: "path", title: card.outDir }, card.outDir) : null,
            h("button", { id: "card-out", class: "link", type: "button", onclick: () => void chooseOutDir() }, card.outDir ? "Change…" : "Choose a folder…"),
          ]),
          h(
            "button",
            { id: "card-repair", class: "primary big-btn", type: "button", disabled: chosen === 0, onclick: () => void repairAll() },
            chosen === n ? (n === 1 ? "Repair" : `Repair all ${n}`) : `Repair ${chosen} of ${n}`,
          ),
          h("p", { class: "muted small" }, "Each recording is copied into that folder and then repaired. The card and the image are only read."),
        ])
      : null;
  return [head, extras, action].filter((x): x is HTMLElement => x !== null);
}

function renderRecording(r: RecordingView): HTMLElement {
  const isTarget = !!card.lookingFor && r.fileName?.toLowerCase() === card.lookingFor.toLowerCase();
  const name = r.fileName ?? "A recording with no file name";
  const length = r.durationSeconds != null ? `About ${fmt.duration(r.durationSeconds)} of video` : `${r.videoFrames.toLocaleString()} video frames`;
  const badges: Child[] = [
    isTarget ? h("span", { class: "badge good" }, "The file you dropped") : null,
    r.badge === "deleted" ? h("span", { class: "badge" }, "Deleted") : null,
    r.badge === "empty" ? h("span", { class: "badge" }, "0-byte file on the card") : null,
    !r.cardPath ? h("span", { class: "badge" }, "Not in the card's file list") : null,
  ];
  const warnings: Child[] = [
    r.mayBeIncomplete ? h("li", {}, [h("strong", {}, "May be incomplete. "), sentence(r.mayBeIncomplete)]) : null,
    r.overlapsClip ? h("li", {}, [h("strong", {}, "Overlaps a finished clip. "), "A later recording was saved over part of it, so that part may show damage."]) : null,
  ];
  const id = `rec-${r.offset}`;
  return h("li", { class: `item${isTarget ? " target" : ""}` }, [
    h("div", { class: "item-head" }, [
      h("label", { class: "pick" }, [
        h("input", {
          type: "checkbox",
          id,
          checked: card.chosen.has(r.offset),
          onchange: (ev: Event) => {
            if ((ev.target as HTMLInputElement).checked) card.chosen.add(r.offset);
            else card.chosen.delete(r.offset);
            host.render();
          },
        }),
        h("span", { class: "file-name", title: r.cardPath ?? "" }, name),
      ]),
    ]),
    h("div", { class: "badges" }, badges),
    h("p", { class: "big" }, length),
    h("dl", { class: "facts" }, [
      fact("Camera", r.camera ? (r.frameRate ? `${r.camera}, ${r.frameRate} frames per second` : r.camera) : "Canon (model unknown)"),
      fact("Size", fmt.bytes(r.sizeBytes)),
      fact("On the card", r.pieces > 1 ? `Joined from ${r.pieces} pieces` : "In one piece"),
      fact("Settings", r.references > 0 ? `From ${plural(r.references, "finished clip")} on the same card, else built-in` : "DATfix's built-in settings"),
    ]),
    warnings.some(Boolean) ? h("ul", { class: "warn-list small" }, warnings) : null,
    host.detailsBox(r.details),
  ]);
}

function fact(label: string, value: string): Child[] {
  return [h("dt", {}, label), h("dd", {}, value)];
}

/** Engine notes start lower-case; make them a sentence. */
function sentence(t: string) {
  const s = t.charAt(0).toUpperCase() + t.slice(1);
  return /[.!?]$/.test(s) ? s : `${s}.`;
}

function renderClips(clips: ClipView[]): HTMLElement {
  const deleted = clips.filter((c) => c.deleted).length;
  return h("details", { class: "group", id: "clips" }, [
    h("summary", {}, `Finished clips on the card (${clips.length}${deleted ? `, ${deleted} deleted` : ""})`),
    clips.length === 0
      ? h("p", { class: "muted small" }, "None found.")
      : h("div", {}, [
          h("p", { class: "muted small" }, "Clips the camera finished normally. They need no repair; DATfix uses them as references. You can save a copy, for example of a deleted clip."),
          h(
            "ul",
            { class: "plain-list" },
            clips.map((c) =>
              h("li", {}, [
                h("span", {}, [
                  h("span", { class: "file-name" }, c.fileName ?? `Clip at ${fmt.bytes(c.offset)}`),
                  c.deleted ? h("span", { class: "badge" }, "Deleted") : null,
                  !c.complete ? h("span", { class: "badge" }, "Incomplete") : null,
                  h("span", { class: "muted small" }, ` ${[c.camera, c.durationSeconds != null ? fmt.duration(c.durationSeconds) : null, fmt.bytes(c.sizeBytes)].filter(Boolean).join(" · ")}`),
                ]),
                saveButton("clip", c.offset),
              ]),
            ),
          ),
        ]),
  ]);
}

function renderFragments(frags: RecordingView[]): HTMLElement {
  return h("details", { class: "group", id: "fragments" }, [
    h("summary", {}, `Pieces without a start (${frags.length})`),
    frags.length === 0
      ? h("p", { class: "muted small" }, "None found.")
      : h("div", {}, [
          h("p", { class: "muted small" }, "Video data that no recording start leads to: the end of a recording whose start was overwritten, or a piece DATfix could not join safely. Save one and drop it on DATfix to try a repair."),
          h(
            "ul",
            { class: "plain-list" },
            frags.map((f) =>
              h("li", {}, [
                h("span", {}, [
                  h("span", { class: "file-name" }, `Piece at ${fmt.bytes(f.offset)}`),
                  h("span", { class: "muted small" }, ` ${[f.camera, f.durationSeconds != null ? `about ${fmt.duration(f.durationSeconds)}` : null, fmt.bytes(f.sizeBytes)].filter(Boolean).join(" · ")}`),
                ]),
                saveButton("fragment", f.offset),
              ]),
            ),
          ),
        ]),
  ]);
}

function saveButton(kind: "clip" | "fragment", offset: number): Child {
  const saved = card.saved.get(offset);
  if (saved)
    return h("button", { id: `saved-${offset}`, class: "link small", type: "button", onclick: () => void revealItemInDir(saved) }, host.os() === "macos" ? "Saved · Show in Finder" : "Saved · Show");
  return h(
    "button",
    { id: `save-${offset}`, class: "small", type: "button", disabled: card.saving !== null, onclick: () => void saveOne(kind, offset) },
    card.saving === offset ? "Saving…" : "Save a copy",
  );
}
