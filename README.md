# Tang

A terminal-based audio plugin host.

[![CI](https://github.com/apanloco/tang/actions/workflows/ci.yml/badge.svg)](https://github.com/apanloco/tang/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Tang turns your terminal into an instrument. Load your favorite audio plugins, plug in a MIDI keyboard (or use your computer keyboard like a tracker), and play — no DAW, no mouse, no plugin GUI in the way.

## Highlights

- Hosts **LV2**, **CLAP**, and **VST3** plugins — both instruments and effects
- Play with a MIDI keyboard or your computer keyboard
- Per-instrument effect chains, modulation, and pattern loops
- Cross-platform: Linux, macOS, Windows

## Plugin support

| Format | Linux | macOS | Windows |
|--------|:-----:|:-----:|:-------:|
| LV2    | ✅    | ✅    | —       |
| CLAP   | ✅    | ✅    | ✅      |
| VST3   | ✅    | ✅    | ✅      |

CLAP and VST3 work everywhere. LV2 needs the `lilv` system library: `apt install liblilv-dev` on Linux, `brew install lilv` on macOS. Not supported on Windows.

## Quickstart

```sh
# Build it
cargo build --release

# TUI mode — full editor: load plugins, build chains, tweak, save
./target/release/tang examples/basic.toml

# Play mode — read-only: no editor, just play via MIDI or computer keyboard
./target/release/tang play examples/basic.toml
```

`examples/basic.toml` loads the built-in sine instrument. Try `examples/split.toml` for a multi-instrument session, and the sounds that show what the built-in plugins do: `supersaw.toml`, `wind.toml`, `bell.toml`, `arp.toml` (hold a key), `pwm.toml`, `epiano.toml`, `sync-lead.toml`, `sub-bass.toml`, `fold.toml` and `echo.toml` (`tang serve examples/bell.toml` for the browser). Or run `tang` with no arguments to start with a new default session (saved under `~/.config/tang/sessions/` on the first `Ctrl+S`).

## Status

Early but usable. The TUI's Session and Help tabs are functional — load plugins, build effect chains, tweak parameters, record patterns, save sessions. The Piano and Oscilloscope tabs are still placeholders. See [CLAUDE.md](CLAUDE.md) for the usage and the index of the design documents in [`docs/reference/`](docs/reference/), which have the full design.

## License

MIT — see [LICENSE](LICENSE).
