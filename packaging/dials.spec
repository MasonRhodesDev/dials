# RPM spec for dials. Built in COPR from a local SRPM produced by
# packaging/build-srpm.sh (source tarball from the git tag + vendored cargo
# deps as Source1 — no rust-*-devel packages needed).
%bcond_without check

Name:           dials
Version:        0.5.0
Release:        1%{?dist}
Summary:        Desktop settings: displays, power, users, system and a settings hub
License:        MIT
URL:            https://github.com/MasonRhodesDev/dials
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo-rpm-macros >= 24
BuildRequires:  desktop-file-utils
# Slint skia renderer (clang/python/ninja) + winit (xkbcommon/wayland/GL).
BuildRequires:  clang
BuildRequires:  python3
BuildRequires:  ninja-build
BuildRequires:  cmake
BuildRequires:  pkgconf
BuildRequires:  fontconfig-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  wayland-devel
BuildRequires:  mesa-libGL-devel
BuildRequires:  mesa-libEGL-devel
BuildRequires:  libstdc++-devel
# crypt_gensalt_ra / crypt_ra for AccountsService password hashes.
BuildRequires:  libxcrypt-devel
Requires:       mesa-libGL
Requires:       fontconfig
Recommends:     hyprstate
# Wallpaper field in Displays: runs `lmtt wallpaper` / `lmtt switch`.
Recommends:     lmtt
# Users page writes (read-only without it); polkit's pkexec for groups.
Recommends:     accountsservice
Recommends:     polkit
# Fingerprint section of Users; hidden without fprintd and a reader. Suggests,
# not Recommends: most machines have no reader. fprintd-pam is what makes
# logins and sudo accept the enrolled fingers.
Suggests:       fprintd
Suggests:       fprintd-pam

%description
Slint desktop settings application. Displays and Power edit hyprstate's
monitor-profile layouts and power policy map and show how lid/power/profile
state is derived; Displays also shows and sets each monitor's wallpaper
through lmtt and the appearance-profiles registry; Appearance edits lmtt's
settings from its config schema; Users manages local accounts and groups
through AccountsService and polkit; System sets hostname, time zone, NTP,
language and keyboard through systemd; More lists every other
installed settings tool from its XDG desktop entry (Categories=Settings; or
X-Dials-Section=) and launches it. Configuration lives in ~/.config/hypr and
~/.config/appearance-profiles and is not part of this package.

%prep
# -a1 unpacks the vendor tarball (vendor/ at its root) into the source dir.
%autosetup -p1 -a1
%cargo_prep -v vendor
# %%cargo_prep only redirects crates.io. Git pins are vendored in Source1
# too; map them so the RPM build stays offline.
cat >> .cargo/config.toml << 'EOF'

[source."git+https://github.com/MasonRhodesDev/monitor-profiles?rev=64d5d1e"]
git = "https://github.com/MasonRhodesDev/monitor-profiles"
rev = "64d5d1e"
replace-with = "vendored-sources"

[source."git+https://github.com/MasonRhodesDev/hyprstate?rev=746bbd2608f19e7e9ac5113131710f54f6c98f9e"]
git = "https://github.com/MasonRhodesDev/hyprstate"
rev = "746bbd2608f19e7e9ac5113131710f54f6c98f9e"
replace-with = "vendored-sources"

[source."git+https://github.com/MasonRhodesDev/slint-kit?rev=3581f653546ae1e187bc3f2bb20c54019d0a202a"]
git = "https://github.com/MasonRhodesDev/slint-kit"
rev = "3581f653546ae1e187bc3f2bb20c54019d0a202a"
replace-with = "vendored-sources"

[source."git+https://github.com/MasonRhodesDev/linux-multi-theme-toggle?rev=344529cd124c131da40409b152bc1604eebd53d0"]
git = "https://github.com/MasonRhodesDev/linux-multi-theme-toggle"
rev = "344529cd124c131da40409b152bc1604eebd53d0"
replace-with = "vendored-sources"

[source."git+https://github.com/MasonRhodesDev/appearance-profiles.git?rev=75d831a"]
git = "https://github.com/MasonRhodesDev/appearance-profiles.git"
rev = "75d831a"
replace-with = "vendored-sources"
EOF

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies

