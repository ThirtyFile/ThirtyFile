import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckIcon, PrinterIcon } from "lucide-react";
import { toast } from "sonner";
import { api, driveName, type AccessReview, type ReviewGrant, type ReviewPlace, type ReviewPrincipal } from "@/api";
import { keys } from "@/api/queryKeys";
import { SettingsFrame } from "@/admin/SettingsFrame";
import { ErrorState } from "@/components/ErrorState";
import { ExportCsvButton, csvTime, type CsvColumn } from "@/components/logs/exportCsv";
import { Button } from "@/components/ui/button";
import { NativeSelect } from "@/components/ui/native-select";
import { ROLE_LABEL } from "@/lib/drives";
import { t, tServer } from "@/lib/i18n";
import { errorMessage, formatDateTime } from "@/lib/utils";

/** How often a review can be asked for, in days (0: never) */
const EVERY = [0, 30, 90, 180, 365];

/** Where something is: its space and folders; in a personal space only whose it is, as administrators see elsewhere */
export function placeLabel(p: ReviewPlace) {
  if (p.space_kind === "personal") return t("My files of {name}", { name: p.space });
  const space = driveName({ kind: p.space_kind, name: p.space });
  return p.path?.length ? [space, ...p.path].join(" › ") : space;
}

function principalLabel(p: ReviewPrincipal) {
  if (p.kind === "everyone") return t("Everyone");
  return p.name === "(deleted)" ? tServer(p.name) : p.name;
}

const until = (t: number | null) => (t ? formatDateTime(t) : "—");
const time = (t: number | null) => (t ? csvTime(t) : "");

function Grants({ grants }: { grants: ReviewGrant[] }) {
  if (grants.length === 0) return <p className="text-muted-foreground">{t("No one has access of their own here")}</p>;
  return (
    <ul className="grid gap-1">
      {grants.map((g) => (
        <li key={`${g.principal.kind}-${g.principal.id}`} className="flex flex-wrap items-baseline gap-x-2">
          <span className="font-medium">{principalLabel(g.principal)}</span>
          <span className="text-muted-foreground">
            {g.principal.kind === "group" ? t("Group") : g.principal.kind === "user" ? (g.principal.guest ? t("Guest") : t("Person")) : ""}
            {g.principal.kind === "group" && g.principal.guest && ` · ${t("Includes guests")}`}
          </span>
          <span>{ROLE_LABEL[g.role]}</span>
          {g.expires_at && <span className="text-muted-foreground">{t("Until {time}", { time: formatDateTime(g.expires_at) })}</span>}
        </li>
      ))}
    </ul>
  );
}

/**
 * Control panel › Access review: who can reach each space and folder, what each person can reach, and the ways in from
 * outside (links, S3 keys, FTP devices), with exports and a reminder to review now and then
 */
