# RPM spec for hyprstate-gui. Built in COPR from a local SRPM produced by
# packaging/build-srpm.sh (source tarball from the git tag + vendored cargo
# deps as Source1 — no rust-*-devel packages needed).
%bcond_without check

Name:           hyprstate-gui
Version:        0.3.3
Release:        1%{?dist}
Summary:        Displays and power configurator for hyprstate
License:        MIT
URL:            https://github.com/MasonRhodesDev/hyprstate-gui
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
Requires:       mesa-libGL
Requires:       fontconfig
Recommends:     hyprstate

%description
Slint configurator for hyprstate: edit monitor-profile layouts, the power
policy map, and inspect how lid/power/profile state is derived. A desktop
entry lands in the Settings category. Configuration lives in ~/.config/hypr
and is not part of this package.

%prep
# -a1 unpacks the vendor tarball (vendor/ at its root) into the source dir.
%autosetup -p1 -a1
%cargo_prep -v vendor
# %%cargo_prep only redirects crates.io. Git pins are vendored in Source1
# too; map them so the RPM build stays offline.
cat >> .cargo/config.toml << 'EOF'

[source."git+https://github.com/MasonRhodesDev/monitor-profiles?rev=aef5f0e"]
git = "https://github.com/MasonRhodesDev/monitor-profiles"
rev = "aef5f0e"
replace-with = "vendored-sources"

[source."git+https://github.com/MasonRhodesDev/hyprstate?rev=38172c2797ac905dfcd04bf5e58b485644d10a2c"]
git = "https://github.com/MasonRhodesDev/hyprstate"
rev = "38172c2797ac905dfcd04bf5e58b485644d10a2c"
replace-with = "vendored-sources"

[source."git+https://github.com/MasonRhodesDev/slint-kit?rev=ccd7397c3da83ff835d6295d6ec3841fc32c8bac"]
git = "https://github.com/MasonRhodesDev/slint-kit"
rev = "ccd7397c3da83ff835d6295d6ec3841fc32c8bac"
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
install -Dpm0755 target/rpm/hyprstate-gui %{buildroot}%{_bindir}/hyprstate-gui
desktop-file-install --dir=%{buildroot}%{_datadir}/applications dist/hyprstate-gui.desktop
desktop-file-validate %{buildroot}%{_datadir}/applications/hyprstate-gui.desktop

%if %{with check}
%check
%cargo_test
%endif

%files
%license LICENSE LICENSE.dependencies
%doc README.md
%{_bindir}/hyprstate-gui
%{_datadir}/applications/hyprstate-gui.desktop

%changelog
* Thu Aug 20 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.3-1
- Use event-driven telemetry and bounded one-shot save convergence checks.
- Pin Slint and slint-build to 1.17.1.

* Sun Aug 16 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.2-1
- Republish so [mason] picks up ARCH_REPO_TOKEN.

* Sun Aug 16 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.1-1
- Pin slint-kit to lmtt 0.2.2 and vendor appearance-profiles for COPR.

* Fri Aug 14 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.0-1
- Initial packaged release: binary plus Settings desktop entry
