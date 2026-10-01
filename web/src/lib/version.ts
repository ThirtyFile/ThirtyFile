// The release the server runs, which only people who are signed in are told (`version` in /api/auth/me)

/**
 * The page of a release's notes on GitHub; null for a build that isn't a release ("dev", or a version of someone's own
 * build that doesn't look like a release number)
 */
export function releaseNotesUrl(version: string): string | null {
  return /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version) ? `https://github.com/ThirtyFile/ThirtyFile/releases/tag/v${version}` : null;
}
