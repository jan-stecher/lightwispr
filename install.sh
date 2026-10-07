#!/bin/sh
# Builds lightwispr, installs it for the current user and starts the service.
# Needs: Rust (cargo), Hyprland, PipeWire, wl-clipboard.
set -eu
cd "$(dirname "$0")"

cargo build --release --locked
install -Dm755 target/release/lightwispr "$HOME/.local/bin/lightwispr"
"$HOME/.local/bin/lightwispr" download-model

unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/lightwispr.service"
install -Dm644 contrib/lightwispr.service "$unit"
systemctl --user daemon-reload
systemctl --user enable --now lightwispr.service

echo
echo "lightwispr is running. Hold Right Ctrl, speak, release."
echo "Bar widget (Noctalia): see README.md, section 'Noctalia bar widget'."
