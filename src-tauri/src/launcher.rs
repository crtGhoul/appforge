//! Launcher: native installed programs listed next to web apps.
//!
//! Windows: enumerate Start Menu `.lnk` shortcuts (all-users and per-user
//! Programs folders) plus Desktop shortcuts (per-user and public), resolve
//! each one through `IShellLinkW` to its target, keep the ones pointing at a
//! `.exe`, de-dupe by target path, and extract the exe's icon to PNG
//! best-effort (the UI falls back to a generic glyph when extraction fails).
//! Microsoft Store (UWP) apps are out of scope.
//!
//! Linux: parse `.desktop` files from the system and user applications dirs
//! (name + exec only, no icon extraction) and launch via `gio`/`gtk-launch`.
//!
//! Launching is always by program **id** with a server-side lookup, so the
//! frontend can never ask the backend to run an arbitrary path.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
#[cfg(not(windows))]
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

/// A native installed program found on this PC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeProgram {
    pub id: String,
    /// Display name (the shortcut name / .desktop Name).
    pub name: String,
    /// Absolute `.exe` path on Windows; the `.desktop` file path on Linux.
    pub exe_path: String,
    /// Absolute path of the extracted icon PNG, when extraction worked.
    pub icon_path: Option<String>,
}

fn hash_str(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub struct LauncherState {
    icons_dir: PathBuf,
    cache_path: PathBuf,
    programs: Mutex<Vec<NativeProgram>>,
}

impl LauncherState {
    pub fn new(app: &AppHandle) -> Result<Self, String> {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("could not resolve app data dir: {e}"))?;
        let icons_dir = dir.join("program-icons");
        fs::create_dir_all(&icons_dir)
            .map_err(|e| format!("could not create program icon dir: {e}"))?;
        let cache_path = dir.join("programs.json");
        let programs = fs::read_to_string(&cache_path)
            .ok()
            .and_then(|c| serde_json::from_str::<Vec<NativeProgram>>(&c).ok())
            .unwrap_or_default();
        Ok(Self {
            icons_dir,
            cache_path,
            programs: Mutex::new(programs),
        })
    }

    /// Cached programs (populated by the startup background scan).
    pub fn list(&self) -> Vec<NativeProgram> {
        self.programs
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default()
    }

    /// Full rescan on the calling thread. Run it on a background thread —
    /// COM work plus icon extraction can take a few seconds.
    pub fn rescan(&self) -> usize {
        let found = scan_all(&self.icons_dir);
        let json = serde_json::to_string(&found).unwrap_or_else(|_| "[]".to_string());
        // Cache to disk first so a crash can't lose the scan; best-effort.
        let tmp = self.cache_path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &self.cache_path);
        }
        let n = found.len();
        if let Ok(mut guard) = self.programs.lock() {
            *guard = found;
        }
        n
    }

    /// Launch a program by id (server-side lookup — the frontend never passes
    /// a raw path, so it can't trick the backend into running something else).
    pub fn launch(&self, id: &str) -> Result<(), String> {
        let exe_path = self
            .programs
            .lock()
            .map_err(|e| format!("launcher state poisoned: {e}"))?
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.exe_path.clone())
            .ok_or_else(|| "Program not found.".to_string())?;
        launch_native(&exe_path)
    }
}

