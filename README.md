# Cartridge

A small desktop app that installs the BIOS files **you already own** onto a
[Batocera](https://batocera.org) device over your home network. It works on Linux, macOS and Windows.

## How it works (step by step)

1. **Connect** - enter your Batocera's IP address (Batocera: Main menu > Network Settings). The defaults are
   user `root` and password `linux`.
2. **Check** - Cartridge runs Batocera's own `batocera-systems` command over SSH. That lists exactly which
   BIOS files your installed emulators are missing (`MISSING`) or have in the wrong version (`UNTESTED`),
   along with the expected MD5 checksums.
3. **Pick a folder** - choose a folder on your computer with your BIOS dumps (subfolders are searched).
   Cartridge calculates MD5s and matches your files to what Batocera needs. A file with the right MD5 is used
   even if it is named differently, and it is uploaded under the name Batocera expects.
4. **Install** - the matched files are uploaded by SFTP into `/userdata/bios/` (the same place as the
   `\\BATOCERA\share\bios` network share), including subfolders like `bios/np2kai/`. Cartridge then re-checks
   and tells you what is still missing.

## Where Batocera keeps BIOS files

All BIOS files go in `/userdata/bios/`. You can also see them in the share at `\\BATOCERA\share\bios`
(Windows) or `smb://batocera/share/bios` (macOS/Linux). The up-to-date list for your version is available
in EmulationStation under *System Settings > Missing BIOS*, or by running `batocera-systems` over SSH.

## Download

Prebuilt apps for Linux, macOS and Windows are built by GitHub Actions: open the **Actions** tab, pick the
latest run and download the artifact for your system. (Tagging a version like `v0.1.0` also publishes a release.)

## Build it yourself

1. Install Rust from <https://rustup.rs>.
2. Linux only: `sudo apt install libgtk-3-dev` (or your distro's equivalent) plus a C compiler and `perl`/`make`.
3. Run:

```
cargo run --release
```

## Notes

- SSH must be enabled on Batocera (it is by default).
- Your password is only sent to the Batocera device you enter.
- Wrong-version files are never overwritten unless a file with the correct MD5 is found.

## Credits

Created by AJ-64x with code written by [Claude](https://claude.ai) (Anthropic).
