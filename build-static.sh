#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

mkdir -p dist

# Local builds default to offline; CI overrides with CARGO_FLAGS="" (or --locked).
read -r -a CARGO_FLAGS <<< "${CARGO_FLAGS:---offline}"

echo "[1/4] static CLI (everything)"
cargo rustc "${CARGO_FLAGS[@]}" --release -p everything -- -C target-feature=+crt-static
cp target/release/everything dist/everything

echo "[2/4] static daemon (everythingd)"
cargo rustc "${CARGO_FLAGS[@]}" --release -p everythingd -- -C target-feature=+crt-static
cp target/release/everythingd dist/everythingd

echo "[3/4] GTK3 GUI (links system GTK3, small binary)"
cargo build "${CARGO_FLAGS[@]}" --release -p everything-gtk
cp target/release/everything-gtk dist/everything-gtk

echo "[4/4] verify"
file dist/everything dist/everythingd dist/everything-gtk
ls -lh dist/

cat <<'NOTE'

说明 / NOTE:
- everything 与 everythingd 为静态链接（static-pie），可直接放到任意 x86-64 Linux 运行。
- everything-gtk 需要系统 GTK3 运行库（libgtk-3.so.0 等）。这是刻意的选择：
  复用系统库可使 GUI 二进制很小，且 GTK 无法现实地静态链接。
  无 GTK3 的系统请使用 CLI。
NOTE
