#!/bin/sh
# Installer for the Rust UA-101 driver (ua101d daemon, ua101-panel, ua101).
#
#   curl -fsSL https://raw.githubusercontent.com/devfrp/ua101-rs/main/packaging/linux/install.sh | sh
#
# Options (pass after `sh -s --` when piping):
#   --version vX.Y.Z      install a specific release (default: latest)
#   --prefix DIR          install binaries into DIR/bin (default: /usr/local)
#   --cli-only            skip the graphical panel and its runtime libraries
#   --no-deps             do not install runtime libraries with the package manager
#   --no-daemon           do not install/enable the ua101d driver daemon
#   --blacklist-kernel    stop the in-kernel snd-ua101 driver from binding the card
#   --uninstall           remove everything this script installed
#
# Environment: UA101_REPO (default devfrp/ua101-rs) selects the GitHub repository.

set -eu

REPO="${UA101_REPO:-devfrp/ua101-rs}"
VERSION="latest"
PREFIX="/usr/local"
CLI_ONLY=0
NO_DEPS=0
BLACKLIST=0
UNINSTALL=0
NO_DAEMON=0

RULES_DST="/etc/udev/rules.d/60-ua101.rules"
MODPROBE_DST="/etc/modprobe.d/ua101-rs-blacklist.conf"
ALOOP_DST="/etc/modprobe.d/ua101-aloop.conf"
MODLOAD_DST="/etc/modules-load.d/ua101.conf"
UNIT_DIR="/etc/systemd/user"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --prefix) PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --cli-only) CLI_ONLY=1; shift ;;
        --no-deps) NO_DEPS=1; shift ;;
        --blacklist-kernel) BLACKLIST=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        --no-daemon) NO_DAEMON=1; shift ;;
        -h|--help) sed -n '2,18p' "$0" 2>/dev/null || true; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

[ "$(uname -s)" = "Linux" ] || die "this installer is for Linux; see the releases page for Windows and macOS"

if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
elif command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
elif command -v doas >/dev/null 2>&1; then
    SUDO="doas"
else
    die "run as root or install sudo/doas"
fi

reload_udev() {
    if command -v udevadm >/dev/null 2>&1; then
        $SUDO udevadm control --reload-rules || true
        $SUDO udevadm trigger --subsystem-match=usb --attr-match=idVendor=0582 || true
    fi
}

if [ "$UNINSTALL" -eq 1 ]; then
    say "removing ua101-rs"
    systemctl --user disable --now ua101d.service 2>/dev/null || true
    $SUDO rm -f "$PREFIX/bin/ua101" "$PREFIX/bin/ua101-panel" "$PREFIX/bin/ua101d" \
        "$RULES_DST" "$MODPROBE_DST" "$ALOOP_DST" "$MODLOAD_DST" "$UNIT_DIR/ua101d.service" \
        /etc/wireplumber/wireplumber.conf.d/51-ua101.conf \
        "/usr/share/applications/ua101-panel.desktop"
    reload_udev
    say "done"
    exit 0
fi

# --- platform detection ----------------------------------------------------

case "$(uname -m)" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) die "unsupported architecture: $(uname -m) (x86_64 and aarch64 are built)" ;;
esac

LIBC="gnu"
if [ -f /etc/alpine-release ] || (ldd --version 2>&1 | grep -qi musl); then
    LIBC="musl"
fi

DISTRO_ID="unknown"
DISTRO_LIKE=""
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    DISTRO_ID="${ID:-unknown}"
    DISTRO_LIKE="${ID_LIKE:-}"
fi

if [ "$LIBC" = "musl" ]; then
    if [ "$ARCH" != "x86_64" ]; then
        die "musl builds are only published for x86_64"
    fi
    ASSET="ua101-linux-x86_64-musl.tar.gz"
    if [ "$CLI_ONLY" -eq 0 ]; then
        warn "musl system detected: installing the command-line tool only (no panel)"
        CLI_ONLY=1
    fi
else
    ASSET="ua101-linux-${ARCH}.tar.gz"
fi

# --- runtime libraries for the panel ----------------------------------------

pkg_family() {
    for id in $DISTRO_ID $DISTRO_LIKE; do
        case "$id" in
            debian|ubuntu|linuxmint|pop|elementary|raspbian|kali|zorin) echo apt; return ;;
            fedora|rhel|centos|rocky|almalinux|nobara) echo dnf; return ;;
            arch|manjaro|endeavouros|garuda|cachyos|artix) echo pacman; return ;;
            opensuse*|suse|sles) echo zypper; return ;;
            alpine) echo apk; return ;;
            void) echo xbps; return ;;
            gentoo) echo emerge; return ;;
        esac
    done
    echo unknown
}

install_deps() {
    family="$(pkg_family)"
    say "installing panel runtime libraries ($family)"
    case "$family" in
        apt)
            $SUDO apt-get update -qq
            $SUDO apt-get install -y -qq libxkbcommon0 libwayland-client0 libwayland-egl1 \
                libegl1 libgl1 libx11-6 libxcursor1 libxrandr2 libxi6 ca-certificates ;;
        dnf)
            $SUDO dnf install -y -q libxkbcommon libwayland-client libwayland-egl \
                libglvnd-egl libglvnd-glx libX11 libXcursor libXrandr libXi ;;
        pacman)
            $SUDO pacman -S --needed --noconfirm libxkbcommon wayland libglvnd \
                libx11 libxcursor libxrandr libxi ;;
        zypper)
            $SUDO zypper --non-interactive install libxkbcommon0 libwayland-client0 \
                libwayland-egl1 Mesa-libEGL1 Mesa-libGL1 libX11-6 libXcursor1 libXrandr2 libXi6 ;;
        xbps)
            $SUDO xbps-install -Sy libxkbcommon wayland libglvnd libX11 libXcursor libXrandr libXi ;;
        *)
            warn "unknown distribution '$DISTRO_ID': install libxkbcommon, wayland, libGL/EGL and X11 client libraries yourself" ;;
    esac
}

