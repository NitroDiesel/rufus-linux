# AppImage notes

AppImage packaging is optional. Prefer distro packages so polkit, partition
tools, and formatters remain system-managed.

If you ship an AppImage:

1. Build `cargo build --release --locked`.
2. Stage `rufus-linux` under `AppDir/usr/bin/`.
3. Do **not** bundle the privileged helper as setuid; document that live writes
   require a system-installed helper and polkit policy. The desktop remains
   read-only when those system components are absent.
4. Bundle only redistributable assets with licenses under `assets/licenses/`.
