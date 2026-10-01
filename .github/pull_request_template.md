## What changes

<!-- One or two sentences: what is different for the people who use ThirtyFile. Closes #123 -->

## How it was tested

<!-- What you tried, in the browser or with commands -->

## Checklist

- [ ] The title says what changes, in plain words (it becomes the line in the release notes)
- [ ] One label: `enhancement`, `bug`, `performance`, `accessibility`, `documentation`, `dependencies` or `maintenance`
- [ ] The code is formatted (`cargo fmt` in `server/`, `pnpm format` in `web/`), and `scripts/check.sh` passes (the server's formatting, tests and clippy; the interface's formatting, translations, types, lint and tests; the end-to-end test)
- [ ] New interface text has a Traditional Chinese translation
- [ ] The guides in `site/docs/` are updated if behaviour changes
