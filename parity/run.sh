#!/usr/bin/env bash
# Go 版 iapctl と Rust 版 releasectl の plan 出力を突き合わせる。
# plan は書き込みをしないので、本番のストアに対して流してよい。
#
#   GO_SRC=~/sonir/sonir-workspace/releasectl \
#   IAP_YAML=~/sonir/sonir-workspace/releasectl/iap.yaml \
#   ENV_FILE=~/sonir/sonir-workspace/releasectl/.env \
#   ./parity/run.sh
#
# PROVIDERS で対象を絞る（既定 googleplay。Rust 版に移した provider だけを並べる）。
# 出力（ケースの yaml と両方の結果）は OUT（既定 ../scratch/parity）に置く。
# ケースは実物の iap.yaml から作るので、リポジトリには入れない。
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
: "${GO_SRC:?GO_SRC（Go 版 releasectl のディレクトリ）を指定してください}"
: "${IAP_YAML:?IAP_YAML（実物の iap.yaml）を指定してください}"
PROVIDERS="${PROVIDERS:-googleplay}"
OUT="${OUT:-$root/../scratch/parity}"
# 下で GO_SRC に cd するので、相対パスはここで絶対パスにする
mkdir -p "$OUT" && OUT="$(cd "$OUT" && pwd)"

if [ -n "${ENV_FILE:-}" ]; then
  set -a; . "$ENV_FILE"; set +a
fi
# Go 版の Makefile と同じく、.env が相対パスを書いていれば GO_SRC からの相対とみなす
cd "$GO_SRC"

mkdir -p "$OUT/cases" "$OUT/go" "$OUT/rust"
python3 -I "$here/make_cases.py" "$IAP_YAML" "$OUT/cases" >/dev/null

echo "build: Go baseline ($GO_SRC)"
GOWORK=off go build -o "$OUT/iapctl-go" ./cmd/iapctl
echo "build: Rust"
cargo build --release -q --manifest-path "$root/Cargo.toml"
rust="$root/target/release/releasectl"

# 意図的な差分を揃える:
#  - コマンド名（iapctl → releasectl iap）
normalize() {
  sed -e 's/`iapctl apply`/`releasectl iap apply`/' -e 's/`iapctl retire`/`releasectl iap retire`/'
}

# plan はケースごと、retire-plan はそのままの iap.yaml でだけ比べる（どちらも書き込みなし）
fail=0
compare() { # <名前> <go の引数...> -- <rust の引数...>
  local n="$1"; shift
  local go_args=() rs_args=()
  while [ "$1" != "--" ]; do go_args+=("$1"); shift; done; shift
  rs_args=("$@")
  set +e
  "$OUT/iapctl-go" "${go_args[@]}" 2>&1 | normalize > "$OUT/go/$n.txt"
  local go_rc=${PIPESTATUS[0]}
  "$rust" "${rs_args[@]}" > "$OUT/rust/$n.txt" 2>&1
  local rs_rc=$?
  set -e
  if [ "$go_rc" != "$rs_rc" ]; then
    echo "✗ $n: exit code go=$go_rc rust=$rs_rc"; fail=1
  elif diff -u "$OUT/go/$n.txt" "$OUT/rust/$n.txt" > "$OUT/$n.diff"; then
    echo "✓ $n ($(grep -c '^  [^ ]' "$OUT/go/$n.txt") lines)"
    rm -f "$OUT/$n.diff"
  else
    echo "✗ $n: 出力が違う"; cat "$OUT/$n.diff"; fail=1
  fi
}

for f in "$OUT"/cases/*.yaml; do
  n="$(basename "$f" .yaml)"
  compare "$n" plan -config "$f" -only "$PROVIDERS" -- iap plan --config "$f" --only "$PROVIDERS"
done
base="$OUT/cases/00-baseline.yaml"
compare retire-plan retire-plan -config "$base" -only "$PROVIDERS" -- iap retire-plan --config "$base" --only "$PROVIDERS"
exit $fail
