#!/bin/sh
# Builds lightwispr, installs it for the current user and starts the service.
# Needs: Rust (cargo), cmake, a C/C++ compiler and libclang, Hyprland, PipeWire, wl-clipboard.
# GPU for Whisper and the enhancement model (optional):
#   LIGHTWISPR_VULKAN=1 ./install.sh   (also needs vulkan-headers, spirv-headers, glslc)
set -eu
cd "$(dirname "$0")"

features=""
if [ "${LIGHTWISPR_VULKAN:-0}" = 1 ]; then
    features="--features lightwispr/vulkan,lightwispr-llm/vulkan"
fi
# shellcheck disable=SC2086
cargo build --release --locked --workspace $features
install -Dm755 target/release/lightwispr "$HOME/.local/bin/lightwispr"
install -Dm755 target/release/lightwispr-llm "$HOME/.local/bin/lightwispr-llm"
"$HOME/.local/bin/lightwispr" download-model

unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/lightwispr.service"
install -Dm644 contrib/lightwispr.service "$unit"
systemctl --user daemon-reload
systemctl --user enable --now lightwispr.service

echo
echo "lightwispr is running. Hold Right Ctrl, speak, release."
echo "Enhancement (optional): lightwispr download-model enhance, then pick a style in the bar or run: lightwispr mode casual"
echo "Bar widget (Noctalia): see README.md, section 'Noctalia bar widget'."
