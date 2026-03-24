#!/bin/sh

set -eu

APP_NAME="apg"
REPO="${APG_REPO:-__GITHUB_REPOSITORY__}"
VERSION="${APG_VERSION:-latest}"
INSTALL_DIR="${APG_INSTALL_DIR:-${HOME}/.local/bin}"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'error: missing required command: %s\n' "$1" >&2
    exit 1
  }
}

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux) platform="unknown-linux-gnu" ;;
    Darwin) platform="apple-darwin" ;;
    *)
      printf 'error: unsupported operating system: %s\n' "$os" >&2
      exit 1
      ;;
  esac

  case "$arch" in
    x86_64|amd64) cpu="x86_64" ;;
    arm64|aarch64) cpu="aarch64" ;;
    *)
      printf 'error: unsupported architecture: %s\n' "$arch" >&2
      exit 1
      ;;
  esac

  printf '%s-%s' "$cpu" "$platform"
}

download_url() {
  target="$1"

  if [ "$VERSION" = "latest" ]; then
    printf 'https://github.com/%s/releases/latest/download/%s-%s.tar.gz' "$REPO" "$APP_NAME" "$target"
    return
  fi

  tag="$VERSION"
  case "$tag" in
    v*) ;;
    *) tag="v$tag" ;;
  esac

  printf 'https://github.com/%s/releases/download/%s/%s-%s.tar.gz' "$REPO" "$tag" "$APP_NAME" "$target"
}

main() {
  if [ "$REPO" = "__GITHUB_REPOSITORY__" ]; then
    printf 'error: APG_REPO is not set and the installer template was not release-patched.\n' >&2
    printf 'hint: rerun with APG_REPO=<owner>/<repo>.\n' >&2
    exit 1
  fi

  need_cmd curl
  need_cmd tar
  need_cmd install

  target="$(detect_target)"
  url="$(download_url "$target")"
  tmpdir="$(mktemp -d)"
  archive="$tmpdir/$APP_NAME.tar.gz"

  trap 'rm -rf "$tmpdir"' EXIT INT TERM

  printf 'downloading %s from %s\n' "$APP_NAME" "$url"
  curl --fail --location --silent --show-error "$url" --output "$archive"

  mkdir -p "$INSTALL_DIR"
  tar -xzf "$archive" -C "$tmpdir"
  install "$tmpdir/$APP_NAME" "$INSTALL_DIR/$APP_NAME"

  printf 'installed %s to %s\n' "$APP_NAME" "$INSTALL_DIR/$APP_NAME"
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
      printf 'warning: %s is not on your PATH\n' "$INSTALL_DIR" >&2
      ;;
  esac
}

main "$@"
