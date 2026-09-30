# syntax=docker/dockerfile:1
#
# ThirtyFile
#
#   docker build -t thirtyfile .
#   docker run -d --restart unless-stopped -p 8080:8080 -v /srv/thirtyfile/data:/data -v /srv/thirtyfile/storage:/storage thirtyfile
#
# Build arguments:
#   VERSION  version written to the image labels (default: dev)
#
# Multi-platform (amd64 + arm64; the backend is cross-compiled, so no emulation is needed):
#   docker buildx build --platform linux/amd64,linux/arm64 -t <registry>/thirtyfile:<version> --push .

# Base images are pinned to a version and digest, so two builds of one commit use the same images. Dependabot proposes
# updates every week. The Rust version matches server/rust-toolchain.toml.
#
# The runtime image is a single static program, so with `--sbom=true` the build stages of the frontend and the backend
# are scanned as well (BUILDKIT_SBOM_SCAN_STAGE): their SBOMs list the npm packages and the build tools. The program
# carries the list of crates built into it (cargo auditable), which image scanners such as Trivy read; the crates are
# also listed in THIRD-PARTY-NOTICES.

# ───────────── 1) Frontend ─────────────
# The output is platform-independent, so multi-platform builds only build it once on the build machine's platform
FROM --platform=$BUILDPLATFORM node:26.10.0-alpine3.24@sha256:0b36e8c136b94cd4fcf02188228e76c31ad5872eef3fec8cbd2eee500cfd9e80 AS web
ARG BUILDKIT_SBOM_SCAN_STAGE=true
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0
# Node no longer includes corepack (since version 25), which installs the pnpm version named in package.json
RUN npm install --global corepack && corepack enable
WORKDIR /src/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN --mount=type=cache,id=pnpm-store,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build
# Licence notices of the npm packages the interface is built from, with the licence files they ship (pnpm looks the
# packages up in its store)
RUN --mount=type=cache,id=pnpm-store,target=/root/.local/share/pnpm/store \
    pnpm licenses list --prod --json > /tmp/licences.json \
 && node scripts/third-party-notices.mjs < /tmp/licences.json > /src/notices-web.txt

# ───────────── 2) Backend (with the frontend embedded) ─────────────
# Runs on the build machine's platform and cross-compiles for the target platform with xx,
# so arm64 images are built at native speed on amd64 machines (and vice versa) without emulation
FROM --platform=$BUILDPLATFORM tonistiigi/xx:1.9.0@sha256:c64defb9ed5a91eacb37f96ccc3d4cd72521c4bd18d5442905b95e2226b0e707 AS xx
# cargo auditable 0.7.6 writes the list of crates into the program, for scanners. Taken from its release for the build
# machine, checked against the checksum published with it (Dependabot doesn't see it: update the version and both
# checksums by hand)
FROM scratch AS cargo-auditable-amd64
ADD --checksum=sha256:42b66c852fbb9074a9ca356279a92eb753f48dde16017b8c82f48dcd05d6c856 \
    https://github.com/rust-secure-code/cargo-auditable/releases/download/v0.7.6/cargo-auditable-x86_64-unknown-linux-musl.tar.xz /cargo-auditable.tar.xz
FROM scratch AS cargo-auditable-arm64
ADD --checksum=sha256:57265fbd87e9277fbd850c74177d17e5a6f51f15e2803643db65c187b0d4feda \
    https://github.com/rust-secure-code/cargo-auditable/releases/download/v0.7.6/cargo-auditable-aarch64-unknown-linux-musl.tar.xz /cargo-auditable.tar.xz
FROM cargo-auditable-${BUILDARCH} AS cargo-auditable

FROM --platform=$BUILDPLATFORM rust:1.98.1-alpine3.24@sha256:7cc1c22d77d9432f7fe012a70e6d3e555af54c2a6832700ed7d553f1769ae89f AS server
ARG BUILDKIT_SBOM_SCAN_STAGE=true
COPY --from=xx / /
# clang/lld cross-compile the C code of aws-lc (TLS crypto) and SQLite; cmake/perl are used by aws-lc's build script
RUN apk add --no-cache clang lld cmake make perl
RUN --mount=from=cargo-auditable,target=/tmp/cargo-auditable \
    tar -xJf /tmp/cargo-auditable/cargo-auditable.tar.xz -C /tmp \
 && mv /tmp/cargo-auditable-*/cargo-auditable /usr/local/bin/ \
 && rm -r /tmp/cargo-auditable-*
ARG TARGETPLATFORM
ARG TARGETARCH
# C runtime of the target platform (musl libc, libgcc) for linking
RUN xx-apk add --no-cache musl-dev gcc
WORKDIR /src/server
COPY server/ ./
COPY --from=web /src/web/dist /src/web/dist
# The version `thirtyfile --version` and /api/health report
ARG VERSION=dev
ENV THIRTYFILE_VERSION=$VERSION
# Dependencies and build output are cached, so rebuilds after code changes only recompile what changed
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=thirtyfile-target-${TARGETARCH},target=/src/server/target \
    xx-cargo auditable build --release --locked \
 && install -m 755 "target/$(xx-cargo --print-target-triple)/release/thirtyfile" /thirtyfile \
 && xx-verify --static /thirtyfile

