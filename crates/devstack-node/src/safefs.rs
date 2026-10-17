//! Files in a lane's mirror, where the lane's own code writes too: it can
//! put a symlink anywhere in it (`ln -s ../../../bin evil`), and the agent
//! that follows one writes or reads outside the mirror, as itself. Every
//! path here is taken beneath a root one component at a time, and a symlink
//! on the way is never followed: on Unix with `openat` and `O_NOFOLLOW`
//! (no window between a check and the use), elsewhere by checking each
//! component (a node runs lanes on Linux only; Windows needs it for tests).
//!
//! Paths are relative, `/`-separated and checked by the caller (no `..`, no
//! root); [`components`] refuses the rest.

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// The components of a relative path, or an error for anything else.
pub fn components(rel: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    let mut out = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(n) => out.push(n.to_os_string()),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("`{}` is not a path inside the lane", rel.display()),
                ));
            }
        }
    }
    if out.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "an empty path"));
    }
    Ok(out)
}

fn symlink_err(rel: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "`{}` passes a symlink: the agent does not follow links in a lane",
            rel.display()
        ),
    )
}

/// A unique name for a file being written beside `name`.
fn tmp_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut b = [0u8; 6];
    rand::fill(&mut b[..]);
    let tag: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let mut t = std::ffi::OsString::from(".");
    t.push(name);
    t.push(format!(".{tag}.devstack-tmp"));
    t
}

/// Write the file `rel` beneath `root` from what `fill` writes, with
/// `mode` (Unix), replacing what is there (a symlink there is replaced, not
/// followed). Directories on the way are made; a symlink or a file where a
/// directory belongs is removed first, so the mirror becomes what the
/// requester listed. Nothing is in place unless `fill` succeeds.
pub fn write_beneath(
    root: &Path,
    rel: &Path,
    mode: u32,
    fill: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    imp::write(root, rel, mode, fill)
}

/// Remove the file `rel` beneath `root` (a symlink there itself), and the
/// directories it leaves empty, up to `root`. Whether a file went.
pub fn remove_beneath(root: &Path, rel: &Path) -> io::Result<bool> {
    imp::remove(root, rel)
}

/// Read the regular file `rel` beneath `root`, at most `max` bytes.
pub fn read_beneath(root: &Path, rel: &Path, max: u64) -> io::Result<Vec<u8>> {
    let mut f = imp::open_read(root, rel)?;
    let mut buf = Vec::new();
    (&mut f).take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("`{}` is larger than {max} bytes", rel.display()),
        ));
    }
    Ok(buf)
}

/// `path` (absolute, or relative to `root`) as it resolves, if that is
/// beneath `root`: for paths handed to Docker, which follows links. A path
/// that does not exist yet is resolved as far as it does.
pub fn resolve_beneath(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    // The longest part that exists, resolved; the rest appended as it is
    // (no `..` in it).
    let mut rest = Vec::new();
    let mut cur = path.as_path();
    let resolved = loop {
        match std::fs::canonicalize(cur) {
            Ok(r) => break r,
            Err(_) => {
                let (Some(parent), Some(name)) = (cur.parent(), cur.file_name()) else {
                    return Err(format!("`{}` does not resolve", path.display()));
                };
                rest.push(name.to_os_string());
                cur = parent;
            }
        }
    };
    let mut full = resolved;
    for n in rest.into_iter().rev() {
        if n == ".." {
            return Err(format!("`{}` leaves the lane", path.display()));
        }
        full.push(n);
    }
    if full.starts_with(&root) {
        Ok(full)
    } else {
        Err(format!(
            "`{}` is outside the lane (it resolves to {})",
            path.display(),
            full.display()
        ))
    }
}

