import { request, get, post, enc, qs, toParams } from "@/api/client";
import { localizeLocated } from "@/api/names";
import type { Node, Crumb, Role, PrincipalType, AccessInfo, Principal, CursorPage, SharedItem, ShareInfo, ShareFilter, ShareUpdate, ShareAccessOptions, PublicShare } from "@/api/types";

/** Share links, access given to people and groups, and the public pages of share links */
export const sharingApi = {
  /** With a node: every link on it the caller may manage; otherwise the caller's own links, or those matching the filter */
  shares: (nodeId?: string, filter: ShareFilter = {}, signal?: AbortSignal) => get<ShareInfo[]>(`/shares${qs(toParams({ ...filter, node_id: nodeId }))}`, signal),
  updateShare: (id: string, req: ShareUpdate) => request<ShareInfo>("PATCH", enc`/shares/${id}`, req),
  createShare: (req: { node_id: string; password?: string; expires_at?: number; max_downloads?: number } & Partial<ShareAccessOptions>) =>
    post<ShareInfo>("/shares", req),
  deleteShare: (id: string) => request("DELETE", enc`/shares/${id}`),
  access: (nodeId: string) => get<AccessInfo>(enc`/nodes/${nodeId}/access`),
  grant: (
    nodeId: string,
    req: {
      principal_type: PrincipalType;
      principal_id: number;
      role: Role;
      expires_at?: number | null;
    },
  ) => post(enc`/nodes/${nodeId}/access`, req),
  revoke: (grantId: number) => request("DELETE", enc`/grants/${grantId}`),
  directory: (q: string) => get<Principal[]>(`/directory${qs({ q })}`),
  sharedWithMe: () => get<SharedItem[]>("/shared-with-me").then((l) => l.map(localizeLocated)),
  publicShare: (token: string) => get<PublicShare>(enc`/public/shares/${token}`),
  unlockShare: (token: string, password: string) => post(enc`/public/shares/${token}/unlock`, { password }),
  publicNode: (token: string, id: string) => get<{ node: Node; path: Crumb[] }>(enc`/public/shares/${token}/nodes/${id}`),
  publicChildrenPage: (token: string, id: string, limit: number, after?: string, signal?: AbortSignal) =>
    get<CursorPage<Node>>(enc`/public/shares/${token}/nodes/${id}/children` + qs({ limit: String(limit), after }), signal),
};
