// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SECTOR_BYTES: u64 = 512;
const START_SECTOR: u32 = 2048;
const CLUSTER_SECTORS: u64 = 8;
const CLUSTER_BYTES: u64 = SECTOR_BYTES * CLUSTER_SECTORS;
const MAX_FAT_FILE: u64 = u32::MAX as u64;
const MIB: u64 = 1024 * 1024;

pub fn build_image(iso: &Path, output: &Path) -> io::Result<()> {
    let iso = iso.canonicalize()?;
    if !iso.metadata()?.is_file() {
        return Err(invalid("the input ISO must be a regular file"));
    }
    let name = output
        .file_name()
        .ok_or_else(|| invalid("output needs a filename"))?;
    let parent = output.parent().filter(|path| !path.as_os_str().is_empty());
    let parent = parent.unwrap_or(Path::new(".")).canonicalize()?;
    let output = parent.join(name);
    match output.symlink_metadata() {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "output already exists",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let staging = Staging::new(&parent)?;
    let files = staging.0.join("files");
    fs::create_dir(&files)?;

    eprintln!("Extracting Windows ISO...");
    let mut destination = OsString::from("-o");
    destination.push(&files);
    let extract = |exe| {
        let mut command = Command::new(exe);
        command
            .args(["x", "-y", "-bd", "-bso0", "-bsp0"])
            .arg(&destination)
            .arg("--")
            .arg(&iso);
        command
    };
    match run(extract("7z")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => run(extract("7zz"))?,
        result => result?,
    }

    // Reject symbolic links and case-insensitive naming collisions before giving
    // paths to wimlib or mtools.
    allocated_bytes(&files, false)?;
    let efi = child(&files, "efi")?;
    let boot = child(&efi, "boot")?;
    let loaders = ["bootx64.efi", "bootaa64.efi"];
    if !loaders.iter().any(|name| file_named(&boot, name).is_ok()) {
        return Err(invalid("ISO needs an x64 or ARM64 UEFI fallback loader"));
    }
    let microsoft = child(&efi, "microsoft")?;
    file_named(&child(&microsoft, "boot")?, "bcd")?;
    let sources = child(&files, "sources")?;
    file_named(&sources, "boot.wim")?;
    file_named(&files, "setup.exe")?;
    if let Some(wim) = find_child(&sources, "install.wim")? {
        let metadata = wim.metadata()?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(invalid("install.wim must be a nonempty regular file"));
        }
        if metadata.len() > MAX_FAT_FILE {
            eprintln!("Splitting install.wim into FAT32-compatible files...");
            split_wim(&sources, &wim, "3800")?;
        }
    } else if file_named(&sources, "install.esd").is_err()
        && file_named(&sources, "install.swm").is_err()
    {
        return Err(invalid("ISO is missing the Windows installation image"));
    }

    let contents = allocated_bytes(&files, true)?;
    let partition_sectors = partition_sectors(contents)?;
    let disk = staging.0.join("windows.img");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&disk)?;
    file.set_len((u64::from(START_SECTOR) + u64::from(partition_sectors)) * SECTOR_BYTES)?;
    file.write_all(&partition_table(partition_sectors))?;
    drop(file);

    eprintln!("Creating an MBR disk with one FAT32 partition...");
    let offset = u64::from(START_SECTOR) * SECTOR_BYTES;
    let mut drive = disk.clone().into_os_string();
    drive.push(format!("@@{offset}"));
    let mut format = Command::new("mformat");
    format
        .env("MTOOLSRC", "/dev/null")
        .args(["-i"])
        .arg(&drive)
        .args([
            "-F",
            "-v",
            "WINSETUP",
            "-c",
            &CLUSTER_SECTORS.to_string(),
            "-H",
            &START_SECTOR.to_string(),
            "-T",
            &partition_sectors.to_string(),
            "::",
        ]);
    run(format)?;

    eprintln!("Copying Windows Setup files...");
    let mut copy = Command::new("mcopy");
    copy.env("MTOOLSRC", "/dev/null")
        .args(["-i"])
        .arg(&drive)
        .arg("-s");
    for entry in fs::read_dir(files)? {
        copy.arg(entry?.path());
    }
    copy.arg("::/");
    run(copy)?;

    OpenOptions::new().write(true).open(&disk)?.sync_all()?;
    fs::hard_link(&disk, &output)?;
    println!("Windows UEFI disk image ready: {}", output.display());
    Ok(())
}

fn split_wim(sources: &Path, wim: &Path, part_size_mib: &str) -> io::Result<()> {
    if !wim.is_file() || find_child(sources, "install.swm")?.is_some() {
        return Err(invalid(
            "expected an install.wim with no existing split files",
        ));
    }
    // Use a dedicated directory so numbered parts cannot replace existing files.
    let work = Staging::new(sources)?;
    let mut split = Command::new("wimlib-imagex");
    split
        .arg("split")
        .arg(wim)
        .arg(work.0.join("install.swm"))
        .args([part_size_mib, "--check"]);
    run(split)?;
    for entry in fs::read_dir(&work.0)? {
        let entry = entry?;
        if find_child(sources, &entry.file_name().to_string_lossy())?.is_some() {
            return Err(invalid("split WIM part conflicts with an existing file"));
        }
        fs::rename(entry.path(), sources.join(entry.file_name()))?;
    }
    fs::remove_file(wim)
}

