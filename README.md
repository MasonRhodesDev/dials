# dials

Desktop settings for Wayland sessions, in [Slint](https://slint.dev) on
[slint-kit](https://github.com/MasonRhodesDev/slint-kit). One window, five
sections:

- **Displays** — canvas-first editor for
  [`monitor-profiles`](https://github.com/MasonRhodesDev/monitor-profiles)
  TOML under `~/.config/hypr/profiles` (and shared `/etc/monitor-profiles`
  for the greeter). Save relies on
  [hyprstate](https://github.com/MasonRhodesDev/hyprstate)'s hot-reload —
  there is no separate Apply.
- **Power** — hyprstate's power policy map, sensors, and overrides.
- **Appearance** — the desktop background, read from the
  [appearance-profiles](https://github.com/MasonRhodesDev/appearance-profiles)
  registry and set through [lmtt](https://github.com/MasonRhodesDev/linux-multi-theme-toggle).
- **Help** — live graph of how hyprstate derives lid/power/display state.
- **More** — every other settings tool installed on the system, launched in
  its own window.

![Canvas editor for the current desk](docs/screenshots/editor.png)

![Inspector for the selected output](docs/screenshots/inspector.png)

![Profiles list](docs/screenshots/profiles.png)

## Appearance

The origin display (the one at logical 0,0) sets the global wallpaper:
every other display inherits it and lmtt derives the palette from it, so
the two never differ. "Different wallpaper per display" (off by default)
gives the other displays their own image; turning it off again removes
those overrides. Images are picked with the XDG portal file chooser.

Reads use the registry directly (`~/.config/appearance-profiles/default.toml`
over the system and packaged layers). Writes go only through `lmtt`:
`lmtt wallpaper set <image> [--output <display>]`, `lmtt wallpaper unset
--output <display>` (lmtt ≥ 0.3.2), then `lmtt switch <current mode>
--no-notify` so the palette, the published greeter/lock snapshot and every
lmtt module follow. Without `lmtt` on `PATH` the page still shows the
current wallpaper but cannot change it. Whether the desktop itself
repaints depends on the wallpaper daemon: on hypr-DE, swaybg reads the
registry when it starts, so it follows once hypr-DE restarts it from an
lmtt reload module; the lock screen and greeter (vigil) follow immediately.

## Listing a tool under More

Dials does not embed other tools and has no plugin API. It reads XDG desktop
entries (`~/.local/share/applications`, then `$XDG_DATA_DIRS`) and lists an
entry when either holds:

- `Categories=` contains `Settings;` (the freedesktop convention — the same
  entry shows up in LXQt's and Xfce's settings managers), or
- `X-Dials-Section=<section>` is set. It also picks the section; without it
  the section (Appearance, Hardware, Network, System, Other) is derived
  from `DesktopSettings` / `HardwareSettings` / `Network` / `System`.

`NoDisplay`, `Hidden`, `OnlyShowIn` / `NotShowIn` (against
`$XDG_CURRENT_DESKTOP`) and a missing `TryExec` all hide an entry. `Exec`
field codes are dropped; tools are launched in their own process group.
`Terminal=true` entries run under `xdg-terminal-exec`, else `$TERMINAL`,
else the first of kitty / foot / alacritty / wezterm / ghostty on `PATH`;
with none of those they are hidden. `dials --entries` prints what More would list.

```ini
[Desktop Entry]
Type=Application
Name=Themes
Exec=kitty lmtt-config
Categories=Settings;DesktopSettings;
X-Dials-Section=Appearance
```

## Run

```sh
cargo run --release
```

Displays and Power need `hyprctl` on `PATH` (Hyprland session) and
hyprstate for apply; Appearance needs `lmtt` to set a wallpaper (and
`hyprctl` to tell displays apart); More works on any Wayland compositor. Packaged
Fedora/Arch installs put the binary in `/usr/bin` and a Settings desktop
entry in `/usr/share/applications`.

## Releasing

Bump `Cargo.toml`, `packaging/dials.spec` (+ `%changelog`), and
`packaging/PKGBUILD` together, then tag `vX.Y.Z`. CI builds the Arch package,
submits COPR, and dispatches [arch-repo](https://github.com/MasonRhodesDev/arch-repo).
