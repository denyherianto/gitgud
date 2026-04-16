#!/usr/bin/env sh

set -eu

REPO="${GITHUB_REPO:-denyherianto/gitgud}"
BIN_NAME="${BIN_NAME:-gg}"
INSTALL_DIR="${GG_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${GG_VERSION:-latest}"

usage() {
  cat <<EOF
Install ${BIN_NAME} from GitHub Releases.

Usage:
  ./install.sh
  ./install.sh --version v0.1.0
  ./install.sh --dir /usr/local/bin

Options:
  --version <tag>  Install a specific GitHub release tag instead of the latest release
  --dir <path>     Install directory (default: ${INSTALL_DIR})
  --help           Show this help text

Environment:
  GITHUB_REPO      GitHub repo slug (default: ${REPO})
  GG_VERSION       Release tag to install, or "latest"
  GG_INSTALL_DIR   Install directory (default: ${INSTALL_DIR})
EOF
}

log() {
  printf '%s\n' "$*"
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || fail "--version requires a value"
      VERSION="$2"
      shift 2
      ;;
    --dir)
      [ "$#" -ge 2 ] || fail "--dir requires a value"
      INSTALL_DIR="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

need_cmd uname
need_cmd mktemp
need_cmd chmod
need_cmd mkdir

if command -v curl >/dev/null 2>&1; then
  FETCH_TOOL="curl"
elif command -v wget >/dev/null 2>&1; then
  FETCH_TOOL="wget"
else
  fail "missing required command: curl or wget"
fi

if command -v tar >/dev/null 2>&1; then
  ARCHIVE_TOOL="tar"
elif command -v unzip >/dev/null 2>&1; then
  ARCHIVE_TOOL="unzip"
else
  fail "missing required archive tool: tar or unzip"
fi

detect_os() {
  case "$(uname -s)" in
    Darwin) printf 'darwin' ;;
    Linux) printf 'linux' ;;
    *) fail "unsupported operating system: $(uname -s)" ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) printf 'x86_64' ;;
    arm64|aarch64) printf 'arm64' ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
  esac
}

fetch_to_file() {
  url="$1"
  output="$2"

  if [ "$FETCH_TOOL" = "curl" ]; then
    curl --fail --location --silent --show-error "$url" --output "$output"
  else
    wget --quiet "$url" -O "$output"
  fi
}

fetch_text() {
  url="$1"

  if [ "$FETCH_TOOL" = "curl" ]; then
    curl --fail --location --silent --show-error "$url"
  else
    wget --quiet -O - "$url"
  fi
}

resolve_version() {
  if [ "$VERSION" != "latest" ]; then
    printf '%s' "$VERSION"
    return
  fi

  api_url="https://api.github.com/repos/${REPO}/releases/latest"
  response="$(fetch_text "$api_url")" || fail "failed to resolve latest release from ${api_url}"
  tag="$(printf '%s\n' "$response" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$tag" ] || fail "could not parse latest release tag from GitHub API"
  printf '%s' "$tag"
}

install_archive() {
  version_tag="$1"
  os_name="$2"
  arch_name="$3"
  tmpdir="$4"

  archive_base="${BIN_NAME}-${os_name}-${arch_name}"
  archive_path="${tmpdir}/${archive_base}.tar.gz"
  download_url="https://github.com/${REPO}/releases/download/${version_tag}/${archive_base}.tar.gz"

  log "Downloading ${download_url}"
  fetch_to_file "$download_url" "$archive_path" || fail "failed to download release archive"

  if [ "$ARCHIVE_TOOL" = "tar" ]; then
    tar -xzf "$archive_path" -C "$tmpdir"
  else
    fail "release archive is .tar.gz but tar is not available"
  fi

  extracted_bin="${tmpdir}/${BIN_NAME}"
  [ -f "$extracted_bin" ] || fail "archive did not contain ${BIN_NAME}"

  mkdir -p "$INSTALL_DIR"
  cp "$extracted_bin" "${INSTALL_DIR}/${BIN_NAME}"
  chmod +x "${INSTALL_DIR}/${BIN_NAME}"
}

OS_NAME="$(detect_os)"
ARCH_NAME="$(detect_arch)"
VERSION_TAG="$(resolve_version)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT INT TERM HUP

log "Installing ${BIN_NAME} ${VERSION_TAG} for ${OS_NAME}/${ARCH_NAME}"
install_archive "$VERSION_TAG" "$OS_NAME" "$ARCH_NAME" "$TMPDIR"

log "Installed ${BIN_NAME} to ${INSTALL_DIR}/${BIN_NAME}"

case ":$PATH:" in
  *":${INSTALL_DIR}:"*)
    log "Run '${BIN_NAME} --help' to verify the installation."
    ;;
  *)
    log "Add ${INSTALL_DIR} to your PATH, then run '${BIN_NAME} --help'."
    ;;
esac