#[cfg(windows)]
fn scan_all(icons_dir: &Path) -> Vec<NativeProgram> {
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };

    let mut roots = Vec::new();
    if let Ok(pd) = std::env::var("ProgramData") {
        roots.push(PathBuf::from(pd).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    if let Ok(ad) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(ad).join("Microsoft\\Windows\\Start Menu\\Programs"));
    }
    // Desktop shortcuts too — many installers drop their only shortcut here.
    if let Ok(up) = std::env::var("USERPROFILE") {
        roots.push(PathBuf::from(up).join("Desktop"));
    }
    if let Ok(pd) = std::env::var("PUBLIC") {
        roots.push(PathBuf::from(pd).join("Desktop"));
    }

    // IShellLinkW needs COM on this thread.
    let com_ok =
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };

    let mut lnks = Vec::new();
    for root in &roots {
        collect_lnks(root, &mut lnks);
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for lnk in lnks {
        let name = lnk
            .file_stem()
            .map(|s| s.to_string_lossy().trim().to_string())
            .filter(|s| !s.is_empty());
        let Some(name) = name else { continue };
        let Some(target) = resolve_lnk_target(&lnk) else { continue };
        let is_exe = target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
        if !is_exe {
            continue;
        }
        let key = target.to_string_lossy().to_lowercase();
        if !seen.insert(key.clone()) {
            continue; // same exe linked twice (e.g. per-user + all-users)
        }
        let icon_path = extract_icon_png(&target, icons_dir);
        out.push(NativeProgram {
            id: hash_str(&key),
            name,
            exe_path: target.to_string_lossy().into_owned(),
            icon_path,
        });
    }

    if com_ok {
        unsafe {
            CoUninitialize();
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

#[cfg(windows)]
fn collect_lnks(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_lnks(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        {
            out.push(path);
        }
    }
}

/// Resolve a `.lnk` file to its target path via IShellLinkW. No UI is ever
/// shown (SLR_NO_UI): an unresolvable link just returns None and is skipped.
#[cfg(windows)]
fn resolve_lnk_target(lnk: &Path) -> Option<PathBuf> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CLSCTX_INPROC_SERVER, IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, SLR_NO_UI, ShellLink};
    use windows::core::{Interface, HSTRING, PCWSTR};

    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        let wide = HSTRING::from(lnk.to_string_lossy().as_ref());
        persist.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;
        link.Resolve(HWND::default(), SLR_NO_UI.0 as u32).ok()?;
        let mut buf = [0u16; 32768];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let target = String::from_utf16_lossy(&buf[..len]);
        if target.trim().is_empty() {
            return None;
        }
        Some(PathBuf::from(target))
    }
}

/// Extract the exe's first icon as a 32-bit PNG under `icons_dir`, named by a
/// hash of the exe path so it is only ever extracted once. Returns the
/// absolute PNG path, or None when anything goes wrong (the UI then shows a
/// generic glyph — extraction is best-effort).
#[cfg(windows)]
fn extract_icon_png(exe: &Path, icons_dir: &Path) -> Option<String> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
        DeleteDC, DeleteObject, GetDIBits, GetObjectW, HDC, HGDIOBJ, RGBQUAD,
    };
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, GetIconInfo, HICON, ICONINFO,
    };
    use windows::core::{HSTRING, PCWSTR};

    let out = icons_dir.join(format!(
        "{:016x}.png",
        {
            let mut h = DefaultHasher::new();
            exe.to_string_lossy().to_lowercase().hash(&mut h);
            h.finish()
        }
    ));
    if out.exists() {
        return Some(out.to_string_lossy().into_owned());
    }

    unsafe {
        let wide = HSTRING::from(exe.to_string_lossy().as_ref());
        let mut large = HICON::default();
        let mut small = HICON::default();
        if ExtractIconExW(
            PCWSTR(wide.as_ptr()),
            0,
            Some(&mut large),
            Some(&mut small),
            1,
        ) == 0
        {
            return None;
        }
        let hicon = if !large.is_invalid() {
            large
        } else if !small.is_invalid() {
            small
        } else {
            return None;
        };

        let png_path = (|| -> Option<PathBuf> {
            let mut info = ICONINFO::default();
            GetIconInfo(hicon, &mut info as *mut ICONINFO).ok()?;
            if info.hbmColor.is_invalid() {
                return None; // monochrome icon: skip rather than guess
            }
            let mut bm = BITMAP::default();
            let got = GetObjectW(
                HGDIOBJ(info.hbmColor.0),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut BITMAP as *mut std::ffi::c_void),
            );
            if got == 0 {
                return None;
            }
            let (w, h) = (bm.bmWidth.max(1), bm.bmHeight.max(1));
            let bih = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down: rows come out in display order
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bmi = BITMAPINFO {
                bmiHeader: bih,
                bmiColors: [RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }],
            };
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            let hdc = CreateCompatibleDC(Some(HDC::default()));
            if hdc.is_invalid() {
                return None;
            }
            let lines = GetDIBits(
                hdc,
                info.hbmColor,
                0,
                h as u32,
                Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
                &mut bmi as *mut BITMAPINFO,
                DIB_RGB_COLORS,
            );
            let _ = DeleteDC(hdc);
            if lines == 0 {
                return None;
            }
            // DIB pixels are BGRA; PNG wants RGBA. Icons without an alpha
            // channel render opaque — acceptable for a best-effort icon.
            for px in pixels.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
            let mut buf = Vec::new();
            {
                let mut enc = png::Encoder::new(&mut buf, w as u32, h as u32);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                let mut writer = enc.write_header().ok()?;
                writer.write_image_data(&pixels).ok()?;
            }
            fs::write(&out, &buf).ok()?;
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
            Some(out)
        })();

        let _ = DestroyIcon(hicon);
        png_path.map(|p| p.to_string_lossy().into_owned())
    }
}

