import { createEditor, type Kind, type OfficeSession, type SessionState } from "@office/sdk";
import { privateSource, type FileSource, type Node } from "@/api";
import { getDraft, onDraftRemoved, setDraft } from "@/lib/drafts";
import { openTabFiles, subscribeTabs } from "@/tabs";
import { OfficeStorage } from "./storage";

export interface OfficeEntry {
  id: string;
  element: HTMLElement;
  storage: OfficeStorage;
  session?: OfficeSession;
  state?: Readonly<SessionState>;
  error?: unknown;
  ready: Promise<void>;
  mounted: boolean;
  disposed: boolean;
  controller: AbortController;
  listeners: Set<() => void>;
  savedListeners: Set<(node: Node) => void>;
  unsubscribe?: () => void;
}

const sessions = new Map<string, OfficeEntry>();
const emit = (entry: OfficeEntry) => entry.listeners.forEach((listener) => listener());

export function releaseOffice(entry: OfficeEntry) {
  if (entry.disposed) return;
  const owned = sessions.get(entry.id) === entry;
  entry.disposed = true;
  if (owned) sessions.delete(entry.id);
  entry.controller.abort();
  entry.unsubscribe?.();
  entry.session?.destroy();
  entry.element.remove();
  entry.listeners.clear();
  entry.savedListeners.clear();
  if (owned && getDraft(entry.id, "office")) setDraft(entry.id, null);
}

function prune() {
  const open = openTabFiles();
  for (const entry of sessions.values()) if (!entry.mounted && !entry.state?.dirty && !entry.state?.saving && !open.has(entry.id)) releaseOffice(entry);
}
subscribeTabs(prune);
onDraftRemoved((id) => {
  const entry = sessions.get(id);
  if (entry?.state?.dirty && !entry.state.saving) releaseOffice(entry);
});
window.addEventListener("pagehide", (event) => {
  if (event.persisted) return;
  for (const entry of sessions.values()) releaseOffice(entry);
});

export function mountOffice(host: HTMLElement, node: Node, source: FileSource, editable: boolean, allowDownload: boolean): OfficeEntry {
  const reusable = editable && source === privateSource;
  let entry = reusable ? sessions.get(node.id) : undefined;
  if (!entry || entry.disposed) {
    const element = document.createElement("div");
    element.dataset.officeSession = node.id;
    element.style.height = "100%";
    element.style.minWidth = "0";
    const controller = new AbortController();
    const created: OfficeEntry = {
      id: node.id,
      element,
      storage: new OfficeStorage(node, source, editable, allowDownload),
      ready: Promise.resolve(),
      mounted: true,
      disposed: false,
      controller,
      listeners: new Set(),
      savedListeners: new Set(),
    };
    // Host integrations and browser acceptance can use the SDK's public session API through this owned mount.
    Object.defineProperty(element, "officeSession", { configurable: true, get: () => created.session });
    entry = created;
    if (reusable) sessions.set(node.id, entry);
    host.append(element);
    created.storage.onSaved = (saved) => {
      for (const listener of created.savedListeners) listener(saved);
    };
    created.ready = (async () => {
      try {
        const kind: Kind = node.name.toLowerCase().endsWith(".docx") ? "word" : node.name.toLowerCase().endsWith(".xlsx") ? "sheets" : "slides";
        const module = kind === "word" ? (await import("@office/word")).word() : kind === "sheets" ? (await import("@office/sheets")).sheets() : (await import("@office/slides")).slides();
        const session = await createEditor({ container: element, module, storage: created.storage, mode: editable ? "edit" : "readonly", signal: controller.signal });
        if (created.disposed) {
          session.destroy();
          return;
        }
        created.session = session;
        await session.open({ id: node.id });
        created.unsubscribe = session.subscribe((state) => {
          created.state = state;
          if (reusable) setDraft(node.id, state.dirty || state.saving ? { kind: "office", base: state.revision ?? "local" } : null);
          emit(created);
          if (!created.mounted) prune();
        });
      } catch (error) {
        if (!created.disposed) {
          created.session?.destroy();
          created.session = undefined;
          created.error = error;
          emit(created);
        }
      }
    })();
  } else {
    host.append(entry.element);
    entry.mounted = true;
  }
  return entry;
}

export function unmountOffice(entry: OfficeEntry) {
  entry.mounted = false;
  entry.element.remove();
  queueMicrotask(() => {
    if (entry.mounted || entry.disposed) return;
    if (sessions.get(entry.id) !== entry) releaseOffice(entry);
    else prune();
  });
}
