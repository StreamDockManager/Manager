Name:           stream-dock-manager
Version:        0.1.0
Release:        0.1%{?dist}
Summary:        Independent modular Stream Dock device manager
License:        GPL-3.0-or-later AND MIT AND Apache-2.0
Source0:        %{name}-%{version}.tar.gz
ExclusiveArch:  x86_64
BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  nodejs >= 24
BuildRequires:  npm >= 11.19.0
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  pkgconfig(webkit2gtk-4.1)
BuildRequires:  pkgconfig(gtk+-3.0)
BuildRequires:  pkgconfig(appindicator3-0.1)
BuildRequires:  pkgconfig(librsvg-2.0)
BuildRequires:  pkgconfig(libudev)
BuildRequires:  pkgconfig(dbus-1)
BuildRequires:  libxdo-devel
Requires:       webkit2gtk4.1
Requires:       gtk3
Requires:       libappindicator-gtk3
Requires:       systemd-udev
Requires:       pulseaudio-utils
Requires:       playerctl
Requires:       ffmpeg

%description
Local profiles and modular hardware support for Stream Dock devices.
Experimental preview; physical qualification is recorded separately.

%prep
%autosetup

%build
# Source archives must include the offline npm cache, cargo vendor config,
# Node 24 is a frontend build dependency; plugin interpreters are downloaded on demand.
npm ci --offline --include=dev --cache .npm-cache --no-audit --no-fund
npm run build
cargo build --offline --locked --release -p stream-dock-manager

%check
cargo test --offline --locked --workspace --exclude stream-dock-manager --exclude stream-dock-manager-cli
cargo run --offline --locked -p xtask -- hardware check

%install
install -Dpm0755 target/release/stream-dock-manager %{buildroot}%{_bindir}/stream-dock-manager
install -Dpm0644 packaging/linux/60-stream-dock-manager.rules %{buildroot}%{_udevrulesdir}/60-stream-dock-manager.rules
install -Dpm0644 packaging/linux/app.streamdockmanager.manager.desktop %{buildroot}%{_datadir}/applications/app.streamdockmanager.manager.desktop
install -Dpm0644 packaging/linux/app.streamdockmanager.manager.metainfo.xml %{buildroot}%{_datadir}/metainfo/app.streamdockmanager.manager.metainfo.xml
install -Dpm0644 src-tauri/icons/icon.png %{buildroot}%{_datadir}/icons/hicolor/128x128/apps/app.streamdockmanager.manager.png

%post
udevadm control --reload-rules >/dev/null 2>&1 || :

%postun
udevadm control --reload-rules >/dev/null 2>&1 || :

%files
%license LICENSE THIRD_PARTY_NOTICES.md
%doc README.md packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md
%{_bindir}/stream-dock-manager
%{_udevrulesdir}/60-stream-dock-manager.rules
%{_datadir}/applications/app.streamdockmanager.manager.desktop
%{_datadir}/metainfo/app.streamdockmanager.manager.metainfo.xml
%{_datadir}/icons/hicolor/128x128/apps/app.streamdockmanager.manager.png

%changelog
* Wed Oct 21 2026 Stream Dock Manager contributors - 0.1.0-0.1
- Initial experimental package recipe.
