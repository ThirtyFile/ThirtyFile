/**
 * A client for the part of tus 1.0 (https://tus.io/protocols/resumable-upload) that this server speaks: creation, `PATCH`
 * in chunks, resuming (`HEAD`), and termination (`DELETE`). Replaces tus-js-client, which did all of it for a handful of
 * options.
 *
 * - **Progress** comes from `XMLHttpRequest`, since `fetch` can't report how much of a request body was sent.
 * - **Resuming**: the address of the upload is kept in `localStorage` under `tus::<fingerprint>::<random>`, with the same
 *   JSON tus-js-client kept (`uploadUrl`, `creationTime`, …), so an upload that was being sent when the page was updated can
 *   be continued. The calling code (`uploadRecovery.ts`) reads those records.
 * - **Retries**: after an error that `onShouldRetry` accepts, the upload waits `retryDelays[n]` ms and starts again, which
 *   asks the server where it is (`HEAD`) first. The count starts over whenever more was sent than before the last retry.
 */

export interface TusResponse {
  status: number;
  body: string;
  header(name: string): string | null;
}

/** A request that failed: `response` is what the server answered, none when it couldn't be reached */
export class TusError extends Error {
  constructor(
    message: string,
    readonly response?: TusResponse,
  ) {
    super(message);
    this.name = "TusError";
  }
}

export interface TusOptions {
  /** Where uploads are created (`POST`) */
  endpoint: string;
  /** Bytes sent by one `PATCH` */
  chunkSize: number;
  /** Waits (ms) before each retry; no more retries once they are used up */
  retryDelays: number[];
  metadata: Record<string, string>;
  /** Names the stored address of this upload: `tus::<fingerprint>::…` */
  fingerprint: string;
  onProgress(sent: number, total: number): void;
  /** `lastResponse`: the answer to the request that completed the upload (the creation of an empty file, the last chunk) */
  onSuccess(done: { lastResponse: TusResponse }): void;
  onError(error: TusError): void;
  onShouldRetry(error: TusError): boolean;
}

const TUS_VERSION = "1.0.0";

const utf8Base64 = (text: string) => {
  let binary = "";
  for (const byte of new TextEncoder().encode(text)) binary += String.fromCharCode(byte);
  return btoa(binary);
};

/** `Upload-Metadata`: `key base64(value)`, comma separated */
export const encodeMetadata = (metadata: Record<string, string>) =>
  Object.entries(metadata)
    .filter(([, value]) => value !== undefined)
    .map(([key, value]) => `${key} ${utf8Base64(value)}`)
    .join(",");

function responseOf(xhr: XMLHttpRequest): TusResponse {
  return { status: xhr.status, body: xhr.responseText ?? "", header: (name) => xhr.getResponseHeader(name) };
}

interface Reply {
  response: TusResponse;
}

export class TusUpload {
  private url: string | null = null;
  private offset = 0;
  private offsetBeforeRetry = 0;
  private retryAttempt = 0;
  private stopped = false;
  private request: XMLHttpRequest | null = null;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private storageKey: string | null = null;

  constructor(
    private readonly file: Blob,
    private readonly options: TusOptions,
  ) {}

  /** Continues the upload at this address (from an earlier page load) instead of creating one */
  resume(url: string) {
    this.url = url;
  }

  start() {
    this.stopped = false;
    void (this.url ? this.ask() : this.create());
  }

  /** Stops sending. With `terminate` the server drops what it received and the upload can't be continued; otherwise it can */
  async abort(terminate = false) {
    this.stopped = true;
    clearTimeout(this.timer);
    this.request?.abort();
    this.request = null;
    if (!terminate) return;
    const url = this.url;
    this.forget();
    this.url = null;
    if (url) await this.send("DELETE", url).catch(() => undefined);
  }

  // ───────────── Requests ─────────────

