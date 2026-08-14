# RPM spec for hyprstate-gui. Built in COPR from a local SRPM produced by
# packaging/build-srpm.sh (source tarball from the git tag + vendored cargo
# deps as Source1 — no rust-*-devel packages needed).
%bcond_without check

Name:           hyprstate-gui
Version:        0.3.0
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
Requires:       libxkbcommon
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

[source."git+https://github.com/MasonRhodesDev/slint-kit?rev=5e3a3a9c7e41585d681149b80070128f0987f0c8"]
git = "https://github.com/MasonRhodesDev/slint-kit"
rev = "5e3a3a9c7e41585d681149b80070128f0987f0c8"
replace-with = "vendored-sources"
EOF

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies

%install
%cargo_install
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
* Fri Aug 14 2026 Mason Rhodes <mrhodesdev@gmail.com> - 0.3.0-1
- Initial packaged release: binary plus Settings desktop entry
