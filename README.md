# lightwispr

Minimal, local voice dictation for Linux. Hold a key, speak, release: the text lands where your cursor is.

A lightweight, open-source take on Wispr Flow for Wayland/Hyprland. Everything runs on your machine. No account, no cloud, no telemetry.

- **Fast and accurate:** NVIDIA Parakeet TDT 0.6B v3 (25 European languages, detected automatically, with punctuation and casing). About 18× real time on a desktop CPU, so a 10 s sentence takes roughly 0.5 s.
- **Types into the focused field:** acts as a Wayland input method, so text is committed directly without fake keystrokes, and umlauts and emoji work. Apps without input-method support get a paste. With no text field focused, the text goes to the clipboard.
- **Never types into password fields:** text meant for a password or PIN field goes to the clipboard instead.
- **Optional polishing:** a small local LLM fixes grammar, spelling and misheard words and adapts the tone. Pick a style in the bar: *Casual*, *Formal*, *Vibe coding* (prompts for coding agents), or write your own. It sees the text around your cursor (where the app reports it), so the dictation fits in.
- **Two speech engines:** Parakeet (default) or OpenAI Whisper large-v3-turbo (99 languages, can be pinned to one). Only the selected one is loaded. Whisper wants a GPU: an 11 s sentence takes 0.6 s with Vulkan on an RTX 2080 Ti, but about 20 s on a desktop CPU.
- **Small:** a Rust daemon plus a CLI, and a separate worker process for the LLM (started only while a style is selected, so turning it off frees all of its memory). About 0.9 GB RAM with Parakeet loaded and about 10 MB when switched off. No CPU while idle. The mic is only open while you hold the key.
- **Calm UI:** synthesized ceramic-keyboard sounds and an optional [Noctalia](https://github.com/noctalia-dev) bar widget with a live level meter, the last 10 transcriptions and settings.

## Requirements

- Hyprland (hotkey and paste) on Wayland, PipeWire (or ALSA)
- `wl-clipboard`
- To build: Rust (`cargo`), `cmake`, a C/C++ compiler and `clang` (for whisper.cpp and llama.cpp)
- ~700 MB disk for Parakeet; Whisper (~575 MB) and the polishing model (1–3 GB) only if you use them
- Optional GPU (Whisper and the LLM via Vulkan): `vulkan-headers`, `spirv-headers` and `glslc` (shaderc) to build

## Install

```sh
git clone https://github.com/jan-stecher/lightwispr
cd lightwispr
./install.sh
```

This builds `lightwispr` and `lightwispr-llm` into `~/.local/bin`, downloads Parakeet (pinned revision, SHA-256 verified) and starts the `lightwispr.service` user service. The daemon registers its hotkey in Hyprland on its own, so you don't need to edit your Hyprland config. With a GPU: `LIGHTWISPR_VULKAN=1 ./install.sh`.

## Usage

| | |
|---|---|
| **Hold Right Ctrl** | record while held, transcribe on release |
| Tap (< 250 ms) | ignored |
| Esc / any other key while recording | cancel |

Other presets: `Ctrl + Super`, `Super + Alt`. Pick one in the bar panel or run `lightwispr hotkey ctrl_super`.

```
lightwispr status | history | toggle | cancel
lightwispr power on|off          # off unloads the models and frees their RAM
lightwispr hotkey right_ctrl|ctrl_super|super_alt
lightwispr mode off|casual|formal|coding|<yours>
lightwispr stt parakeet|whisper  # switch the speech engine
lightwispr context on|off        # use the focused field and window as context
lightwispr gpu on|off            # Whisper and the LLM on the GPU (Vulkan builds)
lightwispr download-model parakeet|whisper|enhance|<model id>
lightwispr sound on|off, volume 0.35
lightwispr sounds                # preview the UI sounds
```

## Polishing (optional)

Download the model once (`lightwispr download-model enhance`, or the button in the bar) and pick a style. After each dictation the transcript goes through the model with that style's prompt; the result is typed instead.

- **Styles** are TOML files: a prompt plus a few examples. The built-in ones are in [`prompts/`](prompts). `lightwispr mode-file formal` copies one to `~/.config/lightwispr/modes/formal.toml` for editing (or use *Edit prompt* in the bar); any other file there adds a style. Changes apply to the next dictation. `lightwispr prompt formal` prints exactly what the model gets.
- **Vocabulary:** `~/.config/lightwispr/vocabulary.txt`, one name or term per line, spelled as you want it. `wrong -> right` lines fix recurring mishearings right after transcription, even with polishing off. Whisper also gets the terms as a hint.
- **Context:** the text around the cursor (GTK, Qt and Firefox report the paragraph, Chrome/Brave/Electron about 100 characters), the app and window title, and, in apps that report nothing (terminals, XWayland), what you dictated into that window in the last minutes. Context lives in memory only.
- **Safety net:** if the model answers instead of cleaning up, comes back empty, much longer or shorter, or takes too long (8 s), the plain transcript is typed. The history keeps both versions (*Copy the original* in the bar).
- **Models** (`[enhance] model` in config.toml): Gemma 4 E4B on the GPU (~0.3 s per dictation, 25/25 in the eval set, ~4 GB VRAM) and Gemma 4 E2B on the CPU (~1.5 s, 18/25, ~4 GB RAM); `qwen3.5-2b` is a lighter CPU option (1.3 GB, 15/25). Try another with `lightwispr eval --model <id>` (runs [`eval/dictations.toml`](eval/dictations.toml)) or one sentence with `lightwispr enhance --mode coding "…"`.

### Browsers and other apps

- **Chrome, Brave, Chromium, Electron:** enable the Wayland IME with `--enable-wayland-ime --wayland-text-input-version=3` (e.g. in `~/.config/chrome-flags.conf`). Without it, lightwispr falls back to pasting.
- **GTK, Qt, Firefox, kitty, foot …:** work out of the box.
- **Warp:** recent builds on Wayland take input-method text directly; older ones get it pasted.
- **XWayland apps:** get the text pasted (Ctrl+V, or Ctrl+Shift+V in terminals). Your previous clipboard text is restored afterwards.
- **Long dictations** are typed in several parts (the protocol limits one insert to 4000 bytes).

## Noctalia bar widget

The plugin lives in [`noctalia/`](noctalia). Point a local Noctalia plugin source at a folder that contains it, then enable it:

```sh
mkdir -p ~/.local/share/noctalia-local-plugins
ln -s "$PWD/noctalia" ~/.local/share/noctalia-local-plugins/lightwispr
# add a [[plugin]] row for "lightwispr/lightwispr" to that folder's catalog.toml (see noctalia/plugin.toml)
noctalia msg plugins source add local path ~/.local/share/noctalia-local-plugins
noctalia msg plugins enable lightwispr/lightwispr
```

Then add `lightwispr/lightwispr:mic` to a bar section. **Left click** opens the panel (status, record, history, speech engine, style, context, shortcut, sounds, on/off), **right click** starts or stops a dictation, and **scrolling** switches the style.

## Files

| | |
|---|---|
| Settings | `~/.config/lightwispr/config.toml` |
| Your styles, vocabulary | `~/.config/lightwispr/modes/*.toml`, `~/.config/lightwispr/vocabulary.txt` |
| Last 10 transcriptions (the only stored data) | `~/.local/state/lightwispr/history.json` |
| Models | `~/.local/share/lightwispr/models/` |
| Live state, socket | `$XDG_RUNTIME_DIR/lightwispr/` |

Audio is only kept in memory and never written to disk.

## How it works

```
hotkey ─▶ mic (PipeWire, 16 kHz) ─▶ Parakeet (ONNX, CPU) | Whisper (whisper.cpp, CPU/Vulkan)
       ─▶ vocabulary ─▶ [polish: lightwispr-llm (llama.cpp) + style prompt + field context]
       ─▶ input method commit │ paste / clipboard fallback
```

While you record, the daemon puts Hyprland into a small `lightwispr` submap: releasing the key finishes the take, and any other key cancels it. Those binds leave the submap on their own, so the keyboard can never get stuck. More background is in [docs/PLAN.md](docs/PLAN.md) and [docs/SPEC-enhancement.md](docs/SPEC-enhancement.md).

## Credits

- Speech model: [NVIDIA Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3), licensed **CC-BY-4.0**; ONNX export by [istupakov](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx)
- Speech model: [OpenAI Whisper large-v3-turbo](https://huggingface.co/openai/whisper-large-v3-turbo), licensed **MIT**; GGML conversion by [ggerganov](https://huggingface.co/ggerganov/whisper.cpp)
- Polishing models: [Google Gemma 4](https://huggingface.co/google/gemma-4-E4B-it) E2B/E4B (Apache-2.0), GGUF by [unsloth](https://huggingface.co/unsloth); others in the registry: Qwen3.5 (Apache-2.0), LFM2.5 (LFM Open License), Ministral 3 (Apache-2.0)
- Inference: [transcribe-rs](https://github.com/cjpais/transcribe-rs) (from [Handy](https://github.com/cjpais/Handy)), ONNX Runtime, [whisper.cpp](https://github.com/ggml-org/whisper.cpp), [llama.cpp](https://github.com/ggml-org/llama.cpp) via [llama-cpp-2](https://github.com/utilityai/llama-cpp-rs)

## License

[MIT](LICENSE)