  /** One request: resolves with the response of any status, rejects when the server can't be reached */
  private send(method: string, url: string, headers: Record<string, string> = {}, body?: Blob, onSent?: (loaded: number) => void): Promise<Reply> {
    return new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      this.request = xhr;
      xhr.open(method, url, true);
      xhr.setRequestHeader("Tus-Resumable", TUS_VERSION);
      for (const [name, value] of Object.entries(headers)) xhr.setRequestHeader(name, value);
      if (onSent) xhr.upload.onprogress = (e) => onSent(e.loaded);
      xhr.onload = () => resolve({ response: responseOf(xhr) });
      xhr.onerror = () => reject(new TusError("The server could not be reached"));
      xhr.onabort = () => reject(new TusError("The request was stopped"));
      xhr.ontimeout = () => reject(new TusError("The server did not answer in time"));
      xhr.send(body ?? null);
    });
  }

  private failedWith(response: TusResponse, what: string) {
    return new TusError(`${what} (${response.status})`, response);
  }

  /** A request that never got an answer: when it was stopped on purpose, nothing is reported */
  private unreachable(error: unknown) {
    if (this.stopped) return;
    this.retryOrFail(error instanceof TusError ? error : new TusError(String(error)));
  }

  private async create() {
    const { endpoint, metadata } = this.options;
    let reply: Reply;
    try {
      reply = await this.send("POST", endpoint, { "Upload-Length": String(this.file.size), "Upload-Metadata": encodeMetadata(metadata) });
    } catch (e) {
      return this.unreachable(e);
    }
    if (this.stopped) return;
    const { response } = reply;
    if (response.status < 200 || response.status >= 300) return this.retryOrFail(this.failedWith(response, "The upload could not be created"));
    const location = response.header("Location");
    if (!location) return this.retryOrFail(new TusError("The server did not say where the upload is", response));
    this.url = new URL(location, new URL(endpoint, window.location.href)).toString();
    this.offset = 0;
    this.remember();
    // Nothing to send: the creation was all of it
    if (this.file.size === 0) return this.finish(response);
    this.options.onProgress(0, this.file.size);
    void this.sendChunk();
  }

  /** Asks the server how much it has of an upload it knows, then sends the rest */
  private async ask() {
    const url = this.url!;
    let reply: Reply;
    try {
      reply = await this.send("HEAD", url);
    } catch (e) {
      return this.unreachable(e);
    }
    if (this.stopped) return;
    const { response } = reply;
    if (response.status >= 200 && response.status < 300) {
      const offset = Number.parseInt(response.header("Upload-Offset") ?? "", 10);
      if (!Number.isInteger(offset) || offset < 0 || offset > this.file.size) return this.retryOrFail(new TusError("The server's answer about the upload is not valid", response));
      this.offset = offset;
      if (offset === this.file.size) {
        this.options.onProgress(offset, this.file.size);
        return this.finish(response);
      }
      this.options.onProgress(offset, this.file.size);
      return void this.sendChunk();
    }
    // The server no longer has it (or won't let this session near it): a new upload takes its place
    if (response.status === 404 || response.status === 410 || response.status === 403) {
      this.forget();
      this.url = null;
      return void this.create();
    }
    this.retryOrFail(this.failedWith(response, "The upload could not be looked up"));
  }

  private async sendChunk() {
    const url = this.url!;
    const start = this.offset;
    const end = Math.min(start + this.options.chunkSize, this.file.size);
    let reply: Reply;
    try {
      reply = await this.send(
        "PATCH",
        url,
        { "Content-Type": "application/offset+octet-stream", "Upload-Offset": String(start) },
        this.file.slice(start, end),
        (loaded) => !this.stopped && this.options.onProgress(start + Math.min(loaded, end - start), this.file.size),
      );
    } catch (e) {
      return this.unreachable(e);
    }
    if (this.stopped) return;
    const { response } = reply;
    if (response.status < 200 || response.status >= 300) return this.retryOrFail(this.failedWith(response, "The upload failed"));
    const offset = Number.parseInt(response.header("Upload-Offset") ?? "", 10);
    if (!Number.isInteger(offset) || offset < start || offset > this.file.size) return this.retryOrFail(new TusError("The server's answer about the upload is not valid", response));
    this.offset = offset;
    this.options.onProgress(offset, this.file.size);
    if (offset === this.file.size) return this.finish(response);
    void this.sendChunk();
  }

  // ───────────── Ending ─────────────

  private finish(lastResponse: TusResponse) {
    this.forget();
    this.options.onSuccess({ lastResponse });
  }

  private retryOrFail(error: TusError) {
    if (this.stopped) return;
    // More was sent than before the last retry: the server is making progress, so the delays start over
    if (this.offset > this.offsetBeforeRetry) this.retryAttempt = 0;
    const { retryDelays } = this.options;
    if (this.retryAttempt < retryDelays.length && this.options.onShouldRetry(error)) {
      const delay = retryDelays[this.retryAttempt++];
      this.offsetBeforeRetry = this.offset;
      this.timer = setTimeout(() => this.start(), delay);
      return;
    }
    this.options.onError(error);
  }

  // ───────────── The address kept for resuming ─────────────

  private remember() {
    try {
      this.storageKey = `tus::${this.options.fingerprint}::${Math.floor(Math.random() * 1e12)}`;
      localStorage.setItem(
        this.storageKey,
        JSON.stringify({ size: this.file.size, metadata: this.options.metadata, creationTime: new Date().toString(), uploadUrl: this.url, urlStorageKey: this.storageKey }),
      );
    } catch {
      // Storage blocked or full: the upload works, it just can't be continued after a reload
      this.storageKey = null;
    }
  }

  /** Forgets the address kept for this upload (and any other under its fingerprint that this one continued) */
  private forget() {
    try {
      if (this.storageKey) localStorage.removeItem(this.storageKey);
      if (this.url) {
        const prefix = `tus::${this.options.fingerprint}::`;
        for (const key of Object.keys(localStorage)) {
          if (!key.startsWith(prefix)) continue;
          if (JSON.parse(localStorage.getItem(key) ?? "{}").uploadUrl === this.url) localStorage.removeItem(key);
        }
      }
    } catch {
      // Nothing kept
    }
    this.storageKey = null;
  }
}
