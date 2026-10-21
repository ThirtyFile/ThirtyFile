import { OfficeError, type Format, type LoadedFile, type SaveRequest, type StorageProvider } from "@office/sdk";
import { ApiError, type FileSource, type Node } from "@/api";
import { checkOoxml, enc, fetchOk, request } from "@/api/client";
import { t } from "@/lib/i18n";
import { MAX_OFFICE_PREVIEW_BYTES, MAX_OFFICE_PREVIEW_LABEL } from "@/lib/officeLimits";

interface Revision {
  version: number;
  etag: string;
}

/** ThirtyFile owns HTTP, authorization, versions and receipts. No server API comes from the SDK. */
export class OfficeStorage implements StorageProvider {
  readonly capabilities = { idempotentSave: true };
  version: number;
  onSaved?: (node: Node) => void;
  constructor(
    public node: Node,
    private source: FileSource,
    private editable: boolean,
    private allowDownload: boolean,
  ) {
    this.version = node.updated_at;
  }

  async load(_ref: { id: string }, options: { signal?: AbortSignal } = {}): Promise<LoadedFile> {
    const response = await fetchOk(this.source.contentUrl(this.node), { signal: options.signal, cache: "no-store" });
    if (Number(response.headers.get("content-length")) > MAX_OFFICE_PREVIEW_BYTES)
      throw new OfficeError("STORAGE_REJECTED", t("The file is too large (over {size}) to preview online. Download it to open it.", { size: MAX_OFFICE_PREVIEW_LABEL }));
    const content = new Uint8Array(checkOoxml(await response.arrayBuffer()));
    if (content.length > MAX_OFFICE_PREVIEW_BYTES)
      throw new OfficeError("STORAGE_REJECTED", t("The file is too large (over {size}) to preview online. Download it to open it.", { size: MAX_OFFICE_PREVIEW_LABEL }));
    const version = Number(response.headers.get("x-version"));
    const etag = response.headers.get("etag") ?? "";
    if (this.editable && (!response.headers.has("x-version") || !Number.isSafeInteger(version) || !etag))
      throw new OfficeError("STORAGE_INVALID", t("Couldn't read the file's revision. Reload the latest version."));
    this.version = version || this.node.updated_at;
    return {
      content,
      name: this.node.name,
      format: this.node.name.split(".").at(-1)!.toLowerCase() as Format,
      revision: JSON.stringify({ version: this.version, etag }),
      capabilities: { edit: this.editable, export: this.allowDownload },
    };
  }

  async save(_ref: { id: string }, save: SaveRequest, options: { signal?: AbortSignal } = {}): Promise<{ revision: string }> {
    if (!this.editable) throw new OfficeError("STORAGE_FORBIDDEN", t("This file is read-only"));
    const base = JSON.parse(save.baseRevision) as Revision;
    try {
      const node = await request<Node>(
        "PUT",
        enc`/files/${this.node.id}/content`,
        undefined,
        new Blob([save.content.slice().buffer]),
        {
          "Content-Type": "application/octet-stream",
          "X-Base-Version": String(base.version),
          ...(base.etag ? { "If-Match": base.etag } : {}),
          "Idempotency-Key": save.idempotencyKey,
        },
        options.signal,
      );
      this.node = node;
      this.version = node.updated_at;
      // Stored content is named by SHA-256; folder-space content has its own opaque tag, read on the next load.
      // Subsequent saves use the committed monotonic version; the initial ETag adds protection for external disk edits.
      this.onSaved?.(node);
      return { revision: JSON.stringify({ version: node.updated_at, etag: "" }) };
    } catch (error) {
      if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
        throw new OfficeError([409, 412].includes(error.status) ? "STORAGE_CONFLICT" : [401, 403].includes(error.status) ? "STORAGE_FORBIDDEN" : "STORAGE_REJECTED", error.message);
      }
      throw error;
    }
  }
}
