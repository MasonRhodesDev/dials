# hyprstate-gui

Slint **Displays** configurator for [hyprstate](https://github.com/MasonRhodesDev/hyprstate).
Edits [`monitor-profiles`](https://github.com/MasonRhodesDev/monitor-profiles)
TOML under `~/.config/hypr/profiles` by default (and optional shared
`/etc/monitor-profiles` for greeter). Save relies on hyprstate’s hot-reload —
there is no separate Apply.

UX: see `monitor-profiles` planning docs (Displays visual review board).

## Run

```sh
cargo run --release
```

Requires `hyprctl` on `PATH` (Hyprland session).

## MVP

- Launch into **current desk** (active match, or seed from live into user dir)
- Profiles list for historical configurations
- Promote user profile to shared when `/etc/monitor-profiles` is writable
- Canvas-first editor (drag, place-beside)
- Inspector: mode, scale, rotate, position, enabled
- Save → wait for session geometry