export function AccessReviewPage() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: keys.accessReview(), queryFn: ({ signal }) => api.accessReview(signal) });
  const [typed, setTyped] = useState("");
  const [guestsOnly, setGuestsOnly] = useState(false);
  // Opened for printing: every person's details
  const [printing, setPrinting] = useState(false);
  useEffect(() => {
    if (!printing) return;
    const done = () => setPrinting(false);
    window.addEventListener("afterprint", done);
    window.print();
    return () => window.removeEventListener("afterprint", done);
  }, [printing]);
  const r = q.data;
  const search = typed.trim().toLowerCase();
  const matches = (...texts: (string | null | undefined)[]) => !search || texts.some((s) => s?.toLowerCase().includes(search));
  const people = (r?.people ?? []).filter((p) => (!guestsOnly || p.guest) && matches(p.username, p.display_name, ...p.groups));
  const refresh = () => void qc.invalidateQueries({ queryKey: keys.accessReview() });
  const change = async (what: () => Promise<unknown>, done: string) => {
    try {
      await what();
      toast.success(done);
      refresh();
    } catch (e) {
      toast.error(errorMessage(e, t("Couldn't make the change")));
    }
  };
  return (
    <SettingsFrame item="access" onRefresh={refresh} searchPlaceholder={t("Search people and spaces")} onSearch={setTyped}>
      {q.error ? (
        <ErrorState message={q.error.message} onRetry={() => q.refetch()} />
      ) : !r ? (
        <p role="status">{t("Loading…")}</p>
      ) : (
        <>
          <section className="grid gap-2 rounded-lg border bg-card p-4 text-sm" aria-label={t("Review")}>
            <p>
              {r.review.last_reviewed_at
                ? t("Last reviewed {time} by {name}", { time: formatDateTime(r.review.last_reviewed_at), name: r.review.last_reviewed_by ?? "—" })
                : t("Not reviewed yet")}
              {r.review.due_at && ` · ${t("Next review due {time}", { time: formatDateTime(r.review.due_at) })}`}
            </p>
            <p className="text-xs text-muted-foreground">
              {t(
                "As of {time}. What has ended is left out. Items in personal spaces show only whose space they are in. Administrators manage team and company spaces without a grant of their own, which isn't listed.",
                { time: formatDateTime(r.generated_at) },
              )}
            </p>
            <div className="flex flex-wrap items-center gap-2 print:hidden">
              <label className="flex items-center gap-2">
                {t("Ask for a review")}
                <NativeSelect value={EVERY.includes(r.review.every_days) ? r.review.every_days : 0} onChange={(e) => change(() => api.setReviewReminder(Number(e.target.value)), t("Saved"))}>
                  {EVERY.map((d) => (
                    <option key={d} value={d}>
                      {d === 0 ? t("Never") : t("Every {n} days", { n: d })}
                    </option>
                  ))}
                </NativeSelect>
              </label>
              <Button size="sm" onClick={() => change(() => api.markReviewed(), t("Marked as reviewed"))}>
                <CheckIcon /> {t("Mark as reviewed")}
              </Button>
              <Button size="sm" variant="outline" onClick={() => setPrinting(true)}>
                <PrinterIcon /> {t("Print")}
              </Button>
            </div>
          </section>

          <ReviewSpaces r={r} matches={matches} />

          <section className="grid gap-3" aria-labelledby="review-people">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h2 id="review-people" className="text-base font-semibold">
                {t("People")}
              </h2>
              <div className="flex items-center gap-3 print:hidden">
                <label className="flex items-center gap-2 text-sm">
                  <input type="checkbox" className="size-4 accent-brand" checked={guestsOnly} onChange={(e) => setGuestsOnly(e.target.checked)} />
                  {t("Guests only")}
                </label>
                <ExportCsvButton
                  name="access-people"
                  load={async () => people.flatMap((p): PersonRow[] => (p.reach.length ? p.reach.map((x) => ({ p, x })) : [{ p, x: null }]))}
                  columns={PEOPLE_CSV}
                />
              </div>
            </div>
            {people.length === 0 && <p className="text-sm text-muted-foreground">{t("No one matches")}</p>}
            {people.map((p) => (
              <details key={p.id} open={printing || undefined} className="rounded-lg border bg-card p-3 text-sm">
                <summary className="cursor-default">
                  <span className="font-medium">{p.username}</span>
                  {p.display_name && p.display_name !== p.username && <span className="text-muted-foreground"> · {p.display_name}</span>}
                  {[p.guest && t("Guest"), p.admin && t("Administrator"), p.disabled && t("Disabled")].filter(Boolean).map((b) => (
                    <span key={b as string} className="ml-2 rounded bg-muted px-1.5 py-0.5 text-xs">
                      {b}
                    </span>
                  ))}
                  <span className="ml-2 text-xs text-muted-foreground">{t("{n} place|{n} places", { n: p.reach.length })}</span>
                </summary>
                <div className="mt-2 grid gap-2">
                  {p.groups.length > 0 && <p className="text-muted-foreground">{t("Groups: {names}", { names: p.groups.join(", ") })}</p>}
                  {p.reach.length === 0 ? (
                    <p className="text-muted-foreground">{t("No grant gives them access")}</p>
                  ) : (
                    <ul className="grid gap-1">
                      {p.reach.map((x, i) => (
                        <li key={i} className="flex flex-wrap gap-x-2">
                          <span>{placeLabel(x.place)}</span>
                          <span className="font-medium">{ROLE_LABEL[x.role]}</span>
                          <span className="text-muted-foreground">
                            {x.via === "direct" ? t("Directly") : x.via === "group" ? t("Through {group}", { group: x.group ?? "" }) : t("Everyone")}
                          </span>
                          {x.expires_at && <span className="text-muted-foreground">{t("Until {time}", { time: formatDateTime(x.expires_at) })}</span>}
                        </li>
                      ))}
                    </ul>
                  )}
                  {(p.app_passwords.length > 0 || p.ssh_keys.length > 0) && (
                    <p className="text-muted-foreground">
                      {[
                        ...p.app_passwords.map((a) =>
                          t("App password “{name}” ({access}), last used {time}", {
                            name: a.name,
                            access: a.scope === "read" ? t("Read files only") : t("Read and change files"),
                            time: until(a.last_used_at),
                          }),
                        ),
                        ...p.ssh_keys.map((k) => t("SSH key “{name}”, last used {time}", { name: k.name, time: until(k.last_used_at) })),
                      ].join(" · ")}
                    </p>
                  )}
                </div>
              </details>
            ))}
          </section>

          <ReviewOutside r={r} matches={matches} />
        </>
      )}
    </SettingsFrame>
  );
}

