#!/usr/bin/env bash
# Genera Lector.app y Lector.dmg en una Mac (sirve para Apple Silicon e Intel).
#   bash scripts/build-macos.sh
set -euo pipefail
cd "$(dirname "$0")/../src-tauri"

xcode-select -p >/dev/null 2>&1 || { echo "Primero instala las herramientas de Xcode: xcode-select --install"; exit 1; }
if ! command -v cargo >/dev/null 2>&1; then
  echo "Instalando Rust…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi
cargo tauri --version >/dev/null 2>&1 || cargo install tauri-cli --version "^2" --locked
rustup target add aarch64-apple-darwin x86_64-apple-darwin

cargo tauri build --target universal-apple-darwin --bundles app,dmg

echo
echo "Listo:"
ls -1 target/universal-apple-darwin/release/bundle/dmg/*.dmg
