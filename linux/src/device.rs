// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::c_int;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

// Linux x86-64 and AArch64 UAPI constants from asm-generic/fcntl.h
// and linux/fs.h. Both ioctl results are written to an unsigned 64-bit value.
const O_EXCL: i32 = 0o200;
pub const O_NONBLOCK: i32 = 0o4000;
const BLKGETSIZE64: u32 = 0x8008_1272;
const BLKGETDISKSEQ: u32 = 0x8008_1280;

unsafe extern "C" {
    fn ioctl(fd: c_int, request: std::ffi::c_ulong, ...) -> c_int;
}

pub fn list() -> io::Result<()> {
    let mut devices = Vec::new();
    for entry in fs::read_dir("/sys/block")? {
        let entry = entry?;
        let path = Path::new("/dev").join(entry.file_name());
        if let Ok((_, sysfs, _)) = identify(&path) {
            let sectors = read_number(&sysfs.join("size"))?;
            if let Some(bytes) = sectors.checked_mul(512).filter(|bytes| *bytes > 0) {
                devices.push((path, bytes));
            }
        }
    }
    devices.sort_unstable();
    if devices.is_empty() {
        println!("No writable removable USB disks found.");
    }
    for (path, bytes) in devices {
        println!("{}\t{bytes} bytes", path.display());
    }
    Ok(())
}

pub fn open_exclusive(path: &Path) -> io::Result<(File, u64)> {
    let (expected_device, _, expected_sequence) = identify(path)?;

    // Linux claims the whole block device with O_EXCL. A filesystem mount
    // or a competing exclusive holder on the disk or a partition blocks it.
    let disk = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(O_EXCL | O_NONBLOCK)
        .open(path)
        .map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("cannot claim {}: {err}", path.display()),
            )
        })?;
    let opened = disk.metadata()?;
    if !opened.file_type().is_block_device()
        || opened.rdev() != expected_device
        || read_u64_ioctl(&disk, BLKGETDISKSEQ)? != expected_sequence
    {
        return Err(io::Error::other(
            "the selected disk changed while it was being opened",
        ));
    }

    let capacity = read_u64_ioctl(&disk, BLKGETSIZE64)?;
    Ok((disk, capacity))
}

fn identify(path: &Path) -> io::Result<(u64, PathBuf, u64)> {
    let metadata = fs::metadata(path)?;
    if !metadata.file_type().is_block_device() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target must be a block device",
        ));
    }

    let device = metadata.rdev();
    // Linux dev_t, as decoded by glibc's major() and minor().
    let major = ((device >> 8) & 0xfff) | ((device >> 32) & 0xffff_f000);
    let minor = (device & 0xff) | ((device >> 12) & 0xffff_ff00);
    let sysfs = fs::canonicalize(format!("/sys/dev/block/{major}:{minor}"))?;
    let is_usb = sysfs.ancestors().any(|ancestor| {
        fs::read_link(ancestor.join("subsystem"))
            .is_ok_and(|subsystem| subsystem.file_name() == Some("usb".as_ref()))
    });
    if sysfs.join("partition").try_exists()?
        || !is_usb
        || read_number(&sysfs.join("removable"))? != 1
        || read_number(&sysfs.join("ro"))? != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target must be a writable, whole removable USB disk",
        ));
    }

    let sequence = read_number(&sysfs.join("diskseq"))?;
    Ok((device, sysfs, sequence))
}

fn read_number(path: &Path) -> io::Result<u64> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn read_u64_ioctl(disk: &File, request: u32) -> io::Result<u64> {
    let mut output = 0_u64;
    // SAFETY: The fd is owned by disk and the supported requests write
    // exactly one u64 to a valid, aligned pointer.
    let result = unsafe { ioctl(disk.as_raw_fd(), request.into(), &mut output as *mut u64) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(output)
}
