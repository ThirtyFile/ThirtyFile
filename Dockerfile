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

# ───────────── 1) Frontend ─────────────
# The output is platform-independent, so multi-platform builds only build it once on the build machine's platform
FROM --platform=$BUILDPLATFORM node:26.10.0-alpine3.24@sha256:0b36e8c136b94cd4fcf02188228e76c31ad5872eef3fec8cbd2eee500cfd9e80 AS web
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0
# Node no longer includes corepack (since version 25), which installs the pnpm version named in package.json
RUN npm install --global corepack && corepack enable
WORKDIR /src/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN --mount=type=cache,id=pnpm-store,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# ───────────── 2) Backend (with the frontend embedded) ─────────────
# Runs on the build machine's platform and cross-compiles for the target platform with xx,
# so arm64 images are built at native speed on amd64 machines (and vice versa) without emulation
FROM --platform=$BUILDPLATFORM tonistiigi/xx:1.9.0@sha256:c64defb9ed5a91eacb37f96ccc3d4cd72521c4bd18d5442905b95e2226b0e707 AS xx
FROM --platform=$BUILDPLATFORM rust:1.98.1-alpine3.24@sha256:7cc1c22d77d9432f7fe012a70e6d3e555af54c2a6832700ed7d553f1769ae89f AS server
COPY --from=xx / /
# clang/lld cross-compile the C code of aws-lc (TLS crypto) and SQLite; cmake/perl are used by aws-lc's build script
RUN apk add --no-cache clang lld cmake make perl
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
    xx-cargo build --release --locked \
 && install -m 755 "target/$(xx-cargo --print-target-triple)/release/thirtyfile" /thirtyfile \
 && xx-verify --static /thirtyfile

# ───────────── 3) Runtime files ─────────────
# Architecture-independent files, prepared on the build machine's platform so that the runtime stage
# needs no RUN step (and therefore no emulation when building for another architecture)
FROM --platform=$BUILDPLATFORM alpine:3.24.2@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 AS rootfs
# ca-certificates: certificate validation for S3, single sign-on and FTPS (without it every https connection fails)
# tzdata: allows setting the time zone with TZ (log exports, archive file names)
RUN apk add --no-cache ca-certificates tzdata \
 && mkdir -p /rootfs/etc/ssl/certs /rootfs/usr/share /data /storage \
 && cp /etc/ssl/certs/ca-certificates.crt /rootfs/etc/ssl/certs/ \
 && cp -r /usr/share/zoneinfo /rootfs/usr/share/ \
 && cp /etc/passwd /etc/group /rootfs/etc/ \
 && echo "drive:x:1000:1000::/data:/sbin/nologin" >> /rootfs/etc/passwd \
 && echo "drive:x:1000:" >> /rootfs/etc/group

# ───────────── 4) Runtime ─────────────
# The runtime image: the backend is a statically linked musl binary, independent of the build image
FROM alpine:3.24.2@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
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
# Apache-2.0 requires distributing a copy of the license with the software
COPY LICENSE /usr/share/licenses/thirtyfile/LICENSE

# /data: database, settings, thumbnails and uploads in progress
# /storage: file contents of the built-in storage location
# The container starts as root, gives both folders to user 1000 when needed (Docker creates missing
# host folders as root), then runs as user 1000. With `--user`, it runs as that user and skips this.
ENV THIRTYFILE_DATA=/data \
    THIRTYFILE_STORAGE=/storage \
    THIRTYFILE_RUN_AS=1000:1000 \
    THIRTYFILE_ADDR=0.0.0.0:8080

VOLUME ["/data", "/storage"]
EXPOSE 8080

# Requests /api/health on the address from THIRTYFILE_ADDR (no need to change this when the port changes)
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --retries=3 CMD ["thirtyfile", "health"]

ENTRYPOINT ["thirtyfile"]
