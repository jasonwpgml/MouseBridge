#!/bin/bash
# Installs this fork's Lan Mouse on a Mac and sets up the Windows PCs next to it.
#
#   setup-mac.sh [--repo OWNER/NAME] [--tag TAG] HOST:POSITION[:IP]...
#
# HOST is a Windows PC, POSITION is where its screen sits next to the Mac
# (left|right|top|bottom). Without IP the PC is looked up as HOST.local.
# Keys sent to the PCs swap Cmd and Ctrl and turn F18 into 한/영.
#
# Run it while Deskflow still works: for each PC it copies a PowerShell
# command to the clipboard to paste there, then replaces Deskflow on the Mac.
set -euo pipefail

REPO=jasonwpgml/lan-mouse
TAG=
SCRIPT_REF=feat/key-remap
CLIENTS=()
while [ $# -gt 0 ]; do
    case $1 in
        --repo) REPO=$2; shift 2 ;;
        --tag) TAG=$2; shift 2 ;;
        -h | --help) sed -n '2,11p' "$0"; exit 0 ;;
        *) CLIENTS+=("$1"); shift ;;
    esac
done
if [ ${#CLIENTS[@]} -eq 0 ]; then
    echo "사용법: setup-mac.sh HOST:POSITION[:IP]...  (예: DESKTOP-ABC:top)" >&2
    exit 1
fi
for c in "${CLIENTS[@]}"; do
    case $(echo "$c" | cut -d: -f2) in
        left | right | top | bottom) ;;
        *) echo "위치는 left/right/top/bottom 중 하나여야 해요: $c" >&2; exit 1 ;;
    esac
done

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

is_private_ip() {
    echo "$1" | grep -qE '^(10\.|192\.168\.|172\.(1[6-9]|2[0-9]|3[01])\.)'
}

# HOST.local's LAN address. Asks mDNS directly: some ISP resolvers answer
# .local names with a public address, faster than the real mDNS reply.
lan_ip() {
    local out="$TMP/mdns-$1" pid ip
    dns-sd -G v4 "$1.local" >"$out" 2>/dev/null &
    pid=$!
    sleep 2
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    while read -r ip; do
        if is_private_ip "$ip"; then echo "$ip"; return; fi
    done < <(awk '$2 == "Add" {print $6}' "$out")
}

my_lan_ip() {
    local ifc ip
    for ifc in $(route -n get default 2>/dev/null | awk '/interface:/ {print $2}') en0 en1; do
        ip=$(ipconfig getifaddr "$ifc" 2>/dev/null || true)
        if [ -n "$ip" ] && is_private_ip "$ip"; then echo "$ip"; return; fi
    done
}

opposite() {
    case $1 in left) echo right ;; right) echo left ;; top) echo bottom ;; bottom) echo top ;; esac
}

# --- install -------------------------------------------------------------

step "Lan Mouse 내려받기"
case $(uname -m) in
    arm64) ASSET=lan-mouse-macos-arm64.zip ;;
    *) ASSET=lan-mouse-macos-intel.zip ;;
esac
if [ -n "$TAG" ]; then
    API="https://api.github.com/repos/$REPO/releases/tags/$TAG"
else
    API="https://api.github.com/repos/$REPO/releases?per_page=1"
fi
URL=$(curl -fsSL "$API" | grep -oE "\"browser_download_url\": *\"[^\"]*/$ASSET\"" | head -1 | cut -d'"' -f4 || true)
if [ -z "$URL" ]; then
    echo "$REPO 릴리스에서 $ASSET 을(를) 찾지 못했어요." >&2
    exit 1
fi
echo "$URL"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl -fL --progress-bar -o "$TMP/lan-mouse.zip" "$URL"
ditto -x -k "$TMP/lan-mouse.zip" "$TMP"

pkill -f "Lan Mouse.app/Contents/MacOS/lan-mouse" 2>/dev/null || true
if [ -w /Applications ]; then APP="/Applications/Lan Mouse.app"; else APP="$HOME/Applications/Lan Mouse.app"; fi
mkdir -p "$(dirname "$APP")"
rm -rf "$APP"
ditto "$TMP/Lan Mouse.app" "$APP"
echo "설치: $APP"

# --- certificate -----------------------------------------------------------

CONF_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/lan-mouse"
CONF="$CONF_DIR/config.toml"
CERT="$CONF_DIR/lan-mouse.pem"
mkdir -p "$CONF_DIR"
if [ ! -s "$CERT" ]; then
    # the daemon creates the certificate before anything else; give it an
    # empty config so it doesn't reach for any screen edge meanwhile
    mkdir -p "$TMP/cfg"
    XDG_CONFIG_HOME="$TMP/cfg" "$APP/Contents/MacOS/lan-mouse" --cert-path "$CERT" daemon >/dev/null 2>&1 &
    pid=$!
    for _ in $(seq 50); do [ -s "$CERT" ] && break; sleep 0.1; done
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
fi
FINGERPRINT=$(awk '/-BEGIN CERTIFICATE-/ {f=1; next} /-END CERTIFICATE-/ {f=0} f' "$CERT" |
    base64 -D | shasum -a 256 | cut -d' ' -f1 | sed 's/../&:/g; s/:$//')

