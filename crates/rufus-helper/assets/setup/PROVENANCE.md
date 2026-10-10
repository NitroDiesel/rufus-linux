# Windows 11 Setup wrapper

`setup_x64.exe` and `setup_arm64.exe` are upstream Rufus's signed
`res/setup/` binaries, copied unchanged. `setup.c` is their source, also
unchanged (GPLv3 or later, Copyright © 2024 Pete Batard).

When the "Remove requirement for 4GB+ RAM, Secure Boot and TPM 2.0" option is
chosen for Windows 11 24H2 or later (build 26000+), Rufus renames the media's
`setup.exe` to `setup.dll` and puts this wrapper in its place. When it is run
for an in-place upgrade, the wrapper asks for elevation, sets the registry
values that skip the hardware checks, and starts the original setup. Booting
from the media does not use it.

The executables' SHA-256 hashes are the ones upstream's
`res/setup/readme.txt` links to VirusTotal reports for:

```
11df838dc69378187e1e1aaf32d34384157642d07096c6e49c1d0e7375634544  setup_x64.exe
14bd07f559513890a0f6565df3927392b4fe6b8e6fc3f5e832e9d69c8b7bb7eb  setup_arm64.exe
c06248ff1b3cc3d5b7a22e4ef1113361305786b0afd19968bef9233901a01e94  setup.c
```
