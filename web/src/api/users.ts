import { request, get, post, enc, qs, toParams } from "@/api/client";
import type { Job, Group, UserRow } from "@/api/types";

/** Accounts and groups (administrators) */
export const usersApi = {
  users: () => get<UserRow[]>("/admin/users"),
  /** A page of accounts, by id */
  usersPage: (after: number, limit: number, q = "", signal?: AbortSignal) =>
    get<UserRow[]>(`/admin/users${qs({ after: String(after), limit: String(limit), q: q.trim() || undefined })}`, signal),
  /** `personal_space` / `personal_location`: "My files" and its storage location; left out, the system settings decide */
  createUser: (req: Partial<Omit<UserRow, "personal_space" | "personal_location">> & { password: string; personal_space?: boolean; personal_location?: string }) =>
    post<UserRow>("/admin/users", req),
  updateUser: (id: number, req: Partial<UserRow> & { password?: string }) => request<UserRow>("PATCH", enc`/admin/users/${id}`, req),
  /** Deletes a user: their personal space's files are moved to another space (move_to, a space id) or deleted (delete_files) */
  deleteUser: (id: number, files: { move_to?: string; delete_files?: boolean } = {}) => request<Job>("DELETE", enc`/admin/users/${id}` + qs(toParams(files))),
  /** Gives a user a personal space on a storage location (the system setting's when left out) */
  addPersonalSpace: (id: number, location_id?: string) => post<UserRow>(enc`/admin/users/${id}/personal-space`, { location_id }),
  /** Removes a user's personal space: its files are moved to another space (move_to) or deleted (delete_files) */
  removePersonalSpace: (id: number, files: { move_to?: string; delete_files?: boolean }) => request<Job>("DELETE", enc`/admin/users/${id}/personal-space` + qs(toParams(files))),
  groups: () => get<Group[]>("/admin/groups"),
  createGroup: (req: { name: string; description?: string; members?: number[] }) => post<{ id: number }>("/admin/groups", req),
  updateGroup: (id: number, req: { name?: string; description?: string; members?: number[] }) => request("PATCH", enc`/admin/groups/${id}`, req),
  deleteGroup: (id: number) => request("DELETE", enc`/admin/groups/${id}`),
};
