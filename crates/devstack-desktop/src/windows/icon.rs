//! A program's icon as the shell shows it (ADR-0046, addendum 2026-11-23):
//! the shell item of the program (its file, or a packaged app's entry in
//! `shell:AppsFolder`) asked for its image, read back as a PNG. Nothing of a
//! vendor's artwork ships with devstack; it is what this machine has.

use super::wide;
use std::path::Path;

/// Edge in pixels: crisp at the dashboard's 14 to 16 px on a 2x screen.
const PX: i32 = 32;

/// The program that opens links of `scheme`: a packaged app by its
/// application id (Claude Desktop), else the program file the association
/// names (VS Code, Zed).
pub fn scheme(scheme: &str) -> Option<Vec<u8>> {
    use windows_sys::Win32::UI::Shell::{ASSOCSTR_APPID, ASSOCSTR_EXECUTABLE};
    let _com = Com::init();
    if let Some(app) = assoc(scheme, ASSOCSTR_APPID) {
        return item(&format!(r"shell:AppsFolder\{app}"));
    }
    item(&assoc(scheme, ASSOCSTR_EXECUTABLE)?)
}

/// A program file. An app execution alias (`wt.exe`) shows only a generic
/// window as its own icon: the packaged app it starts, by the application id
/// the alias holds.
pub fn file(path: &Path) -> Option<Vec<u8>> {
    let _com = Com::init();
    match alias_app(path) {
        Some(app) => item(&format!(r"shell:AppsFolder\{app}")),
        None => item(&path.to_string_lossy()),
    }
}

/// The application id an app execution alias starts: its reparse point
/// (`IO_REPARSE_TAG_APPEXECLINK`) holds, after a version, the package family
/// name, the application id and the target, each zero-terminated.
fn alias_app(path: &Path) -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;
    const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;
    let name = wide(&path.to_string_lossy());
    let mut buf = vec![0u8; 16 * 1024];
    let mut got = 0u32;
    // SAFETY: our own buffers; the handle is closed.
    let ok = unsafe {
        let h = CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let ok = DeviceIoControl(
            h,
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut got,
            std::ptr::null_mut(),
        );
        CloseHandle(h);
        ok != 0
    };
    if !ok {
        return None;
    }
    appexeclink_app(&buf[..got as usize], IO_REPARSE_TAG_APPEXECLINK)
}

/// The application id in an `APPEXECLINK` reparse buffer: tag (4 bytes),
/// length (2), reserved (2), version (4), then UTF-16 strings.
fn appexeclink_app(buf: &[u8], tag: u32) -> Option<String> {
    if buf.len() < 12 || u32::from_le_bytes(buf[..4].try_into().ok()?) != tag {
        return None;
    }
    let units: Vec<u16> = buf[12..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    let app = units.split(|&u| u == 0).nth(1)?;
    let app = String::from_utf16(app).ok()?;
    (app.contains('!')).then_some(app)
}

/// What the association of the link scheme `scheme` says about `what`.
fn assoc(scheme: &str, what: windows_sys::Win32::UI::Shell::ASSOCSTR) -> Option<String> {
    use windows_sys::Win32::UI::Shell::{ASSOCF_IS_PROTOCOL, AssocQueryStringW};
    let name = wide(scheme);
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: our own buffers; `len` is the buffer's length in and the
    // answer's (with its terminating zero) out.
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL,
            what,
            name.as_ptr(),
            std::ptr::null(),
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    let len = (len as usize).saturating_sub(1).min(buf.len());
    (hr == 0 && len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
}

/// The shell item `name` (a path or a `shell:` name) as a PNG of [`PX`].
fn item(name: &str) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY,
    };
    use windows::core::PCWSTR;
    let name = wide(name);
    // SAFETY: COM is initialised by the caller (`Com`); the name is ours
    // and zero-terminated; the bitmap is ours to delete.
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None).ok()?;
        let bitmap = factory
            .GetImage(SIZE { cx: PX, cy: PX }, SIIGBF_ICONONLY)
            .ok()?;
        let rgba = pixels(bitmap.0);
        windows_sys::Win32::Graphics::Gdi::DeleteObject(bitmap.0);
        let (w, h, rgba) = rgba?;
        Some(crate::png::encode(w, h, &rgba))
    }
}

