import { t } from "@/lib/i18n";

/**
 * The page to go to after signing in: `next` when it is a page of this site, otherwise the files. Compared by origin
 * after the browser's own parsing, which drops tabs and line breaks and reads "\" as "/", so "/\t/host" or "/\host"
 * (another site to a browser) are caught too. The server's safe_next follows the same rule.
 */
export function safeNext(next: string | null | undefined, origin: string = location.origin): string {
  if (!next?.startsWith("/")) return "/files";
  try {
    const url = new URL(next, origin);
    return url.origin === origin ? url.pathname + url.search + url.hash : "/files";
  } catch {
    return "/files";
  }
}

const ERROR_COOKIE = "tf_sso_error";

/** The reason read last, for the same page asking again (React may run a state initialiser twice) */
let taken: { text: string; at: number } | null = null;

/**
 * Why a sign-in with another account failed, when the address says one did (`sso_error`): the text comes from the
 * cookie the server set with it, never from the address, so a link can't make the page show words of its own. The
 * cookie is removed once read.
 */
export function takeSsoError(params: URLSearchParams): string | null {
  if (!params.has("sso_error")) return null;
  const found = document.cookie
    .split(";")
    .map((c) => c.trim())
    .find((c) => c.startsWith(`${ERROR_COOKIE}=`) && c.length > ERROR_COOKIE.length + 1);
  let text = "";
  try {
    text = found ? decodeURIComponent(found.slice(ERROR_COOKIE.length + 1)) : "";
  } catch {
    text = "";
  }
  if (found) {
    document.cookie = `${ERROR_COOKIE}=; Path=/; Max-Age=0`;
    taken = text ? { text, at: Date.now() } : null;
  } else if (taken && Date.now() - taken.at < 2000) {
    text = taken.text;
  } else {
    taken = null;
  }
  return text || t("Sign-in failed");
}