fn run(mut command: Command) -> io::Result<()> {
    let tool = command.get_program().to_string_lossy().into_owned();
    let status = command
        .stdin(Stdio::null())
        .status()
        .map_err(|error| io::Error::new(error.kind(), format!("{tool}: {error}")))?;
    if !status.success() {
        return Err(io::Error::other(format!("{tool} failed ({status})")));
    }
    Ok(())
}

fn child(parent: &Path, name: &str) -> io::Result<PathBuf> {
    find_child(parent, name)?.ok_or_else(|| invalid(format!("ISO is missing {name}")))
}

fn file_named(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let path = child(parent, name)?;
    let metadata = path.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(invalid(format!("{name} must be a nonempty regular file")));
    }
    Ok(path)
}

fn find_child(parent: &Path, name: &str) -> io::Result<Option<PathBuf>> {
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry
            .file_name()
            .as_encoded_bytes()
            .eq_ignore_ascii_case(name.as_bytes())
        {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

fn allocated_bytes(directory: &Path, check_file_size: bool) -> io::Result<u64> {
    let mut bytes = 0u64;
    let mut dir_bytes = 64u64;
    let mut names = HashSet::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| invalid("invalid filename in ISO"))?;
        if !names.insert(name.to_uppercase()) {
            return Err(invalid(format!("conflicting FAT filenames in ISO: {name}")));
        }
        // Budget one short directory entry and one long entry per 13 UTF-16
        // code units, including a possible terminator.
        dir_bytes += 32 * (1 + (name.encode_utf16().count() as u64 + 1).div_ceil(13));
        let metadata = entry.path().symlink_metadata()?;
        let size = if metadata.is_dir() {
            allocated_bytes(&entry.path(), check_file_size)?
        } else if metadata.is_file() {
            if check_file_size && metadata.len() > MAX_FAT_FILE {
                return Err(invalid(format!("{name} exceeds the FAT32 file size limit")));
            }
            metadata.len().div_ceil(CLUSTER_BYTES) * CLUSTER_BYTES
        } else {
            return Err(invalid(format!(
                "ISO contains an unsupported entry: {name}"
            )));
        };
        bytes = bytes
            .checked_add(size)
            .ok_or_else(|| invalid("ISO is too large"))?;
    }
    bytes
        .checked_add(dir_bytes.div_ceil(CLUSTER_BYTES) * CLUSTER_BYTES)
        .ok_or_else(|| invalid("ISO is too large"))
}

fn partition_sectors(contents: u64) -> io::Result<u32> {
    // Allow room for both FATs, reserved sectors, and directory growth. The
    // minimum keeps the FAT32 cluster count valid even for a tiny test ISO.
    let bytes = contents
        .checked_add(contents / 50 + 32 * MIB)
        .ok_or_else(|| invalid("ISO is too large"))?
        .max(320 * MIB);
    let sectors = bytes.div_ceil(MIB) * (MIB / SECTOR_BYTES);
    u32::try_from(sectors)
        .ok()
        .filter(|count| count.checked_add(START_SECTOR).is_some())
        .ok_or_else(|| invalid("ISO exceeds the supported MBR disk size"))
}

fn partition_table(partition_sectors: u32) -> [u8; SECTOR_BYTES as usize] {
    let mut mbr = [0; SECTOR_BYTES as usize];
    // FAT32 LBA primary partition. Windows Setup can access it as a data volume.
    mbr[447..450].copy_from_slice(&[0xfe, 0xff, 0xff]);
    mbr[450] = 0x0c;
    mbr[451..454].copy_from_slice(&[0xfe, 0xff, 0xff]);
    mbr[454..458].copy_from_slice(&START_SECTOR.to_le_bytes());
    mbr[458..462].copy_from_slice(&partition_sectors.to_le_bytes());
    mbr[510..].copy_from_slice(&[0x55, 0xaa]);
    mbr
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

struct Staging(PathBuf);

impl Staging {
    fn new(parent: &Path) -> io::Result<Self> {
        let mut random = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let path = parent.join(format!(".rufus-{:032x}", u128::from_ne_bytes(random)));
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_a_fat32_lba_partition() {
        let count = partition_sectors(4096).unwrap();
        let mbr = partition_table(count);
        assert_eq!(mbr[450], 0x0c);
        assert_eq!(&mbr[454..458], &START_SECTOR.to_le_bytes());
        assert_eq!(&mbr[458..462], &count.to_le_bytes());
        assert_eq!(&mbr[510..], &[0x55, 0xaa]);
        assert!(partition_sectors(u64::MAX).is_err());
    }

    #[test]
    #[ignore = "requires wimlib-imagex"]
    fn splits_install_wim_into_multiple_parts() -> io::Result<()> {
        let work = Staging::new(&std::env::temp_dir())?;
        let input = work.0.join("input");
        let sources = work.0.join("sources");
        fs::create_dir(&input)?;
        fs::create_dir(&sources)?;
        for value in 0..4u8 {
            fs::write(input.join(value.to_string()), vec![value; 768 * 1024])?;
        }
        let wim = sources.join("Install.WIM");
        let mut capture = Command::new("wimlib-imagex");
        capture
            .arg("capture")
            .arg(input)
            .arg(&wim)
            .args(["Fixture", "--compress=none", "--quiet"]);
        run(capture)?;
        split_wim(&sources, &wim, "1")?;
        assert!(!wim.exists());
        assert!(sources.join("install2.swm").is_file());
        let mut verify = Command::new("wimlib-imagex");
        verify
            .arg("verify")
            .arg(sources.join("install.swm"))
            .arg(format!("--ref={}/*.swm", sources.display()));
        run(verify)
    }
}