# --- Windows PCs -----------------------------------------------------------

MAC_NAME=$(scutil --get LocalHostName)
MAC_IP=$(my_lan_ip)
if [ -z "$MAC_IP" ]; then
    echo "이 Mac의 공유기 내부 IP를 찾지 못했어요. Wi-Fi 연결을 확인해 주세요." >&2
    exit 1
fi
PS_URL="https://raw.githubusercontent.com/$REPO/$SCRIPT_REF/scripts/setup-windows.ps1"
SAVED="$CONF_DIR/windows-setup.txt"
: >"$SAVED"
for c in "${CLIENTS[@]}"; do
    IFS=: read -r host pos _ <<<"$c"
    cmd="& ([scriptblock]::Create((irm '$PS_URL'))) -MacName '$MAC_NAME' -MacIp '$MAC_IP' -Side $(opposite "$pos") -MacFingerprint '$FINGERPRINT'"
    printf '# %s\n%s\n\n' "$host" "$cmd" >>"$SAVED"

    step "$host 설정"
    printf '%s' "$cmd" | pbcopy
    echo "PowerShell 명령을 클립보드에 복사했어요. Deskflow로 $host 에 가서:"
    echo "  1. 시작 메뉴에서 PowerShell을 열고"
    echo "  2. 붙여넣기(Cmd+V) 후 Enter"
    echo "  3. 방화벽 창이 뜨면 '개인 네트워크' 허용"
    printf '끝나면 여기서 Enter (꺼져 있으면 s 입력 후 Enter): '
    read -r answer </dev/tty
    if [ "$answer" = s ]; then echo "건너뜀 — 나중에 $SAVED 의 명령을 쓰면 돼요."; fi
done

# --- Mac -------------------------------------------------------------------

step "Mac 설정"
for proc in Deskflow deskflow deskflow-core deskflow-server; do
    pkill -x "$proc" 2>/dev/null || true
done

TRUSTED=
if [ -f "$CONF" ]; then
    cp "$CONF" "$CONF.bak-$(date +%Y%m%d-%H%M%S)"
    # keep devices already trusted through the Authorize dialog
    TRUSTED=$(awk '/^\[/ {keep = ($0 == "[authorized_fingerprints]")} keep' "$CONF")
fi
{
    echo "# setup-mac.sh 가 만든 파일 — 배치를 바꾸려면 스크립트를 다시 실행하세요"
    for c in "${CLIENTS[@]}"; do
        IFS=: read -r host pos ip <<<"$c"
        [ -n "$ip" ] || ip=$(lan_ip "$host")
        printf '\n[[clients]]\nhostname = "%s"\nposition = "%s"\n' "$host" "$pos"
        if [ -n "$ip" ]; then
            printf 'ips = ["%s"]\n' "$ip"
        else
            echo "$host 의 IP를 찾지 못했어요 — 켜져 있으면 다시 실행하거나 $host:$pos:<IP> 로 지정하세요." >&2
        fi
        echo "activate_on_startup = true"
    done
    for c in "${CLIENTS[@]}"; do
        host=${c%%:*}
        printf '\n[key_remap."%s"]\n' "$host"
        echo 'KeyLeftMeta = "KeyLeftCtrl"'
        echo 'KeyLeftCtrl = "KeyLeftMeta"'
        echo 'KeyRightmeta = "KeyRightCtrl"'
        echo 'KeyRightCtrl = "KeyRightmeta"'
        echo 'KeyF18 = "KeyHanguel"'
    done
    if [ -n "$TRUSTED" ]; then printf '\n%s\n' "$TRUSTED"; fi
} >"$CONF"
echo "설정: $CONF"

# every build is ad-hoc signed, so a grant for the previous build no longer
# applies even though System Settings still shows it switched on
tccutil reset All de.feschber.LanMouse >/dev/null 2>&1 || true
open "$APP"
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"

# start with every login; macOS may ask to let Terminal control System Events
if ! osascript -e "tell application \"System Events\" to if not (exists login item \"Lan Mouse\") then make login item at end with properties {path:\"$APP\", hidden:true}" >/dev/null 2>&1; then
    echo "로그인 항목 추가에 실패했어요 — 시스템 설정 > 일반 > 로그인 항목에서 Lan Mouse를 추가하세요." >&2
fi

step "남은 일"
echo "1. 손쉬운 사용 목록에서 Lan Mouse를 켜고, Lan Mouse를 껐다 켜기"
echo "2. Windows에서 Mac 쪽으로 넘어올 때 Mac에 'Authorize' 창이 뜨면 허용"
echo "3. Deskflow가 로그인 시 자동 실행되면 Deskflow 설정에서 끄기"
