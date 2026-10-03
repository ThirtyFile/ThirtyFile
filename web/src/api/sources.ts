//! Where file content is read from: the signed-in API, or a share link

import { request, enc, fetchOk } from "@/api/client";
import type { Node } from "@/api/types";

/** Source of file content URLs; signed-in files and public shares use the same components */
export interface FileSource {
  contentUrl(n: Node, download?: boolean): string;
  thumbUrl(n: Node): string;
  /** Keeps a thumbnail made in the browser (PDFs, videos) on the server; missing where that isn't possible (share links) */
  saveThumb?(n: Node, image: Blob): Promise<void>;
  /**
   * Link to download the given items from: one item goes in the URL; several are sent to the server, which answers
   * with a short-lived link (a URL holding hundreds of ids is too long for many reverse proxies)
   */
  downloadLink(ids: string[], signal?: AbortSignal): Promise<string>;
}

/** Download link for items at `base` (`/download` of the signed-in API or of a share) */
function downloadLink(base: string, ids: string[], signal?: AbortSignal): Promise<string> {
  const tz = new Date().getTimezoneOffset();
  if (ids.length === 1) return Promise.resolve(`/api${base}?ids=${encodeURIComponent(ids[0])}&tz=${tz}`);
  return request<{ url: string }>("POST", base, { ids, tz }, undefined, undefined, signal).then((r) => r.url);
}

export const privateSource: FileSource = {
  contentUrl: (n, download) => enc`/api/files/${n.id}/content` + (download ? "?download=1" : ""),
  thumbUrl: (n) => enc`/api/files/${n.id}/thumbnail?v=${n.updated_at}`,
  saveThumb: async (n, image) => void (await fetchOk(enc`/api/files/${n.id}/thumbnail`, { method: "PUT", body: image, headers: { "Content-Type": image.type } })),
  downloadLink: (ids, signal) => downloadLink("/download", ids, signal),
};

/** Where visitors of a share link that accepts files upload them */
export const shareUploadEndpoint = (token: string) => enc`/api/public/shares/${token}/uploads`;

export function shareSource(token: string): FileSource {
  const base = enc`/api/public/shares/${token}`;
  return {
    contentUrl: (n, download) => base + enc`/nodes/${n.id}/content` + (download ? "?download=1" : ""),
    thumbUrl: (n) => base + enc`/nodes/${n.id}/thumbnail?v=${n.updated_at}`,
    downloadLink: (ids, signal) => downloadLink(enc`/public/shares/${token}/download`, ids, signal),
  };
}