/// A 32-bit bitmap's pixels, top row first, as straight RGBA.
///
/// # Safety
/// `bitmap` is a valid `HBITMAP` not selected into a device context.
unsafe fn pixels(bitmap: *mut core::ffi::c_void) -> Option<(u32, u32, Vec<u8>)> {
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDC, GetDIBits, GetObjectW,
        ReleaseDC,
    };
    // SAFETY: as the caller promises; our own, sized buffers.
    unsafe {
        let mut bm: BITMAP = std::mem::zeroed();
        if GetObjectW(bitmap, size_of::<BITMAP>() as i32, (&raw mut bm).cast()) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 || w > 1024 || h > 1024 {
            return None;
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative: top row first.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(std::ptr::null_mut());
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            h as u32,
            buf.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(std::ptr::null_mut(), dc);
        if lines != h {
            return None;
        }
        Some((w as u32, h as u32, straight_rgba(buf)))
    }
}

/// The shell's premultiplied BGRA as straight RGBA; a bitmap without any
/// alpha (an old icon) as opaque.
fn straight_rgba(mut px: Vec<u8>) -> Vec<u8> {
    let has_alpha = px.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
        if !has_alpha {
            p[3] = 255;
        } else if p[3] != 0 && p[3] != 255 {
            let a = u32::from(p[3]);
            for c in &mut p[..3] {
                *c = (u32::from(*c) * 255 / a).min(255) as u8;
            }
        }
    }
    px
}

/// COM for this thread, released again: the shell's items need it.
struct Com(bool);

impl Com {
    fn init() -> Self {
        use windows_sys::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
        };
        // SAFETY: plain call; released in `drop` when it succeeded.
        let hr = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        Self(hr >= 0)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with the successful `CoInitializeEx` above.
            unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explorer is on every Windows desktop, and has an icon with alpha.
    #[test]
    fn explorer_has_an_icon() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let png = file(&Path::new(&root).join("explorer.exe")).expect("an icon");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "{:?}", &png[..8]);
        // IHDR: width and height.
        assert_eq!(&png[16..24], &[0, 0, 0, 32, 0, 0, 0, 32]);
    }

    #[test]
    fn a_scheme_nobody_opens_has_none() {
        assert!(scheme("devstack-no-such-scheme-4f1c").is_none());
    }

    #[test]
    fn an_alias_names_the_app_it_starts() {
        let tag = 0x8000_001Bu32;
        let mut buf = tag.to_le_bytes().to_vec();
        buf.extend([0; 4]); // length, reserved
        buf.extend(3u32.to_le_bytes()); // version
        for s in [
            "Microsoft.WindowsTerminal_8wekyb3d8bbwe",
            "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App",
            r"C:\Program Files\WindowsApps\x\wt.exe",
            "0",
        ] {
            buf.extend(s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        }
        assert_eq!(
            appexeclink_app(&buf, tag).as_deref(),
            Some("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App")
        );
        // Another kind of reparse point (a symbolic link): not an alias.
        assert_eq!(appexeclink_app(&buf, 0xA000_000C), None);
    }

    #[test]
    fn premultiplied_bgra_becomes_straight_rgba() {
        // Half-transparent pure red, premultiplied: B=0 G=0 R=128 A=128.
        assert_eq!(straight_rgba(vec![0, 0, 128, 128]), vec![255, 0, 0, 128]);
        // No alpha anywhere: opaque.
        assert_eq!(straight_rgba(vec![1, 2, 3, 0]), vec![3, 2, 1, 255]);
    }
}
