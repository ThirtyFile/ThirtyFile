## What changes

<!-- One or two sentences: what is different for the people who use ThirtyFile. Closes #123 -->

## How it was tested

<!-- What you tried, in the browser or with commands -->

## Checklist

- [ ] The title says what changes, in plain words (it becomes the line in the release notes)
- [ ] One label: `bug`, `enhancement`, `documentation` or `dependencies`
- [ ] `cargo test` and `cargo clippy --all-targets` pass
- [ ] `pnpm typecheck` and `node scripts/check-i18n.mjs` pass (new interface text has a Traditional Chinese translation)
- [ ] The guides in `site/docs/` are updated if behaviour changes