/// Copy `from` into `to` while hashing it; the BLAKE3 hash of what was
/// copied.
pub fn copy_hashing(from: &mut impl Read, to: &mut impl Write) -> io::Result<String> {
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        to.write_all(&buf[..n])?;
    }
    Ok(h.finalize().to_hex().to_string())
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::ffi::{CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    fn cstr(s: &OsStr) -> io::Result<CString> {
        CString::new(s.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL"))
    }

    fn check(r: libc::c_int) -> io::Result<libc::c_int> {
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(r)
        }
    }

    fn open_root(root: &Path) -> io::Result<OwnedFd> {
        let p = cstr(root.as_os_str())?;
        let fd = check(unsafe {
            libc::open(
                p.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn open_dir_at(dir: &OwnedFd, name: &CString) -> io::Result<OwnedFd> {
        let fd = check(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Is `name` in `dir` a directory (not a link to one)?
    fn kind_at(dir: &OwnedFd, name: &CString) -> io::Result<libc::mode_t> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        check(unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        Ok(st.st_mode & libc::S_IFMT)
    }

    /// The directories of `comps` beneath `root`, opened without following
    /// a link; made (`create`) where missing, with what is in the way
    /// removed. `None` when one is missing and `create` is off.
    fn walk(
        root: &Path,
        comps: &[std::ffi::OsString],
        create: bool,
    ) -> io::Result<Option<Vec<OwnedFd>>> {
        let mut fds = vec![open_root(root)?];
        for c in comps {
            let name = cstr(c)?;
            let dir = fds.last().expect("the root at least");
            let next = match open_dir_at(dir, &name) {
                Ok(fd) => fd,
                Err(e) if !create => {
                    return match e.raw_os_error() {
                        Some(libc::ENOENT) => Ok(None),
                        Some(libc::ELOOP) | Some(libc::ENOTDIR) => Err(symlink_err(Path::new(c))),
                        _ => Err(e),
                    };
                }
                Err(e) => {
                    match e.raw_os_error() {
                        Some(libc::ENOENT) => {}
                        Some(libc::ELOOP) | Some(libc::ENOTDIR) => {
                            // A link or a file where a directory belongs:
                            // it goes (the entry itself, never its target).
                            if kind_at(dir, &name)? != libc::S_IFDIR {
                                check(unsafe {
                                    libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0)
                                })?;
                            }
                        }
                        _ => return Err(e),
                    }
                    match check(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) }) {
                        Ok(_) => {}
                        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {}
                        Err(e) => return Err(e),
                    }
                    open_dir_at(dir, &name)?
                }
            };
            fds.push(next);
        }
        Ok(Some(fds))
    }

    pub fn write(
        root: &Path,
        rel: &Path,
        mode: u32,
        fill: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        let comps = components(rel)?;
        let (name, dirs) = comps.split_last().expect("not empty");
        let fds = walk(root, dirs, true)?.expect("created");
        let dir = fds.last().expect("the root at least");
        let tmp = cstr(&tmp_name(name))?;
        let fd = check(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        })?;
        let mut f = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let filled = fill(&mut f).and_then(|_| {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(mode))
        });
        drop(f);
        let name = cstr(name)?;
        let done = filled.and_then(|_| {
            check(unsafe {
                libc::renameat(
                    dir.as_raw_fd(),
                    tmp.as_ptr(),
                    dir.as_raw_fd(),
                    name.as_ptr(),
                )
            })
            .map(|_| ())
        });
        if done.is_err() {
            unsafe { libc::unlinkat(dir.as_raw_fd(), tmp.as_ptr(), 0) };
        }
        done
    }

    pub fn remove(root: &Path, rel: &Path) -> io::Result<bool> {
        let comps = components(rel)?;
        let (name, dirs) = comps.split_last().expect("not empty");
        let Some(fds) = walk(root, dirs, false).or_else(|e| {
            // A link on the way: the file is not in the mirror anyway.
            if e.kind() == io::ErrorKind::PermissionDenied {
                Ok(None)
            } else {
                Err(e)
            }
        })?
        else {
            return Ok(false);
        };
        let n = cstr(name)?;
        let dir = fds.last().expect("the root at least");
        let gone = unsafe { libc::unlinkat(dir.as_raw_fd(), n.as_ptr(), 0) } == 0;
        // Directories left empty go too (rmdir fails on one that is not).
        for (i, c) in dirs.iter().enumerate().rev() {
            let c = cstr(c)?;
            if unsafe { libc::unlinkat(fds[i].as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                break;
            }
        }
        Ok(gone)
    }

    pub fn open_read(root: &Path, rel: &Path) -> io::Result<std::fs::File> {
        let comps = components(rel)?;
        let (name, dirs) = comps.split_last().expect("not empty");
        let fds = walk(root, dirs, false)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.display().to_string()))?;
        let dir = fds.last().expect("the root at least");
        let n = cstr(name)?;
        let fd = check(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                n.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        })
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                symlink_err(rel)
            } else {
                e
            }
        })?;
        let f = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        if !f.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("`{}` is not a file", rel.display()),
            ));
        }
        Ok(f)
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    /// The directory of `dirs` beneath `root`, checked one component at a
    /// time; made where missing (`create`), with a link or file in the way
    /// removed.
    fn walk(root: &Path, dirs: &[std::ffi::OsString], create: bool) -> io::Result<Option<PathBuf>> {
        let mut cur = root.to_path_buf();
        for c in dirs {
            cur.push(c);
            match std::fs::symlink_metadata(&cur) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => continue,
                Ok(_) if !create => return Err(symlink_err(&cur)),
                Ok(m) => {
                    if m.is_dir() {
                        std::fs::remove_dir(&cur)?; // a directory link
                    } else {
                        std::fs::remove_file(&cur)?;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            std::fs::create_dir(&cur)?;
        }
        Ok(Some(cur))
    }

    pub fn write(
        root: &Path,
        rel: &Path,
        _mode: u32,
        fill: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        let comps = components(rel)?;
        let (name, dirs) = comps.split_last().expect("not empty");
        let dir = walk(root, dirs, true)?.expect("created");
        let tmp = dir.join(tmp_name(name));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        let filled = fill(&mut f);
        drop(f);
        let target = dir.join(name);
        let done = filled.and_then(|_| {
            if std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = std::fs::remove_file(&target).or_else(|_| std::fs::remove_dir(&target));
            }
            std::fs::rename(&tmp, &target)
        });
        if done.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        done
    }

    pub fn remove(root: &Path, rel: &Path) -> io::Result<bool> {
        let comps = components(rel)?;
        let (name, dirs) = comps.split_last().expect("not empty");
        let Some(dir) = walk(root, dirs, false).or_else(|e| {
            if e.kind() == io::ErrorKind::PermissionDenied {
                Ok(None)
            } else {
                Err(e)
            }
        })?
        else {
            return Ok(false);
        };
        let gone = std::fs::remove_file(dir.join(name)).is_ok();
        let mut d = dir;
        while d != root && d.starts_with(root) && std::fs::remove_dir(&d).is_ok() {
            if !d.pop() {
                break;
            }
        }
        Ok(gone)
    }

    pub fn open_read(root: &Path, rel: &Path) -> io::Result<std::fs::File> {
        let comps = components(rel)?;
        let (name, dirs) = comps.split_last().expect("not empty");
        let dir = walk(root, dirs, false)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.display().to_string()))?;
        let p = dir.join(name);
        let m = std::fs::symlink_metadata(&p)?;
        if m.file_type().is_symlink() {
            return Err(symlink_err(rel));
        }
        if !m.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("`{}` is not a file", rel.display()),
            ));
        }
        std::fs::File::open(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_beneath_and_removes_emptied_dirs() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write_beneath(root, Path::new("a/b/c.txt"), 0o644, |f| f.write_all(b"hi")).unwrap();
        assert_eq!(
            read_beneath(root, Path::new("a/b/c.txt"), 10).unwrap(),
            b"hi"
        );
        assert!(read_beneath(root, Path::new("a/b/c.txt"), 1).is_err());
        // A failed fill leaves nothing behind.
        assert!(
            write_beneath(root, Path::new("a/x"), 0o644, |_| Err(io::Error::other(
                "no"
            )))
            .is_err()
        );
        assert_eq!(std::fs::read_dir(root.join("a")).unwrap().count(), 1);
        assert!(remove_beneath(root, Path::new("a/b/c.txt")).unwrap());
        assert!(!root.join("a").exists(), "emptied directories go");
        assert!(components(Path::new("../x")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_mirror_is_never_followed() {
        let d = tempfile::tempdir().unwrap();
        let (root, outside) = (d.path().join("src"), d.path().join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "key").unwrap();
        std::os::unix::fs::symlink("../outside", root.join("evil")).unwrap();
        std::os::unix::fs::symlink("../outside/secret", root.join("token")).unwrap();
        // Reading through a link fails, at the end or on the way.
        assert!(read_beneath(&root, Path::new("token"), 100).is_err());
        assert!(read_beneath(&root, Path::new("evil/secret"), 100).is_err());
        // Writing through one replaces the link with a directory.
        write_beneath(&root, Path::new("evil/secret"), 0o644, |f| {
            f.write_all(b"x")
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(outside.join("secret")).unwrap(),
            "key"
        );
        assert!(root.join("evil").symlink_metadata().unwrap().is_dir());
        // Writing over a link to a file replaces the link.
        write_beneath(&root, Path::new("token"), 0o644, |f| f.write_all(b"t")).unwrap();
        assert_eq!(
            std::fs::read_to_string(outside.join("secret")).unwrap(),
            "key"
        );
        assert!(resolve_beneath(&root, Path::new("evil")).is_ok());
        std::os::unix::fs::symlink("../outside", root.join("out")).unwrap();
        assert!(resolve_beneath(&root, Path::new("out/secret")).is_err());
        assert!(resolve_beneath(&root, Path::new("out/new/file")).is_err());
        assert!(resolve_beneath(&root, &outside).is_err());
    }

    #[test]
    fn resolving_stays_beneath_the_root() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("src");
        std::fs::create_dir_all(root.join("db")).unwrap();
        assert!(resolve_beneath(&root, Path::new("db")).is_ok());
        assert!(resolve_beneath(&root, Path::new("db/init/new")).is_ok());
        assert!(resolve_beneath(&root, &root.join("../x")).is_err());
        assert!(resolve_beneath(&root, d.path()).is_err());
    }
}
