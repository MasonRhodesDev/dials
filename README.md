# hyprstate-gui

Slint **Displays** configurator for [hyprstate](https://github.com/MasonRhodesDev/hyprstate).
Edits shared [`monitor-profiles`](https://github.com/MasonRhodesDev/monitor-profiles)
TOML under `/etc/monitor-profiles` (and user overrides). Save relies on hyprstate’s
hot-reload — there is no separate Apply.

UX: see `monitor-profiles` planning docs (Displays visual review board).

## Run

```sh
cargo run --release
```

Requires `hyprctl` on `PATH` (Hyprland session).

## MVP

- Profiles list (system/user, matched/active)
- Canvas-first editor (drag, center-to-neighbor)
- Inspector: mode, scale, rotate, position, enabled
- Capture current layout
- Save → wait for session geometry