%install
# %%cargo_install re-resolves without Cargo.lock; git pins then fail offline.
# %%cargo_build already produced the rpm-profile binary.
install -Dpm0755 target/rpm/dials %{buildroot}%{_bindir}/dials
desktop-file-install --dir=%{buildroot}%{_datadir}/applications dist/dials.desktop
desktop-file-validate %{buildroot}%{_datadir}/applications/dials.desktop

%if %{with check}
%check
%cargo_test
%endif

%files
%license LICENSE LICENSE.dependencies
%doc README.md
%{_bindir}/dials
%{_datadir}/applications/dials.desktop

%changelog
* Thu Oct 15 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.5.0-1
- Users page: accounts (create, full name, picture, administrator, disable
  login, password, delete with or without the home directory), group
  membership, and groups you make, through AccountsService and pkexec'd
  shadow tools. Changing your own password as a standard user runs passwd.
- System page: hostname, device name, time zone, NTP, language and keyboard
  layout through hostnamed, timedated and localed; keyboard options survive a
  layout change.
- Every Users and System change is authorized by polkit through the
  session's agent; dials never runs as root. Recommends accountsservice and
  polkit.
- Displays: Save writes to the shared /etc/monitor-profiles whenever it is
  writable, so the login screen sees the change; the editor says whether a
  profile is yours only or shared.
- Wrapped text takes its minimum height from slint-kit.
- tests/qa: hypr-qa scenarios for every Users and System write, run in a VM.

* Wed Oct 14 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.4.7-1
- Displays inspector: the wallpaper field's note and path no longer get the
  "Match / advanced" disclosure drawn over them, and the Choose button stays
  inside the panel. Slint measures a conditional layout child at its
  preferred width, so wrapped text inside one now pins its own height; the
  same pin on the Appearance descriptions, Power sensors note, Help notes
  and the More empty-state line.
- The two entries tests that set and clear TERMINAL no longer race.

* Tue Oct 13 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.4.6-1
- Appearance page: lmtt's settings rendered from its own config schema, with
  option lists from lmtt-core's detectors. Dark/Light switches at once; Apply
  writes ~/.config/lmtt/config.toml without touching comments and re-renders
  with lmtt switch; the palette lmtt derived is shown. Reads the packaged
  schema from lmtt 0.3.3, or the copy lmtt-config writes.
- Wallpaper stub tests no longer race on parallel builders.

* Tue Oct 13 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.4.5-1
- Displays shows each monitor's wallpaper on its tile and sets it from the
  inspector through lmtt: the origin display owns the global wallpaper, any
  other display can override it. Images are picked with the XDG portal.
- Pages are feature-gated in-process modules (displays, power, help,
  wallpaper) that meet only through hooks; More is always present.
- Recommends lmtt (wallpaper selection; `wallpaper unset` needs 0.3.2).

* Fri Aug 28 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.4.4-1
- Profile editor shows newly plugged monitors (seeded from live layout) and marks them unsaved until the profile is saved.

* Sat Aug 22 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.4.3-1
- Terminal=true entries fall back to kitty/foot/alacritty/wezterm/ghostty when xdg-terminal-exec and $TERMINAL are absent.

* Sat Aug 22 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.4.2-1
- COPR: enable network for the skia-bindings binary download (as hyprstate-gui had).

* Sat Aug 22 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.4.1-1
- Republish so the dials COPR project is created with current chroots.

* Sat Aug 22 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.4.0-1
- Rename from hyprstate-gui; add the More page (XDG Settings entries).

* Thu Aug 20 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.3-1
- Use event-driven telemetry and bounded one-shot save convergence checks.
- Pin Slint and slint-build to 1.17.1.

* Sun Aug 16 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.2-1
- Republish so [mason] picks up ARCH_REPO_TOKEN.

* Sun Aug 16 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.1-1
- Pin slint-kit to lmtt 0.2.2 and vendor appearance-profiles for COPR.

* Fri Aug 14 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.0-1
- Initial packaged release: binary plus Settings desktop entry
