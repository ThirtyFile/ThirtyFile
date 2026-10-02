# Contributing to ThirtyFile

Thank you for helping. The full guide is on the website: **[Contributing](https://thirtyfile.github.io/ThirtyFile/docs/contributing.html)**.

## Problems and ideas

[Open an issue](https://github.com/ThirtyFile/ThirtyFile/issues/new/choose) and pick the form that fits: a problem, an idea, an accessibility barrier, the guides, or an improvement to the code. Look through the [existing issues](https://github.com/ThirtyFile/ThirtyFile/issues) first.

Security problems are reported privately: see the [security policy](SECURITY.md).

## Changing the code

1. For anything larger than a small fix, agree on the idea in an issue first.
2. Create a branch from `main`: `feat/…`, `fix/…`, `docs/…` or `chore/…`.
3. Format the code (`cd server && cargo fmt`, `cd web && pnpm format`) and run the checks, the same ones that run on GitHub:
   ```bash
   scripts/check.sh
   ```
4. Open a pull request with a title that says what changes, one label (`enhancement`, `bug`, `performance`, `accessibility`, `documentation`, `dependencies` or `maintenance`), and `Closes #123` in the description.

Code, comments and guides are written in English. Text in the interface needs its translation in every required language in `web/src/lib/i18n/<lang>/` (Traditional Chinese for now; `node scripts/check-i18n.mjs` lists what is missing). When behaviour changes, update the guide in `site/docs/` that describes it.

By contributing, you agree that your contribution is licensed under the [Apache License 2.0](../LICENSE), and you agree to follow the [code of conduct](CODE_OF_CONDUCT.md).