/// Launch via ShellExecuteW ("open" verb): Windows picks the exe's working
/// directory from its location and shows the normal UAC prompt itself for
/// programs whose manifest requires elevation.
#[cfg(windows)]
fn launch_native(exe_path: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR};

    unsafe {
        let file = HSTRING::from(exe_path);
        let dir = Path::new(exe_path)
            .parent()
            .map(|p| HSTRING::from(p.to_string_lossy().as_ref()));
        let dir_ptr = dir
            .as_ref()
            .map(|h| PCWSTR(h.as_ptr()))
            .unwrap_or(PCWSTR::null());
        let ret = ShellExecuteW(
            None,
            PCWSTR::null(),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            dir_ptr,
            SW_SHOWNORMAL,
        );
        if ret.0 as usize > 32 {
            Ok(())
        } else {
            Err(format!(
                "Windows refused to launch it (error {}).",
                ret.0 as usize
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Linux: .desktop files (name + exec only; no icon extraction)
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn scan_all(_icons_dir: &Path) -> Vec<NativeProgram> {
    let mut dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    // Later dirs win, so the user's own .desktop files shadow system ones.
    let mut by_id: HashMap<String, NativeProgram> = HashMap::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(prog) = parse_desktop(&path) {
                by_id.insert(prog.id.clone(), prog);
            }
        }
    }
    let mut out: Vec<_> = by_id.into_values().collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

#[cfg(not(windows))]
fn parse_desktop(path: &Path) -> Option<NativeProgram> {
    let text = fs::read_to_string(path).ok()?;
    let mut name: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut nodisplay = false;
    let mut hidden = false;
    let mut is_app = false;
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            if name.is_none() {
                name = Some(v.trim().to_string());
            }
        } else if let Some(v) = line.strip_prefix("Exec=") {
            if exec.is_none() {
                exec = Some(v.trim().to_string());
            }
        } else if line == "NoDisplay=true" {
            nodisplay = true;
        } else if line == "Hidden=true" {
            hidden = true;
        } else if let Some(v) = line.strip_prefix("Type=") {
            is_app = v.trim() == "Application";
        }
    }
    if !is_app || nodisplay || hidden {
        return None;
    }
    let name = name.filter(|n| !n.is_empty())?;
    if exec.is_none_or(|e| e.trim().is_empty()) {
        return None;
    }
    let id = hash_str(&path.to_string_lossy().to_lowercase());
    Some(NativeProgram {
        id,
        name,
        exe_path: path.to_string_lossy().into_owned(),
        icon_path: None,
    })
}

/// Launch through the desktop environment so Terminal=/DBusActivatable and
/// friends are honored. `gio` first, `gtk-launch` as fallback.
#[cfg(not(windows))]
fn launch_native(desktop_path: &str) -> Result<(), String> {
    let via_gio = std::process::Command::new("gio")
        .arg("launch")
        .arg(desktop_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if via_gio {
        return Ok(());
    }
    let stem = Path::new(desktop_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let via_gtk = std::process::Command::new("gtk-launch")
        .arg(stem)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if via_gtk {
        Ok(())
    } else {
        Err("Could not launch it (neither gio nor gtk-launch worked).".to_string())
    }
}
