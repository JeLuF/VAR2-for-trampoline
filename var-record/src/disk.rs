//! Free disk space of the recording drive (DSK-001).

use std::io;
use std::path::Path;

/// Bytes available to this user on the drive holding `path`. `path` need not exist
/// yet: the nearest existing parent folder is used.
pub fn available_space(path: &Path) -> io::Result<u64> {
    let existing = path
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.exists())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no existing folder in the path"))?;
    available_space_of(existing)
}

#[cfg(windows)]
fn available_space_of(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path; the out pointer is valid, the
    // others may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

#[cfg(unix)]
fn available_space_of(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid NUL-terminated path and a valid out structure.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_of_temp_and_missing_subfolder() {
        let temp = std::env::temp_dir();
        let free = available_space(&temp).unwrap();
        assert!(free > 0);
        // A folder that doesn't exist yet: its parent's drive.
        let missing = temp.join("var-does-not-exist").join("event");
        let free_missing = available_space(&missing).unwrap();
        // Same drive; free space may change slightly in between.
        assert!(free_missing.abs_diff(free) < 1 << 30);
    }
}
