# Rufus Linux CLI

This experimental Rust CLI writes raw disk images and hybrid ISOs to removable
USB disks and prepares Windows installer images for UEFI. It requires Linux 5.15 or later on x86-64 or AArch64. The build
requires Rust 1.85 or later and a system linker. The executable has no Cargo
dependencies.

From the repository root:

```sh
cargo build --manifest-path linux/Cargo.toml --release --locked
linux/target/release/rufus-linux list
```

Check the device name and capacity against your USB drive. Unmount all its
partitions, then run the following command with your image and whole-disk device
path. **The write overwrites the existing filesystem and data on the selected
disk.**

```sh
sudo linux/target/release/rufus-linux write /path/to/hybrid.iso /dev/sdX --erase
```

The writer checks the device type and capacity and requires an exclusive Linux
block-device claim. It flushes writes and compares the written range byte for
byte through the kernel. This comparison can be served from the kernel cache
and does not test bootability. The raw mode leaves bytes beyond the end of the
image untouched. Reconnect the USB drive after a successful write so Linux
rediscovers the partition table.

## Windows installer USB (UEFI)

Install [7-Zip](https://www.7-zip.org/) (`7z` or `7zz`), mtools, and wimlib
(`wimlib-imagex`; needed for oversized `install.wim`). On Debian 13, these
are the `7zip`, `mtools`, and `wimtools` packages.

Run the first command as your normal user with an official x64 or ARM64
Windows ISO. Use a working directory with enough free space for both the
extracted files and disk image, plus temporary parts when splitting a WIM.
The output path must be new.

```sh
linux/target/release/rufus-linux windows-image Windows.iso windows-usb.img
linux/target/release/rufus-linux list
sudo linux/target/release/rufus-linux write windows-usb.img /dev/sdX --erase
```

Use a removable drive with 512-byte logical sectors. The first command builds
an MBR image with a FAT32 primary partition, copies the installer files and
splits an oversized `sources/install.wim` into `install*.swm` files so Windows
Setup can read it from FAT32. It accepts ISO media containing `install.wim`,
`install.esd`, or a pre-split `install.swm`. Any other individual file above
the FAT32 limit stops the build. Legacy BIOS and Windows To Go are unsupported.
The target computer must support the ISO's architecture and bootloader.

[Microsoft's Windows installation USB instructions](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/install-windows-from-a-usb-flash-drive)

## Porting notes

The upstream Rufus maintainer identified the Win32 interface, formatting APIs,
Windows To Go boot configuration, VHD and ReFS as major porting costs.
This CLI uses Linux block-device operations and builds Windows installer media
using the installer ISO's existing bootloader and boot configuration. Windows
To Go would additionally need to apply the Windows image and configure its
own boot files and BCD store.

[Upstream FAQ](https://github.com/pbatard/rufus/wiki/FAQ#do-you-plan-to-port-rufus-to-linuxmac-ossome-other-os) ·
[Maintainer's porting discussion](https://github.com/pbatard/rufus/issues/679#issuecomment-172692258)

## Tests

```sh
cargo test --manifest-path linux/Cargo.toml --locked
```

With wimlib installed, run the split-image integration test:

```sh
cargo test --manifest-path linux/Cargo.toml --locked -- --ignored
```