# ───────────── 3) Third-party notices ─────────────
# The licences of the crates and npm packages built into ThirtyFile require passing on their notices. cargo about
# collects those of the crates (settings: server/about.toml, which accepts the licences deny.toml allows). The same for
# every platform, so built once, on the build machine's platform, beside the backend.
# cargo about 0.9.2 is taken from its release for the build machine, checked against the checksum published with it
# (Dependabot doesn't see it: update the version and both checksums by hand)
FROM scratch AS cargo-about-amd64
ADD --checksum=sha256:9099a59e820c38a68b9d65f300662a567d56562f9a10f6aa4c7e86c17c2566af \
    https://github.com/EmbarkStudios/cargo-about/releases/download/0.9.2/cargo-about-0.9.2-x86_64-unknown-linux-musl.tar.gz /cargo-about.tar.gz
FROM scratch AS cargo-about-arm64
ADD --checksum=sha256:af5169282fb6f84e13471493f405437e43ac517744c9ae12fbe2cdf0a6f0e5a8 \
    https://github.com/EmbarkStudios/cargo-about/releases/download/0.9.2/cargo-about-0.9.2-aarch64-unknown-linux-musl.tar.gz /cargo-about.tar.gz
FROM cargo-about-${BUILDARCH} AS cargo-about

FROM --platform=$BUILDPLATFORM rust:1.98.1-alpine3.24@sha256:7cc1c22d77d9432f7fe012a70e6d3e555af54c2a6832700ed7d553f1769ae89f AS notices
RUN --mount=from=cargo-about,target=/tmp/cargo-about \
    tar -xzf /tmp/cargo-about/cargo-about.tar.gz -C /tmp \
 && mv /tmp/cargo-about-*/cargo-about /usr/local/bin/ \
 && rm -r /tmp/cargo-about-*
WORKDIR /src/server
# Only what decides the dependencies: code changes don't redo this
COPY server/Cargo.toml server/Cargo.lock server/about.toml server/about.hbs ./
COPY --from=web /src/notices-web.txt /tmp/
# cargo metadata reads every crate of the lock file, including those for other systems, hence the full fetch;
# after it, cargo about only reads the downloaded crates (--offline), so the result depends on nothing else
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    cargo fetch --locked \
 && cargo about generate --offline --locked --fail -o /tmp/notices-rust.txt about.hbs \
 && { echo "ThirtyFile includes the following third-party software, under the licences reproduced below."; \
      echo "ThirtyFile itself is licensed under the Apache License 2.0 (LICENSE, next to this file)."; \
      echo; cat /tmp/notices-rust.txt; echo; echo; cat /tmp/notices-web.txt; } > /THIRD-PARTY-NOTICES

# ───────────── 4) Runtime files ─────────────
# Everything the runtime image contains besides the server, prepared on the build machine's platform: the runtime
# stage is empty (scratch) and runs no commands, so building it for another architecture needs no emulation
FROM --platform=$BUILDPLATFORM alpine:3.24.2@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 AS rootfs
# ca-certificates: certificate validation for S3, single sign-on and FTPS (without it every https connection fails)
# tzdata: allows setting the time zone with TZ (log exports, archive file names)
# passwd/group: names for user 1000 and root (the server switches user by number and doesn't read them itself)
# tmp: the usual temporary folder, for anything that expects one (ThirtyFile's own temporary files are in /data/tmp)
RUN apk add --no-cache ca-certificates tzdata \
 && mkdir -p /rootfs/etc/ssl/certs /rootfs/usr/share /rootfs/tmp /data /storage \
 && chmod 1777 /rootfs/tmp \
 && cp /etc/ssl/certs/ca-certificates.crt /rootfs/etc/ssl/certs/ \
 && cp -r /usr/share/zoneinfo /rootfs/usr/share/ \
 && printf '%s\n' "root:x:0:0:root:/root:/sbin/nologin" "drive:x:1000:1000::/data:/sbin/nologin" > /rootfs/etc/passwd \
 && printf '%s\n' "root:x:0:" "drive:x:1000:" > /rootfs/etc/group

# ───────────── 5) Runtime ─────────────
# The runtime image starts empty: the backend is a statically linked musl binary and needs nothing else (no shell, no
# package manager), which leaves less to scan and to update
FROM scratch
ARG VERSION=dev
LABEL org.opencontainers.image.title="ThirtyFile" \
      org.opencontainers.image.description="A lightweight self-hosted file manager" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.source="https://github.com/ThirtyFile/ThirtyFile" \
      org.opencontainers.image.url="https://thirtyfile.github.io/ThirtyFile/"

COPY --from=rootfs /rootfs/ /
COPY --from=rootfs --chown=1000:1000 /data /data
COPY --from=rootfs --chown=1000:1000 /storage /storage
COPY --from=server /thirtyfile /usr/local/bin/thirtyfile
# Apache-2.0 requires distributing a copy of the license with the software, and the licences of the crates and npm
# packages inside require their notices
COPY LICENSE /usr/share/licenses/thirtyfile/LICENSE
COPY --from=notices /THIRD-PARTY-NOTICES /usr/share/licenses/thirtyfile/THIRD-PARTY-NOTICES

# /data: database, settings, thumbnails and uploads in progress
# /storage: file contents of the built-in storage location
# The container starts as root, gives both folders to user 1000 when needed (Docker creates missing
# host folders as root), then runs as user 1000. With `--user`, it runs as that user and skips this.
ENV PATH=/usr/local/bin \
    THIRTYFILE_DATA=/data \
    THIRTYFILE_STORAGE=/storage \
    THIRTYFILE_RUN_AS=1000:1000 \
    THIRTYFILE_ADDR=0.0.0.0:8080

VOLUME ["/data", "/storage"]
EXPOSE 8080

# Requests /api/health on the address from THIRTYFILE_ADDR (no need to change this when the port changes)
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --start-interval=2s --retries=3 CMD ["thirtyfile", "health"]

ENTRYPOINT ["thirtyfile"]
