# lightwispr: plan

A minimal, local, open-source Wispr Flow alternative for Linux (Wayland/Hyprland first).
Speak, release, and the text lands where your cursor is. Everything runs on your machine.

## Principles

- **Lightweight:** one Rust binary, low idle RAM. Models are loaded only when the feature is on.
- **Invisible:** controlled from the bar (Noctalia plugin) and a hotkey. No app window to open.
- **Private and minimal:** no telemetry. Audio stays in memory. The only persisted data is config plus the last 10 transcriptions.
- **Clean, state-of-the-art UI/UX:** native to the bar's theme, calm motion, satisfying sound.

## Pipeline

```
hotkey/bar ─▶ capture (PipeWire, 16 kHz mono, in memory)
          ─▶ STT: Parakeet TDT 0.6B v3 (ONNX int8, CPU)
          ─▶ [optional] enhance: small local LLM (llama.cpp, GGUF) with the mode's system prompt
          ─▶ deliver: commit into focused text field (input-method), else clipboard
          ─▶ history (ring buffer, 10)
```

### STT

- **Parakeet TDT 0.6B v3** (NVIDIA, CC-BY-4.0, 25 European languages, automatic language detection).
  int8 ONNX ~650 MB, ~1–1.5 GB RAM, CPU only.
- **Language:** automatic, per recording (no setting needed). Switching language mid-sentence is weak.
- **Later:** Whisper large-v3-turbo as an optional backend (99 languages, can pin a language).

### Enhancement (optional, toggle in bar)

- Small instruct model via llama.cpp, Q4 GGUF: Qwen3/3.5 0.6–0.8B (~0.5–1 GB) by default, 1.7–2B for better quality (~1.5–2 GB).
- **Off = unloaded:** the RAM is freed immediately (Parakeet output is already punctuated and cased).
- **Modes** = name + system prompt, user-defined in config, switchable from the bar. Built-in examples:
  - `clean`: remove fillers/false starts, fix punctuation, keep wording and language
  - `formal`: tidy into polished written prose
  - `translate-en`: translate to English
- Base rule in every prompt: output only the text, keep the input language unless the mode says otherwise.

## Delivery: focused input first, clipboard otherwise (must work)

- lightwispr registers as a Wayland **input method** (`zwp_input_method_v2`, supported by Hyprland).
  The compositor tells it when a text field gains focus (`activate`) via apps' `text-input-v3`.
- **Text field focused** → `commit_string`: inserts the text directly, Unicode-safe (umlauts, emoji), no fake keystrokes.
- **No text field focused** → put the text on the clipboard (`wl-clipboard`/data-control) + subtle bar feedback "copied".
- **Caveats to verify in the prototype:**
  - Chromium/Electron (Chrome, Brave) need `--enable-wayland-ime` (add to their flags files).
  - XWayland apps don't speak text-input-v3 → they get the clipboard path.
  - Terminals (kitty, Warp) and Qt apps: check support case by case.
  - Only one input method can be active (fcitx5/ibus aren't installed here, so no conflict).
- Fallback idea if needed: `wtype` for apps that lack text-input-v3 but are known to accept typing.

## Sound design

- Cues: **start**, **stop/submit**, **cancel**, **error**. Short (30–80 ms), soft, "creamy" like a ceramic/thocky keyboard:
  low body (~150–300 Hz decaying sine/resonant filter) + a short soft high transient, no harsh clicks.
- **Synthesized by our own script** (`tools/gen-sounds`) → WAVs embedded in the binary.
  License-clean (CC0/ours), tweakable.
- Played in-process from pre-decoded buffers for <20 ms latency. Volume + mute in config/bar.

## Bar (Noctalia plugin `lightwispr`)

- **Widget:** mic glyph. Idle = muted color · recording = accent + live level bars · processing = subtle pulse ·
  delivered = brief check / "copied". Short mode tag optional. Left click = start/stop, right click = cycle mode.
- **Panel (click):**
  - Enhancement toggle
  - Mode selector
  - Last 10 transcriptions (click = copy, relative time)
  - Edit modes (name + prompt)
  - Sound toggle + volume
- **Design:** uses Noctalia theme tokens (looks native with any wallpaper/theme), generous spacing, one accent color,
  150–200 ms eased transitions, no clutter.
- Talks to the daemon only via the CLI/socket → other bars (Waybar etc.) can be added later by the community.

## Hotkeys (Hyprland)

- **Default: hold Ctrl+Super** (modifier-only), release to transcribe.
  Caveat: Super+Ctrl+<key> binds exist (workspace switching etc.). Recording starts silently on press; the start sound and
  the "recording" state only kick in after ~250 ms of holding with no other key. A chord (Super+Ctrl+Left …) or a shorter tap
  is discarded silently. Needs a way to see "another key was pressed" (Hyprland submap or keyboard events); solve in milestone 3.
- Toggle variant for long dictation.
- **Esc cancels** while recording: bound dynamically only during recording, so Esc stays untouched otherwise.

## Daemon / CLI

- `lightwispr daemon` (systemd user service), Unix socket in `$XDG_RUNTIME_DIR/lightwispr.sock`.
- `lightwispr start|stop|toggle|cancel|status --json|mode <name>|enhance on|off|history --json`.
- Live state for the bar: `$XDG_RUNTIME_DIR/lightwispr/state.json` (+ socket events).

## Files

| What | Where |
|---|---|
| Config + modes | `~/.config/lightwispr/config.toml` |
| History (max 10) | `~/.local/state/lightwispr/history.json` |
| Models (downloaded on first run, with checksum) | `~/.local/share/lightwispr/models/` |
| Runtime state/socket | `$XDG_RUNTIME_DIR/lightwispr*` |

## Stack (Rust)

- Audio in/out: `cpal` (or `pipewire` crate); resampling with `rubato`.
- STT: `transcribe-rs` (onnx feature, CPU), as used by Handy.
- LLM: `llama-cpp-2` (CPU).
- Wayland: `wayland-client` + `wayland-protocols-misc` (input-method-v2), `wl-clipboard-rs`.
- IPC: tokio + Unix socket, `serde_json`.
- Toolchain via mise (`mise use rust` in the repo); `target/` gets `nosnap`.

## Milestones

1. **Prototype:** record → Parakeet → print (CLI). Measure RAM/latency. ✅
2. **Delivery:** input-method commit + clipboard fallback. Test in GTK, Chrome, Brave, kitty, Warp.
3. **Daemon + hotkeys + sounds.**
4. **Noctalia plugin** (widget + panel).
5. **Enhancement + modes.**
6. **Open-source release:** README, MIT license, model attribution (Parakeet CC-BY-4.0), AUR package.

## Open questions

- Default enhancement model size (0.6B vs 1.7B): decide after measuring quality on German.
- GPU: **decided 2026-10-07: CPU only, no GPU backend** (fast enough, keeps it light).

## Prototype results (2026-10-07, i9-9900K, CPU only, int8)

| | |
|---|---|
| Model load | 0.85 s |
| Speed | ~18x real-time (11 s audio → 0.59 s, 29 s → 1.6 s) |
| RAM | ~1.0 GB loaded, ~1.1–1.2 GB peak while transcribing |
| Binary | 31 MB (ONNX Runtime linked in) |

Model files: `istupakov/parakeet-tdt-0.6b-v3-onnx` @ `8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce`, sha256:
encoder `6139d2fa…aff09`, decoder_joint `eea7483e…67a70`, nemo128 `a9fde148…19e9f`, vocab `d5854467…3c35d`.
