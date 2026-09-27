<img src="web/public/favicon.svg" width="72" alt="ThirtyFile logo: a tiger's head" />

# ThirtyFile

[![Build](https://github.com/ThirtyFile/ThirtyFile/actions/workflows/build.yml/badge.svg)](https://github.com/ThirtyFile/ThirtyFile/actions/workflows/build.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Docker image](https://img.shields.io/badge/docker-ghcr.io%2Fthirtyfile%2Fthirtyfile-2563eb?logo=docker&logoColor=white)](https://github.com/ThirtyFile/ThirtyFile/pkgs/container/thirtyfile)

A self-hosted file manager that looks and works like Windows File Explorer, in your browser.

**[Website and guides](https://thirtyfile.github.io/ThirtyFile/)**

![The ThirtyFile file list](site/assets/files-light.webp)

- Tabs, folder tree, drag and drop, right-click menus and keyboard shortcuts
- Preview Word, PowerPoint and Excel in the browser; edit Excel and text files online
- Share with a link: password, expiry date and download limit
- Keep files on a local disk or NAS, S3-compatible storage, SFTP or FTP
- Personal, company and team spaces, with roles and size limits
- Sign in with a password, or with Microsoft, Google or GitHub
- English and Traditional Chinese, dark mode, works on phones

## Install

You need [Docker](https://docs.docker.com/get-docker/).

**docker run**

```bash
docker run -d --name thirtyfile \
  -p 8080:8080 \
  -v thirtyfile-data:/data \
  -e THIRTYFILE_ADMIN_PASSWORD='<a password of at least 8 characters>' \
  ghcr.io/thirtyfile/thirtyfile:latest
```

**Docker Compose**

```bash
curl -O https://raw.githubusercontent.com/ThirtyFile/ThirtyFile/main/compose.yaml
THIRTYFILE_ADMIN_PASSWORD='<a password of at least 8 characters>' docker compose up -d
```

Open <http://localhost:8080> and sign in as `admin` with that password.

## Guides

| | |
| --- | --- |
| [First steps](https://thirtyfile.github.io/ThirtyFile/docs/first-steps.html) | What to set up after installing |
| [Working with files](https://thirtyfile.github.io/ThirtyFile/docs/files.html) | Uploading, organising, shortcuts |
| [Sharing](https://thirtyfile.github.io/ThirtyFile/docs/sharing.html) | Share links, roles and spaces |
| [Settings](https://thirtyfile.github.io/ThirtyFile/docs/settings.html) | Options for the container |
| [Putting it on the internet](https://thirtyfile.github.io/ThirtyFile/docs/internet.html) | HTTPS and reverse proxies |
| [Upgrade and backup](https://thirtyfile.github.io/ThirtyFile/docs/backup.html) | Keeping it up to date and safe |
| [Troubleshooting](https://thirtyfile.github.io/ThirtyFile/docs/help.html) | Common problems |

## Development

You need Rust 1.89+, Node 22.12+ and pnpm 11.

```bash
# Terminal 1: backend
cd server
THIRTYFILE_ADMIN_PASSWORD=changeme123 cargo run

# Terminal 2: frontend, at http://127.0.0.1:5173
cd web
pnpm install
pnpm dev
```

Checks before committing:

```bash
cd server && cargo test && cargo clippy --all-targets
cd ../web && pnpm typecheck && node scripts/check-i18n.mjs
```

| Folder | Contents |
| --- | --- |
| `server/` | Backend (Rust) |
| `web/` | Frontend (React). Traditional Chinese translations are in `web/src/lib/i18n/zh-TW/` |
| `site/` | The website and guides, published to GitHub Pages |

Every push to `main` is tested and published as `ghcr.io/thirtyfile/thirtyfile:latest`. A tag such as `v1.2.3` publishes `1.2.3` and `1.2` as well.

## License

ThirtyFile is licensed under the [Apache License 2.0](LICENSE). Unless you state otherwise, any contribution you submit for inclusion is licensed under the same terms.
