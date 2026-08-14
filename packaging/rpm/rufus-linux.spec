Name:           rufus-linux
Version:        0.1.2
Release:        1%{?dist}
Summary:        Format and create bootable USB drives
License:        GPL-3.0-or-later
URL:            https://github.com/NitroDiesel/rufus-linux
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo rust pkgconf fontconfig-devel freetype-devel
BuildRequires:  libxkbcommon-devel wayland-devel libX11-devel libxcb-devel
BuildRequires:  systemd-rpm-macros
Requires:       polkit util-linux parted dosfstools exfatprogs ntfsprogs e2fsprogs udftools
Recommends:     libarchive xz bzip2 zstd

%description
Independent Linux port inspired by Rufus. Writes and formats removable
media through a polkit-authorized helper.

%prep
%autosetup

%build
cargo build --release --locked

%install
install -Dm0755 target/release/rufus-linux %{buildroot}%{_bindir}/rufus-linux
install -Dm0755 target/release/rufus-linux-helper %{buildroot}%{_libexecdir}/rufus-linux-helper
install -Dm0644 packaging/desktop/io.github.nitrodiesel.rufus-linux.desktop \
  %{buildroot}%{_datadir}/applications/io.github.nitrodiesel.rufus-linux.desktop
install -Dm0644 packaging/metainfo/io.github.nitrodiesel.rufus-linux.metainfo.xml \
  %{buildroot}%{_metainfodir}/io.github.nitrodiesel.rufus-linux.metainfo.xml
install -Dm0644 packaging/polkit/io.github.nitrodiesel.rufus-linux.policy \
  %{buildroot}%{_datadir}/polkit-1/actions/io.github.nitrodiesel.rufus-linux.policy
install -Dm0644 packaging/tmpfiles/rufus-linux.conf \
  %{buildroot}%{_tmpfilesdir}/rufus-linux.conf
for size in 16 24 32 48 64 128 256 512; do
  install -Dm0644 "assets/icons/rufus-linux-${size}.png" \
    "%{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/io.github.nitrodiesel.rufus-linux.png"
done

%files
%license LICENSE.txt
%license assets/icons/LICENSE.txt
%doc README.md docs/
%{_bindir}/rufus-linux
%{_libexecdir}/rufus-linux-helper
%{_datadir}/applications/io.github.nitrodiesel.rufus-linux.desktop
%{_metainfodir}/io.github.nitrodiesel.rufus-linux.metainfo.xml
%{_datadir}/polkit-1/actions/io.github.nitrodiesel.rufus-linux.policy
%{_tmpfilesdir}/rufus-linux.conf
%{_datadir}/icons/hicolor/*/apps/io.github.nitrodiesel.rufus-linux.png

%changelog
* Sat Aug 15 2026 Rufus Linux contributors - 0.1.2-1
- Add a verified portable AppImage and harden helper readiness checks

* Sun Aug 09 2026 Rufus Linux contributors - 0.1.1-1
- Stable UI alignment, live USB detection, and privileged I/O hardening

* Wed Jul 29 2026 Rufus Linux contributors - 0.1.0-1
- Initial technology-preview package
