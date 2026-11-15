<p align="center"><img src="docs/banner.png" alt="Cartridge"></p>

A small desktop app that installs the BIOS files **you already own** onto a
[Batocera](https://batocera.org) device over your home network. It runs on **Windows and Linux**.

## BIOS Files downloadable [Here](https://abdess.github.io/retrobios/get/batocera/)

<p align="center"><img src="docs/2-pick.png" alt="Cartridge choosing a BIOS folder" width="560"></p>

## How it works

### 1. Connect
When Cartridge starts it **looks for your Batocera automatically**. It checks your local network for devices
that share files over SMB (Windows file sharing) and accept SSH, and asks each for its network name (Batocera's
default is `BATOCERA`). If it finds one, it asks **"Is this your Batocera?"**. Click **Yes, use it** and the IP
address is filled in for you. Nothing is used until you confirm, and nothing leaves your local network.

<p align="center"><img src="docs/0-found.png" alt="Cartridge found a Batocera and asks if it is yours" width="480"></p>

Then enter the password (the defaults are user `root` and password `linux`). If nothing is found, click **Scan
again**, or type the IP address yourself (Batocera: *Main menu > Network Settings*).

> Detection scans your computer's own `/24` network (for example `192.168.1.x`). It can't see devices on a
> different network, behind a guest Wi-Fi, or when Batocera is turned off. Typing the IP always works.

<p align="center"><img src="docs/1-connect.png" alt="Connect step" width="480"></p>

Cartridge runs Batocera's own `batocera-systems` command over SSH. It lists exactly which BIOS files your
installed emulators are missing (`MISSING`) or have in the wrong version (`UNTESTED`), with the expected MD5
checksums.

### 2. Choose your files
Pick (or drag in) a folder with your BIOS dumps. Subfolders are searched. Cartridge calculates MD5s and matches
your files to what Batocera needs. A file with the right MD5 is used even if it is named differently, and it is
uploaded under the name Batocera expects. You can also save a list of everything still needed, so you know what
to dump.

### 3. Install
Matched files are uploaded by SFTP into `/userdata/bios/`, including subfolders like `bios/np2kai/`.
Cartridge then re-checks with Batocera and tells you what is still missing.

<p align="center">
  <img src="docs/3-install.png" alt="Install step" width="380">
  <img src="docs/4-done.png" alt="Finished" width="380">
</p>

*The screenshots use demo data with placeholder file names.*

## Where Batocera keeps BIOS files

All BIOS files go in `/userdata/bios/`. You can also reach that folder through the network share at
`\\BATOCERA\share\bios` (Windows) or `smb://batocera/share/bios` (Linux). The current list for your version
is in EmulationStation under *System Settings > Missing BIOS*, or by running `batocera-systems` over SSH.

## Download and install

Get the latest version from the [**Releases**](../../releases/latest) page and pick the file for your computer:

| Your computer | Windows | Linux (with app-menu installer) | Linux (raw program) |
|---|---|---|---|
| **Intel / AMD** (most PCs, called x86_64 or "x64") | `cartridge-windows-x86_64.exe` | `cartridge-linux-x86_64.zip` | `cartridge-linux-x86_64` |
| **ARM** (aarch64 / arm64, e.g. Snapdragon laptops, Raspberry Pi) | `cartridge-windows-aarch64.exe` | `cartridge-linux-aarch64.zip` | `cartridge-linux-aarch64` |

Not sure? On Windows check *Settings > System > About > System type*. On Linux run `uname -m`
(`x86_64` = Intel/AMD, `aarch64` = ARM).

**Windows:** just download the `.exe` and double-click it. No install or unzip needed. (Windows may show a
SmartScreen warning because the app isn't code-signed: click *More info > Run anyway*.)

**Linux:** if you want it in your app menu, unzip the zip and run the installer:

```
./install-linux.sh
```

That puts Cartridge in your app menu with its icon. Search for "Cartridge" (log out and back in once if it
doesn't appear right away). To remove it: `./install-linux.sh --uninstall`. You can also just run `./cartridge`
without installing. (If your system says the script isn't executable, run `sh install-linux.sh` instead.)

**Linux, raw program:** download `cartridge-linux-<your type>`, then run `chmod +x cartridge-linux-x86_64`
(or `-aarch64`) and start it with `./cartridge-linux-x86_64`. Downloads don't keep the executable permission, so
the `chmod` step is needed.

Releases are built by GitHub Actions. Pushing a tag like `v0.2.1` builds and publishes a new one.

## Build it yourself

1. Install Rust from <https://rustup.rs>.
2. Linux only: `sudo apt install libgtk-3-dev` (or your distro's equivalent), plus a C compiler, `perl` and `make`.
3. Run `cargo run --release`.

## Notes

- SSH must be enabled on Batocera (it is by default).
- Your password is only sent to the Batocera device you enter.
- Files with the wrong checksum are never installed.
- Preview the interface without a device: set `CARTRIDGE_DEMO=pick` (or `connect`, `install`, `done`).

## Credits

Created by AJ-64x with code written by [Claude](https://claude.ai) (Anthropic).
