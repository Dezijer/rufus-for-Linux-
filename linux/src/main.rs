// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

mod device;
mod windows;

use std::env;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "Usage:
  rufus-linux list
  rufus-linux windows-image WINDOWS_ISO OUTPUT_IMG
  rufus-linux write IMAGE DEVICE --erase

Prepare Windows UEFI installer media as a disk image, or write an image
to an unmounted removable USB disk.
The write command overwrites data on the selected disk.";
const BUFFER_SIZE: usize = 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rufus-linux: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[std::ffi::OsString]) -> io::Result<()> {
    match args {
        [command] if command == "list" => device::list(),
        [command] if command == "--help" || command == "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        [command, iso, output] if command == "windows-image" => {
            windows::build_image(Path::new(iso), Path::new(output))
        }
        [command, image, disk, flag] if command == "write" && flag == "--erase" => {
            write_image(image, disk)
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE)),
    }
}

fn write_image(image: &OsStr, disk: &OsStr) -> io::Result<()> {
    let mut image = OpenOptions::new()
        .read(true)
        .custom_flags(device::O_NONBLOCK)
        .open(image)?;
    let input = image.metadata()?;
    let length = input.len();
    if !input.is_file() || length == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the image must be a nonempty regular file",
        ));
    }

    let (mut disk, capacity) = device::open_exclusive(Path::new(disk))?;
    if length > capacity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("image is {length} bytes but the disk only has {capacity} bytes"),
        ));
    }
    eprintln!("Writing {length} bytes...");
    copy_image(&mut image, &mut disk, length)?;
    disk.sync_all()?;

    eprintln!("Verifying {length} bytes...");
    verify_image(&mut image, &mut disk, length)?;
    let current = image.metadata()?;
    if (
        current.len(),
        current.mtime(),
        current.mtime_nsec(),
        current.ctime(),
        current.ctime_nsec(),
    ) != (
        length,
        input.mtime(),
        input.mtime_nsec(),
        input.ctime(),
        input.ctime_nsec(),
    ) {
        return Err(io::Error::other(
            "the input image changed during the operation",
        ));
    }
    println!("Wrote and verified {length} bytes.");
    Ok(())
}

fn copy_image(image: &mut impl Read, disk: &mut impl Write, length: u64) -> io::Result<()> {
    let copied = io::copy(&mut image.take(length), disk)?;
    if copied != length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the input image ended before all bytes could be written",
        ));
    }
    Ok(())
}

fn verify_image(
    image: &mut (impl Read + Seek),
    disk: &mut (impl Read + Seek),
    length: u64,
) -> io::Result<()> {
    image.seek(SeekFrom::Start(0))?;
    disk.seek(SeekFrom::Start(0))?;
    let mut expected = vec![0; BUFFER_SIZE];
    let mut actual = vec![0; BUFFER_SIZE];
    let mut offset = 0;
    while offset < length {
        let size = (length - offset).min(BUFFER_SIZE as u64) as usize;
        image.read_exact(&mut expected[..size])?;
        disk.read_exact(&mut actual[..size])?;
        if let Some(index) = expected[..size]
            .iter()
            .zip(&actual[..size])
            .position(|(expected, actual)| expected != actual)
        {
            return Err(io::Error::other(format!(
                "verification failed at byte {}",
                offset + index as u64
            )));
        }
        offset += size as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn copies_and_verifies_the_image_without_touching_trailing_bytes() {
        let mut image = Cursor::new(vec![0xa5; BUFFER_SIZE + 17]);
        let mut disk = Cursor::new(vec![0x5a; BUFFER_SIZE + 18]);
        let length = image.get_ref().len() as u64;
        copy_image(&mut image, &mut disk, length).unwrap();
        verify_image(&mut image, &mut disk, length).unwrap();
        assert_eq!(disk.into_inner()[length as usize], 0x5a);
    }

    #[test]
    fn rejects_a_short_source() {
        let error = copy_image(&mut &b"x"[..], &mut Vec::new(), 2).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn reports_corruption_in_the_last_chunk() {
        let mut image = Cursor::new(vec![0; BUFFER_SIZE + 1]);
        let mut disk = image.clone();
        disk.get_mut()[BUFFER_SIZE] = 1;
        let error = verify_image(&mut image, &mut disk, BUFFER_SIZE as u64 + 1).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("verification failed at byte {BUFFER_SIZE}")
        );
    }

    #[test]
    fn rejects_a_truncated_readback() {
        let mut image = Cursor::new(b"abc");
        let mut disk = Cursor::new(b"ab");
        let error = verify_image(&mut image, &mut disk, 3).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}