# --- download ---------------------------------------------------------------

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        die "curl or wget is required"
    fi
}

if [ "$VERSION" = "latest" ]; then
    BASE="https://github.com/$REPO/releases/latest/download"
else
    BASE="https://github.com/$REPO/releases/download/$VERSION"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

say "downloading $ASSET ($VERSION) from $REPO"
fetch "$BASE/$ASSET" "$TMP/$ASSET"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS"

say "verifying checksum"
expected="$(grep " $ASSET\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$expected" ] || die "$ASSET is not listed in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$TMP/$ASSET" | cut -d' ' -f1)"
else
    actual="$(openssl dgst -sha256 "$TMP/$ASSET" | awk '{print $NF}')"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $ASSET"

tar -xzf "$TMP/$ASSET" -C "$TMP"
SRC="$TMP/ua101-linux"
[ -d "$SRC" ] || die "unexpected archive layout"

# --- install ----------------------------------------------------------------

if [ "$CLI_ONLY" -eq 0 ] && [ "$NO_DEPS" -eq 0 ]; then
    install_deps
fi

say "installing binaries into $PREFIX/bin"
$SUDO install -d "$PREFIX/bin"
$SUDO install -m 755 "$SRC/ua101" "$PREFIX/bin/ua101"
if [ "$CLI_ONLY" -eq 0 ] && [ -f "$SRC/ua101-panel" ]; then
    $SUDO install -m 755 "$SRC/ua101-panel" "$PREFIX/bin/ua101-panel"
    if [ -f "$SRC/ua101-panel.desktop" ]; then
        $SUDO install -d /usr/share/applications
        sed "s|@BINDIR@|$PREFIX/bin|" "$SRC/ua101-panel.desktop" > "$TMP/ua101-panel.desktop"
        $SUDO install -m 644 "$TMP/ua101-panel.desktop" /usr/share/applications/ua101-panel.desktop
    fi
fi

if [ "$NO_DAEMON" -eq 0 ] && [ -f "$SRC/ua101d" ]; then
    say "installing the ua101d driver daemon"
    $SUDO install -m 755 "$SRC/ua101d" "$PREFIX/bin/ua101d"
    $SUDO install -d /etc/modprobe.d /etc/modules-load.d "$UNIT_DIR"
    $SUDO install -m 644 "$SRC/ua101-aloop.conf" "$ALOOP_DST"
    $SUDO install -m 644 "$SRC/ua101-modules.conf" "$MODLOAD_DST"
    if [ -f "$SRC/51-ua101.conf" ]; then
        # PipeWire/WirePlumber: pro-audio profile, 10 out / 12 in, hide the
        # daemon's side of the loopback card.
        $SUDO install -d /etc/wireplumber/wireplumber.conf.d
        $SUDO install -m 644 "$SRC/51-ua101.conf" /etc/wireplumber/wireplumber.conf.d/51-ua101.conf
        systemctl --user restart wireplumber 2>/dev/null || true
    fi
    sed "s|@BINDIR@|$PREFIX/bin|" "$SRC/ua101d.service" > "$TMP/ua101d.service"
    $SUDO install -m 644 "$TMP/ua101d.service" "$UNIT_DIR/ua101d.service"
    if ! grep -q '\[UA101RS' /proc/asound/cards 2>/dev/null; then
        if lsmod 2>/dev/null | grep -q '^snd_aloop'; then
            warn "snd-aloop is already loaded with other options; reboot to apply $ALOOP_DST"
        else
            $SUDO modprobe snd-aloop || warn "could not load snd-aloop"
        fi
    fi
    if [ "$(id -u)" -ne 0 ] && command -v systemctl >/dev/null 2>&1; then
        systemctl --user daemon-reload || true
        systemctl --user enable --now ua101d.service \
            || warn "could not start ua101d; run: systemctl --user enable --now ua101d"
    else
        warn "enable the daemon as your user: systemctl --user enable --now ua101d"
    fi
fi

say "installing udev rule (access without root)"
$SUDO install -d /etc/udev/rules.d
$SUDO install -m 644 "$SRC/60-ua101.rules" "$RULES_DST"
reload_udev

if [ "$BLACKLIST" -eq 1 ]; then
    say "blacklisting the in-kernel snd-ua101 driver"
    $SUDO install -d /etc/modprobe.d
    printf '# Installed by ua101-rs: the Rust user-space driver owns the UA-101.\nblacklist snd_ua101\n' \
        | $SUDO tee "$MODPROBE_DST" >/dev/null
    $SUDO modprobe -r snd_ua101 2>/dev/null || warn "snd_ua101 is in use; it stays loaded until reboot"
fi

say "installed: $("$PREFIX/bin/ua101" --version 2>/dev/null || echo ua101)"
echo "Replug the UA-101, then try:  ua101 list && ua101 info"