type Matches = (...texts: (string | null | undefined)[]) => boolean;

function ReviewSpaces({ r, matches }: { r: AccessReview; matches: Matches }) {
  const spaces = r.spaces.filter((s) =>
    matches(s.name, ...s.members.map((g) => g.principal.name), ...s.folders.flatMap((f) => [...(f.place.path ?? []), ...f.grants.map((g) => g.principal.name)])),
  );
  const rows: SpaceRow[] = spaces.flatMap((s): SpaceRow[] => [
    ...s.members.map((g) => ({ space: driveName(s), folder: "", restricted: false, g })),
    ...s.folders.flatMap((f): SpaceRow[] =>
      f.grants.length
        ? f.grants.map((g) => ({ space: driveName(s), folder: (f.place.path ?? []).join(" › "), restricted: f.restricted, g }))
        : [{ space: driveName(s), folder: (f.place.path ?? []).join(" › "), restricted: f.restricted, g: null }],
    ),
  ]);
  return (
    <>
      <section className="grid gap-3" aria-labelledby="review-spaces">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 id="review-spaces" className="text-base font-semibold">
            {t("Spaces and folders")}
          </h2>
          <span className="print:hidden">
            <ExportCsvButton name="access-spaces" load={async () => rows} columns={SPACES_CSV} />
          </span>
        </div>
        {spaces.map((s) => (
          <div key={s.id} className="grid gap-2 rounded-lg border bg-card p-3 text-sm">
            <h3 className="font-medium">{driveName(s)}</h3>
            <Grants grants={s.members} />
            {s.folders.map((f) => (
              <div key={f.place.node_id} className="grid gap-1 border-t pt-2">
                <p className="font-medium">
                  {placeLabel(f.place)}
                  {f.restricted && <span className="ml-2 rounded bg-muted px-1.5 py-0.5 text-xs font-normal">{t("Restricted folder")}</span>}
                </p>
                <Grants grants={f.grants} />
              </div>
            ))}
          </div>
        ))}
        {r.personal.length > 0 && (
          <div className="grid gap-1 rounded-lg border bg-card p-3 text-sm">
            <h3 className="font-medium">{t("Personal spaces")}</h3>
            <ul className="grid gap-1 text-muted-foreground">
              {r.personal
                .filter((p) => matches(p.owner))
                .map((p) => (
                  <li key={p.owner}>{t("{name} — shared items: {items}, grants: {grants}, links: {links}", { name: p.owner, items: p.items, grants: p.grants, links: p.links })}</li>
                ))}
            </ul>
          </div>
        )}
      </section>
      <section className="grid gap-3" aria-labelledby="review-ending">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 id="review-ending" className="text-base font-semibold">
            {t("Access with an end date")}
          </h2>
          <span className="print:hidden">
            <ExportCsvButton name="access-ending" load={async () => r.expiring} columns={ENDING_CSV} />
          </span>
        </div>
        {r.expiring.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("None")}</p>
        ) : (
          <ul className="grid gap-1 rounded-lg border bg-card p-3 text-sm">
            {r.expiring.map((e, i) => (
              <li key={i} className="flex flex-wrap gap-x-2">
                <span className="tabular-nums">{until(e.grant.expires_at)}</span>
                <span className="font-medium">{principalLabel(e.grant.principal)}</span>
                <span>{ROLE_LABEL[e.grant.role]}</span>
                <span className="text-muted-foreground">{placeLabel(e.place)}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </>
  );
}

function ReviewOutside({ r, matches }: { r: AccessReview; matches: Matches }) {
  const links = r.links.filter((l) => matches(l.creator, l.place.space, ...(l.place.path ?? [])));
  const buckets = r.buckets.filter((b) => matches(b.name, b.creator, b.place.space, ...(b.place.path ?? [])));
  const ftp = r.receive_folders.filter((f) => matches(f.name, f.username, f.creator, f.place.space, ...(f.place.path ?? [])));
  const rows = [
    ...links.map((l) => ({
      kind: t("Share link"),
      name: "",
      place: placeLabel(l.place),
      creator: l.creator,
      until: l.expires_at,
      details: l.password ? t("Password") : t("No password"),
      last: l.last_access,
    })),
    ...buckets.flatMap((b) =>
      b.keys.map((k) => ({
        kind: t("S3 key"),
        name: `${b.name} · ${k.description}`,
        place: placeLabel(b.place),
        creator: b.creator,
        until: k.expires_at,
        details: k.writable ? t("Read and change files") : t("Read files only"),
        last: k.last_used_at,
      })),
    ),
    ...ftp.map((f) => ({
      kind: t("FTP receive folder"),
      name: `${f.name} · ${f.username}`,
      place: placeLabel(f.place),
      creator: f.creator,
      until: f.expires_at,
      details: f.enabled ? t("On") : t("Off"),
      last: f.last_used_at,
    })),
  ];
  return (
    <section className="grid gap-3" aria-labelledby="review-outside">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="review-outside" className="text-base font-semibold">
          {t("Outside access")}
        </h2>
        <span className="print:hidden">
          <ExportCsvButton name="access-outside" load={async () => rows} columns={OUTSIDE_CSV} />
        </span>
      </div>
      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("None")}</p>
      ) : (
        <ul className="grid gap-1 rounded-lg border bg-card p-3 text-sm">
          {rows.map((x, i) => (
            <li key={i} className="flex flex-wrap gap-x-2">
              <span className="font-medium">{x.kind}</span>
              {x.name && <span>{x.name}</span>}
              <span className="text-muted-foreground">{x.place}</span>
              <span className="text-muted-foreground">{t("By {name}", { name: x.creator })}</span>
              <span className="text-muted-foreground">{x.details}</span>
              <span className="text-muted-foreground">{t("Until {time}", { time: until(x.until) })}</span>
              <span className="text-muted-foreground">{t("Last used {time}", { time: until(x.last) })}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

type SpaceRow = { space: string; folder: string; restricted: boolean; g: ReviewGrant | null };
const SPACES_CSV: CsvColumn<SpaceRow>[] = [
  { header: t("Space"), value: (x) => x.space },
  { header: t("Folder"), value: (x) => x.folder },
  { header: t("Restricted folder"), value: (x) => (x.restricted ? t("Yes") : "") },
  { header: t("Who"), value: (x) => (x.g ? principalLabel(x.g.principal) : "") },
  { header: t("Guest"), value: (x) => (x.g?.principal.guest ? t("Yes") : "") },
  { header: t("Role"), value: (x) => (x.g ? ROLE_LABEL[x.g.role] : "") },
  { header: t("Until"), value: (x) => time(x.g?.expires_at ?? null) },
];
type PersonRow = { p: AccessReview["people"][number]; x: AccessReview["people"][number]["reach"][number] | null };
const PEOPLE_CSV: CsvColumn<PersonRow>[] = [
  { header: t("Username"), value: ({ p }) => p.username },
  { header: t("Guest"), value: ({ p }) => (p.guest ? t("Yes") : "") },
  { header: t("Groups"), value: ({ p }) => p.groups.join(", ") },
  { header: t("Where"), value: ({ x }) => (x ? placeLabel(x.place) : "") },
  { header: t("Role"), value: ({ x }) => (x ? ROLE_LABEL[x.role] : "") },
  { header: t("Through"), value: ({ x }) => (x ? (x.via === "direct" ? t("Directly") : x.via === "group" ? (x.group ?? "") : t("Everyone")) : "") },
  { header: t("Until"), value: ({ x }) => time(x?.expires_at ?? null) },
  { header: t("App passwords"), value: ({ p }) => p.app_passwords.map((a) => a.name).join(", ") },
  { header: t("SSH keys"), value: ({ p }) => p.ssh_keys.map((k) => k.name).join(", ") },
];
const ENDING_CSV: CsvColumn<AccessReview["expiring"][number]>[] = [
  { header: t("Until"), value: (e) => time(e.grant.expires_at) },
  { header: t("Who"), value: (e) => principalLabel(e.grant.principal) },
  { header: t("Role"), value: (e) => ROLE_LABEL[e.grant.role] },
  { header: t("Where"), value: (e) => placeLabel(e.place) },
];
type OutsideRow = { kind: string; name: string; place: string; creator: string; until: number | null; details: string; last: number | null };
const OUTSIDE_CSV: CsvColumn<OutsideRow>[] = [
  { header: t("Kind"), value: (x) => x.kind },
  { header: t("Name"), value: (x) => x.name },
  { header: t("Where"), value: (x) => x.place },
  { header: t("Created by"), value: (x) => x.creator },
  { header: t("Access"), value: (x) => x.details },
  { header: t("Until"), value: (x) => time(x.until) },
  { header: t("Last used"), value: (x) => time(x.last) },
];
