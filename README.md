# lightwispr

Minimal, local voice dictation for Linux. Hold a key, speak, release: the text lands where your cursor is.

A lightweight, open-source take on Wispr Flow for Wayland/Hyprland. Everything runs on your machine, on the CPU. No account, no cloud, no telemetry.

- **Fast and accurate:** NVIDIA Parakeet TDT 0.6B v3 (25 European languages, detected automatically, with punctuation and casing). About 18× real time on a desktop CPU, so a 10 s sentence takes roughly 0.5 s.
- **Types into the focused field:** acts as a Wayland input method, so text is committed directly without fake keystrokes, and umlauts and emoji work. Apps without input-method support get a paste. With no text field focused, the text goes to the clipboard.
- **Never types into password fields:** text meant for a password or PIN field goes to the clipboard instead.
- **One small binary:** a Rust daemon plus a CLI. Uses about 0.9 GB RAM with the model loaded and about 35 MB when switched off. Uses no CPU while idle. The mic is only open while you hold the key.
- **Calm UI:** synthesized ceramic-keyboard sounds and an optional [Noctalia](https://github.com/noctalia-dev) bar widget with a live level meter, the last 10 transcriptions and settings.

## Requirements

- Hyprland (hotkey and paste) on Wayland, PipeWire (or ALSA)
- `wl-clipboard`
- Rust toolchain to build (`cargo`)
- ~700 MB disk for the model

## Install

```sh
git clone https://github.com/jan-stecher/lightwispr
cd lightwispr
./install.sh
```

This builds the binary into `~/.local/bin`, downloads the model (pinned revision, SHA-256 verified) and starts the `lightwispr.service` user service. The daemon registers its hotkey in Hyprland on its own, so you don't need to edit your Hyprland config.

## Usage

| | |
|---|---|
| **Hold Right Ctrl** | record while held, transcribe on release |
| Tap (< 250 ms) | ignored |
| Esc / any other key while recording | cancel |

Other presets: `Ctrl + Super`, `Super + Alt`. Pick one in the bar panel or run `lightwispr hotkey ctrl_super`.

```
lightwispr status | history | toggle | cancel
lightwispr power on|off          # off unloads the model and frees its RAM
lightwispr hotkey right_ctrl|ctrl_super|super_alt
lightwispr sound on|off, volume 0.35
lightwispr sounds                # preview the UI sounds
```

### Browsers and other apps

- **Chrome, Brave, Chromium, Electron:** enable the Wayland IME with `--enable-wayland-ime --wayland-text-input-version=3` (e.g. in `~/.config/chrome-flags.conf`). Without it, lightwispr falls back to pasting.
- **GTK, Qt, Firefox, kitty, foot …:** work out of the box.
- **XWayland apps and Warp:** get the text pasted (Ctrl+V, or Ctrl+Shift+V in terminals). Your previous clipboard text is restored afterwards.

## Noctalia bar widget

The plugin lives in [`noctalia/`](noctalia). Point a local Noctalia plugin source at a folder that contains it, then enable it:

```sh
mkdir -p ~/.local/share/noctalia-local-plugins
ln -s "$PWD/noctalia" ~/.local/share/noctalia-local-plugins/lightwispr
# add a [[plugin]] row for "lightwispr/lightwispr" to that folder's catalog.toml (see noctalia/plugin.toml)
noctalia msg plugins source add local path ~/.local/share/noctalia-local-plugins
noctalia msg plugins enable lightwispr/lightwispr
```

Then add `lightwispr/lightwispr:mic` to a bar section. **Left click** opens the panel (status, record, history, shortcut, sounds, on/off) and **right click** starts or stops a dictation.

## Files

| | |
|---|---|
| Settings | `~/.config/lightwispr/config.toml` |
| Last 10 transcriptions (the only stored data) | `~/.local/state/lightwispr/history.json` |
| Model | `~/.local/share/lightwispr/models/` |
| Live state, socket | `$XDG_RUNTIME_DIR/lightwispr/` |

Audio is only kept in memory and never written to disk.

## How it works

```
hotkey ─▶ mic (PipeWire, 16 kHz) ─▶ Parakeet (ONNX, int8, CPU) ─▶ input method commit
                                                              └▶ paste / clipboard fallback
```

While you record, the daemon puts Hyprland into a small `lightwispr` submap: releasing the key finishes the take, and any other key cancels it. Those binds leave the submap on their own, so the keyboard can never get stuck. More background is in [docs/PLAN.md](docs/PLAN.md).

## Credits

- Speech model: [NVIDIA Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3), licensed **CC-BY-4.0**; ONNX export by [istupakov](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx)
- Inference: [transcribe-rs](https://github.com/cjpais/transcribe-rs) (from [Handy](https://github.com/cjpais/Handy)), ONNX Runtime

## License

[MIT](LICENSE)
