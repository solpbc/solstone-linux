// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Bind an inventory or content read to an object below the chosen directory.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use rustix::fs::{CWD, Mode, OFlags, ResolveFlags, openat2};
use rustix::io::Errno;
use sha2::{Digest, Sha256};

pub(crate) fn object(root: &Path, relative: &Path) -> io::Result<File> {
    let flags = OFlags::CLOEXEC | OFlags::PATH;
    // The picker stores a canonical path. Refuse if the root or any ancestor
    // has since been replaced by a symbolic link.
    let dir = openat2(
        CWD,
        root,
        flags | OFlags::DIRECTORY,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )?;
    let resolve = ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS;
    let fd = match openat2(&dir, relative, flags, Mode::empty(), resolve) {
        Ok(fd) => fd,
        Err(Errno::XDEV) => {
            // Absolute symlinks pointing inside the folder are supported too.
            let canonical = root.join(relative).canonicalize()?;
            let within = canonical
                .strip_prefix(root)
                .map_err(|_| io::Error::from(Errno::XDEV))?;
            openat2(&dir, within, flags, Mode::empty(), resolve)?
        }
        Err(err) => return Err(err.into()),
    };
    Ok(File::from(fd))
}

// Linux UAPI filesystem magic values. Local disk filesystems and kernel
// NFS/SMB expose file data directly. Unknown and userspace filesystems
// can retrieve cloud data on open, so only their attributes are used.
fn supports_content(magic: u32) -> bool {
    matches!(
        magic,
        0xef53 | 0x9123683e | 0x58465342 | 0xf2f52010 | 0x3434 | // ext, btrfs, XFS, F2FS, NILFS
        0x2fc12fc1 | 0x52654973 | 0x3153464a | 0x5346544e | // ZFS, ReiserFS, JFS, NTFS
        0x4d44 | 0x2011bab0 | 0x9660 | 0x15013346 | // FAT, exFAT, ISO, UDF
        0x01021994 | 0x858458f6 | 0x794c7630 | 0x73717368 | 0xe0f5e1e2 | // tmpfs, ramfs, overlay, squashfs, EROFS
        0x6969 | 0x517b | 0xff534d42 | 0xfe534d42 // kernel NFS, SMB, CIFS, SMB2
    )
}

pub(crate) fn content_available(file: &File) -> bool {
    rustix::fs::fstatfs(file).is_ok_and(|fs| supports_content(fs.f_type as u32))
}

pub(super) fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

pub(crate) fn copy_file(
    root: &Path,
    relative: &Path,
    limit: u64,
    dest: &mut impl Write,
) -> io::Result<()> {
    let handle = object(root, relative)?;
    let before = handle.metadata()?;
    if !content_available(&handle) || !before.is_file() || before.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "folder file exceeds the read limit or is not regular",
        ));
    }
    let file = File::open(fd_path(&handle))?;
    let count = io::copy(&mut file.take(limit.saturating_add(1)), dest)?;
    let after = handle.metadata()?;
    if count != before.len() || count != after.len() || before.modified()? != after.modified()? {
        return Err(io::Error::other("folder file changed during the read"));
    }
    Ok(())
}

pub(super) fn read_file(root: &Path, relative: &str, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    copy_file(root, Path::new(relative), limit, &mut bytes)?;
    Ok(bytes)
}

pub(super) fn hash_file(root: &Path, relative: &str, limit: u64) -> io::Result<String> {
    let mut hash = Sha256::new();
    copy_file(root, Path::new(relative), limit, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_fuse_mounts_use_only_attributes() {
        assert!(!supports_content(0x65735546));
        assert!(!supports_content(0x1234));
        assert!(supports_content(0x6969));
        assert!(supports_content(0xff534d42));
    }

    #[test]
    fn reads_in_folder_aliases_and_obeys_the_byte_limit() {
        let root = tempfile::tempdir().unwrap();
        let note = root.path().join("note");
        std::fs::write(&note, "hello").unwrap();
        std::os::unix::fs::symlink(&note, root.path().join("alias")).unwrap();
        assert_eq!(read_file(root.path(), "alias", 5).unwrap(), b"hello");
        assert!(read_file(root.path(), "alias", 4).is_err());
        assert!(read_file(root.path(), ".", 5).is_err());
    }
}
