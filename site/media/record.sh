#!/usr/bin/env bash
# Re-record the knixl demo videos for the site.
#
#   site/media/record.sh               record every tape
#   site/media/record.sh drift pin     record just these
#
# Each tape gets a fresh throwaway project built from examples/, is recorded with VHS,
# and lands in site/public/media/ as <name>.webm, <name>.mp4 (faststart) and <name>.png.
# Work happens in $KNIXL_MEDIA_WORK (default: a temp dir removed on exit).
# Needs: mise (fetches vhs and ttyd), cargo, nix, nixfmt, git, bat, ffmpeg, Chrome/Chromium.
set -euo pipefail

VHS_VERSION=0.12.1
TTYD_VERSION=1.7.7
# install's package check evaluates against this nixpkgs (via NIX_PATH); also the flake baseline.
NIXPKGS_REV=241313f4e8e508cb9b13278c2b0fa25b9ca27163
ALL_TAPES=(generate drift oracle pin tui flake)

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
out="$repo/site/public/media"
examples="$repo/examples/hosts"

if [[ -n "${KNIXL_MEDIA_WORK:-}" ]]; then
    work=$KNIXL_MEDIA_WORK
    mkdir -p "$work"
else
    work=$(mktemp -d "${TMPDIR:-/tmp}/knixl-media.XXXXXX")
    trap 'rm -rf "$work"' EXIT
fi

tapes=("$@")
[[ ${#tapes[@]} -eq 0 ]] && tapes=("${ALL_TAPES[@]}")
for t in "${tapes[@]}"; do
    [[ -f "$here/tapes/$t.tape" ]] || { echo "record.sh: no tape named '$t'" >&2; exit 2; }
done

echo "==> building knixl"
(cd "$repo" && cargo build -p knixl --quiet)
mkdir -p "$work/bin" "$out"
ln -sf "$repo/target/debug/knixl" "$work/bin/knixl"

echo "==> fetching nixpkgs $NIXPKGS_REV"
nixpkgs=$(nix-instantiate --eval --raw -E \
    "builtins.fetchTarball \"https://github.com/NixOS/nixpkgs/archive/$NIXPKGS_REV.tar.gz\"")

# Everything the recorded shell sees. NIX_PATH pins <nixpkgs> so install's package check
# evaluates locally rather than against whatever channel the machine has.
export PATH="$work/bin:$PATH"
export KNIXL_FORMATTER=nixfmt
export KNIXL_PIN_RESOLVER="$here/stubs/pin-resolver"
export NIX_PATH="nixpkgs=$nixpkgs"
export BAT_THEME=ansi

# Copy the named example hosts into project $1.
hosts() {
    local dir=$1
    shift
    mkdir -p "$dir/hosts"
    for h in "$@"; do cp "$examples/$h.kdl" "$dir/hosts/"; done
}

flake_project() {
    mkdir -p "$1/hosts"
    cat >"$1/knixl.kdl" <<'EOF'
system {
    state-version "25.11"
    input "nixpkgs" url="github:NixOS/nixpkgs"
    input "disko" url="github:nix-community/disko" rev="ff8702b4de27f72b4c78573dfb89ec74e36abdf1" {
        follows nixpkgs="nixpkgs"
    }
}
EOF
    cat >"$1/hosts/web.kdl" <<EOF
host "web" {
    system "x86_64-linux"
    nixpkgs release="unstable" rev="$NIXPKGS_REV"

    web-service "example.com" {
        upstream "http://127.0.0.1:3000"
        acme email="ops@example.com"
        hardened #true
    }
}
EOF
    git -C "$1" init -q
}

# Prepare $1 (the project dir) for tape $2. Anything slow or networked happens here.
setup() {
    local p=$1
    case $2 in
        generate) hosts "$p" web nas ;;
        drift) hosts "$p" web ;;
        oracle)
            hosts "$p" web nas
            (cd "$p" && knixl generate)
            sed -i 's/timezone "Europe/timezon "Europe/' "$p/hosts/nas.kdl"
            ;;
        pin)
            hosts "$p" web
            (cd "$p" && knixl generate)
            nix-instantiate --eval -E '(import <nixpkgs> {}) ? curl' >/dev/null
            ;;
        tui) hosts "$p" web nas ;;
        flake)
            flake_project "$p"
            # Dry run in a copy, so the recording's `nix flake lock` finds its inputs cached.
            local warm="$p.warm"
            rm -rf "$warm"
            cp -r "$p" "$warm"
            (cd "$warm" && knixl upgrade --yes >/dev/null && knixl generate &&
                git add -A && nix flake lock ./generated 2>/dev/null && knixl check >/dev/null)
            rm -rf "$warm"
            ;;
    esac
}

for t in "${tapes[@]}"; do
    echo "==> $t"
    dir="$work/$t"
    rm -rf "$dir"
    mkdir -p "$dir"
    ln -s "$here/tapes" "$dir/tapes"
    setup "$dir/project" "$t"
    (cd "$dir" && KNIXL_DEMO_DIR="$dir/project" \
        mise exec "vhs@$VHS_VERSION" "ttyd@$TTYD_VERSION" -- \
        vhs -o raw.mp4 -o raw.webm "tapes/$t.tape" >"$dir/vhs.log")
    ffmpeg -loglevel error -y -i "$dir/raw.mp4" -c copy -movflags +faststart "$out/$t.mp4"
    cp "$dir/raw.webm" "$out/$t.webm"
    cp "$dir/poster.png" "$out/$t.png"
    ls -l "$out/$t".{webm,mp4,png} | awk '{print "    " $5 "\t" $NF}'
done
