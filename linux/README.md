# Rufus Linux CLI

This experimental Rust CLI writes raw disk images and hybrid ISOs to removable
USB disks. It requires Linux 5.15 or later on x86-64 or AArch64. The build
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

Use an image whose publisher supports raw USB writing. Filesystem creation and
ISO extraction, standard Windows installer media, Windows To Go, persistence,
compressed images, and a graphical interface are outside the scope of this
initial CLI.

## Why start with raw writing?

The upstream Rufus maintainer identified the Win32 interface, formatting APIs,
Windows To Go boot configuration, VHD and ReFS as major porting costs.
This CLI uses sysfs and Linux block-device operations for device discovery,
claiming and capacity, without relying on filesystem formatting or Windows APIs.
A Windows ISO mode would need a separate path to create a partition table and
filesystem, extract the installer, and handle WIM files that exceed the FAT32
file size limit. Windows To Go would additionally need to apply the Windows
image and configure the boot files and BCD store.

[Upstream FAQ](https://github.com/pbatard/rufus/wiki/FAQ#do-you-plan-to-port-rufus-to-linuxmac-ossome-other-os) ·
[Maintainer's porting discussion](https://github.com/pbatard/rufus/issues/679#issuecomment-172692258)

## Tests

```sh
cargo test --manifest-path linux/Cargo.toml --locked
```
