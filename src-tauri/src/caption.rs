//! Slim custom caption bars for page windows (v0.9.6, Windows only).
//!
//! Account windows and the search window load arbitrary external sites, so
//! they must never expose the Tauri IPC bridge (`withGlobalTauri: false`).
//! That rules out an HTML caption bar: `data-tauri-drag-region` and any
//! button both need `window.__TAURI_INTERNALS__`, which is absent by design.
//! Instead, each page window goes frameless (`decorations(false)`, keeping
//! its taskbar button, Alt+Space menu, and tao's emulated border resizing)
//! and gets a tiny native caption strip: one `WS_POPUP` HWND owned by the
//! page window, painted dark, carrying exactly one button (minimize). The
//! strip lives on a dedicated chrome thread with its own message pump.
//!
//! The same thread owns the close-gesture hook (WH_KEYBOARD_LL): on Esc
//! key-down it checks `GetAsyncKeyState(VK_LBUTTON)` — no second mouse hook
//! needed — and closes the window only when the foreground window is one of
//! ours (a page window or its caption). Esc alone, or Esc+LMB anywhere else,
//! passes through untouched. The hook installs with the first caption and
//! uninstalls with the last one: there is no session-wide hook while no
//! page window exists.
//!
//! The gesture classifier is pure logic (events in, close-or-not out) and is
//! unit-tested on every platform; only the Win32 machinery is cfg(windows).

/// One decoded close-gesture event.
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscGesture {
    /// An Esc key-down arrived (auto-repeat counts: the window is gone after
    /// the first close, so repeats are harmless no-ops).
    pub esc_down: bool,
    /// The left mouse button is physically held right now.
    pub lmb_held: bool,
    /// The foreground window is an account/search window (or its caption).
    /// A minimized window can never be foreground, so it can never match.
    pub foreground_is_page: bool,
}

/// True when the gesture should close the focused page window: Esc pressed
/// while the left button is held, with a page window in the foreground.
/// Every other combination passes through untouched.
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
pub fn esc_gesture_closes(g: EscGesture) -> bool {
    g.esc_down && g.lmb_held && g.foreground_is_page
}

#[cfg(windows)]
mod imp {
    use super::{esc_gesture_closes, EscGesture};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::mpsc;
    use std::sync::OnceLock;
    use tauri::{AppHandle, Manager, WebviewWindow};
    use windows::core::{w, PWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::*;
    use windows::Win32::UI::Controls::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    const VK_LBUTTON_I32: i32 = 0x01;
    const VK_ESCAPE_U32: u32 = 0x1B;
    /// Caption height and minimize-button width, logical pixels.
    const BAR_H_LOGICAL: f64 = 30.0;
    const BTN_W_LOGICAL: f64 = 46.0;

    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
    }

    enum ChromeCmd {
        AddCaption {
            owner: isize,
            label: String,
            scale: f64,
        },
        RemoveCaption {
            label: String,
        },
        Reposition {
            label: String,
        },
        ClosePage {
            label: String,
        },
    }

    struct Caption {
        hwnd: HWND,
        owner: HWND,
        scale: f64,
        hover_min: bool,
        pressed_min: bool,
        mouse_in: bool,
        /// True while we are moving the caption ourselves (from the owner's
        /// Moved/Resized events): WM_WINDOWPOSCHANGED must not echo the move
        /// back onto the owner.
        syncing: bool,
        last_x: i32,
        last_y: i32,
    }

    struct Chrome {
        app: AppHandle,
        tx: mpsc::Sender<ChromeCmd>,
        wake: HANDLE,
        hook: HHOOK,
        captions: HashMap<String, Caption>,
        /// caption HWND -> page label, for the Esc hook's foreground check.
        by_hwnd: HashMap<isize, String>,
    }

    // HANDLE is a raw pointer (not Send/Sync), but Wake is only ever passed
    // to SetEvent, which is thread-safe by contract.
    #[derive(Clone, Copy)]
    struct Wake(HANDLE);
    unsafe impl Send for Wake {}
    unsafe impl Sync for Wake {}

    thread_local! {
        static CHROME: RefCell<Option<Chrome>> = const { RefCell::new(None) };
    }

    static CHROME_CTL: OnceLock<(mpsc::Sender<ChromeCmd>, Wake)> = OnceLock::new();

    fn with_chrome<R>(f: impl FnOnce(&mut Chrome) -> Option<R>) -> Option<R> {
        CHROME.with(|c| c.borrow_mut().as_mut().and_then(f))
    }

    fn button_rect(hwnd: HWND, scale: f64) -> Option<RECT> {
        unsafe {
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc).ok()?;
            let bw = (BTN_W_LOGICAL * scale).round() as i32;
            Some(RECT {
                left: rc.right - bw,
                top: rc.top,
                right: rc.right,
                bottom: rc.bottom,
            })
        }
    }

    fn pt_in_rect(x: i32, y: i32, rc: &RECT) -> bool {
        x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
    }

    fn mouse_xy(lparam: LPARAM) -> (i32, i32) {
        (
            (lparam.0 & 0xffff) as i16 as i32,
            ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        )
    }

    // ------------------------------------------------------------------
    // Caption window proc
    // ------------------------------------------------------------------

    fn hbrush_to_obj(hbr: HBRUSH) -> HGDIOBJ {
        HGDIOBJ(hbr.0)
    }

    unsafe fn paint_caption(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            return;
        }
        let (scale, hover, pressed) = with_chrome(|ch| {
            let label = ch.by_hwnd.get(&(hwnd.0 as isize))?;
            let cp = ch.captions.get(label)?;
            Some((cp.scale, cp.hover_min, cp.pressed_min))
        })
        .unwrap_or((1.0, false, false));

        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let bg = CreateSolidBrush(rgb(27, 27, 27));
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(hbrush_to_obj(bg));

        if let Some(btn) = button_rect(hwnd, scale) {
            if hover || pressed {
                let bbg = CreateSolidBrush(if pressed {
                    rgb(46, 46, 46)
                } else {
                    rgb(58, 58, 58)
                });
                FillRect(hdc, &btn, bbg);
                let _ = DeleteObject(hbrush_to_obj(bbg));
            }
            // Minimize glyph: a small horizontal bar, centered.
            let gw = (10.0 * scale).round() as i32;
            let gh = (2.0 * scale).max(1.0).round() as i32;
            let bw = btn.right - btn.left;
            let bh = btn.bottom - btn.top;
            let grc = RECT {
                left: btn.left + (bw - gw) / 2,
                top: btn.top + (bh - gh) / 2,
                right: btn.left + (bw - gw) / 2 + gw,
                bottom: btn.top + (bh - gh) / 2 + gh,
            };
            let glyph = CreateSolidBrush(if hover || pressed {
                rgb(255, 255, 255)
            } else {
                rgb(204, 204, 204)
            });
            FillRect(hdc, &grc, glyph);
            let _ = DeleteObject(hbrush_to_obj(glyph));
        }
        let _ = EndPaint(hwnd, &ps);
    }

    unsafe fn on_lbutton_down(hwnd: HWND, lparam: LPARAM) {
        let (x, y) = mouse_xy(lparam);
        with_chrome(|ch| {
            let label = ch.by_hwnd.get(&(hwnd.0 as isize))?.clone();
            let cp = ch.captions.get_mut(&label)?;
            let over_button = button_rect(hwnd, cp.scale).is_some_and(|b| pt_in_rect(x, y, &b));
            if over_button {
                cp.pressed_min = true;
                SetCapture(hwnd);
            } else {
                // Native drag of the caption strip; the owner follows via
                // WM_WINDOWPOSCHANGED below.
                let _ = ReleaseCapture();
                let _ = SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            Some(())
        });
    }

    unsafe fn on_lbutton_up(hwnd: HWND, lparam: LPARAM) {
        let (x, y) = mouse_xy(lparam);
        with_chrome(|ch| {
            let label = ch.by_hwnd.get(&(hwnd.0 as isize))?.clone();
            let cp = ch.captions.get_mut(&label)?;
            if cp.pressed_min {
                cp.pressed_min = false;
                if GetCapture() == hwnd {
                    let _ = ReleaseCapture();
                }
                let over_button =
                    button_rect(hwnd, cp.scale).is_some_and(|b| pt_in_rect(x, y, &b));
                if over_button {
                    // The owned caption hides with its owner automatically.
                    let _ = PostMessageW(
                        Some(cp.owner),
                        WM_SYSCOMMAND,
                        WPARAM(SC_MINIMIZE as usize),
                        LPARAM(0),
                    );
                }
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            Some(())
        });
    }

    unsafe fn on_mouse_move(hwnd: HWND, lparam: LPARAM) {
        let (x, y) = mouse_xy(lparam);
        with_chrome(|ch| {
            let label = ch.by_hwnd.get(&(hwnd.0 as isize))?.clone();
            let cp = ch.captions.get_mut(&label)?;
            if !cp.mouse_in {
                cp.mouse_in = true;
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            let hover =
                button_rect(hwnd, cp.scale).is_some_and(|b| pt_in_rect(x, y, &b));
            if hover != cp.hover_min {
                cp.hover_min = hover;
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            Some(())
        });
    }

    unsafe fn on_mouse_leave(hwnd: HWND) {
        with_chrome(|ch| {
            let label = ch.by_hwnd.get(&(hwnd.0 as isize))?.clone();
            let cp = ch.captions.get_mut(&label)?;
            cp.mouse_in = false;
            cp.hover_min = false;
            let _ = InvalidateRect(Some(hwnd), None, false);
            Some(())
        });
    }

    /// The caption moved (user drag): move the owner by the same delta,
    /// keeping the strip pinned on-screen (it is the window's only drag
    /// handle and its only minimize button).
    unsafe fn on_pos_changed(hwnd: HWND) {
        with_chrome(|ch| {
            let label = ch.by_hwnd.get(&(hwnd.0 as isize))?.clone();
            let cp = ch.captions.get_mut(&label)?;
            if cp.syncing {
                return Some(());
            }
            let mut rc = RECT::default();
            GetWindowRect(hwnd, &mut rc).ok()?;
            if rc.top < 0 {
                cp.syncing = true;
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rc.left,
                    0,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                cp.syncing = false;
                GetWindowRect(hwnd, &mut rc).ok()?;
            }
            let (dx, dy) = (rc.left - cp.last_x, rc.top - cp.last_y);
            if dx != 0 || dy != 0 {
                cp.last_x = rc.left;
                cp.last_y = rc.top;
                let mut orc = RECT::default();
                if GetWindowRect(cp.owner, &mut orc).is_ok() {
                    let _ = SetWindowPos(
                        cp.owner,
                        None,
                        orc.left + dx,
                        orc.top + dy,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            Some(())
        });
    }

    unsafe fn on_dpi_changed(hwnd: HWND, wparam: WPARAM) {
        let dpi = (wparam.0 & 0xffff) as u32;
        if dpi == 0 {
            return;
        }
        with_chrome(|ch| {
            let label = ch.by_hwnd.get(&(hwnd.0 as isize))?.clone();
            {
                let cp = ch.captions.get_mut(&label)?;
                cp.scale = dpi as f64 / 96.0;
            }
            reposition_caption(ch, &label);
            Some(())
        });
    }

    unsafe extern "system" fn caption_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_PAINT => {
                paint_caption(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                on_lbutton_down(hwnd, lparam);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                on_lbutton_up(hwnd, lparam);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                on_mouse_move(hwnd, lparam);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                on_mouse_leave(hwnd);
                LRESULT(0)
            }
            WM_WINDOWPOSCHANGED => {
                on_pos_changed(hwnd);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                on_dpi_changed(hwnd, wparam);
                LRESULT(0)
            }
            WM_DESTROY => {
                // The owner is going away (owned windows die with it): drop
                // the bookkeeping so a later RemoveCaption is a no-op.
                with_chrome(|ch| {
                    if let Some(label) = ch.by_hwnd.remove(&(hwnd.0 as isize)) {
                        ch.captions.remove(&label);
                    }
                    maybe_uninstall_esc_hook(ch);
                    Some(())
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn register_class() {
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(caption_proc),
            hInstance: HINSTANCE(hinstance.0),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("AppMakaCaption"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }

    /// Quiet discoverability for the close gesture: a standard tooltip over
    /// the strip. TTF_SUBCLASS relays the mouse messages for us.
    unsafe fn create_tooltip(parent: HWND) {
        let tip = match CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            None,
            None,
        ) {
            Ok(h) => h,
            Err(_) => return,
        };
        let text = "Hold the left mouse button and press Esc to close this window.";
        let mut wide: Vec<u16> = OsStr::new(text).encode_wide().chain(std::iter::once(0)).collect();
        let mut rc = RECT::default();
        let _ = GetClientRect(parent, &mut rc);
        let mut ti = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_SUBCLASS | TTF_IDISHWND,
            hwnd: parent,
            uId: parent.0 as usize,
            rect: rc,
            hinst: HINSTANCE::default(),
            lpszText: PWSTR(wide.as_mut_ptr()),
            lParam: LPARAM(0),
            lpReserved: std::ptr::null_mut(),
        };
        SendMessageW(
            tip,
            TTM_ADDTOOLW,
            Some(WPARAM(0)),
            Some(LPARAM(&mut ti as *mut TTTOOLINFOW as isize)),
        );
        SendMessageW(tip, TTM_SETMAXTIPWIDTH, Some(WPARAM(0)), Some(LPARAM(320)));
    }

    /// Place the strip directly above its owner. A maximized owner gets no
    /// strip (it would sit off-screen); it reappears on restore.
    unsafe fn reposition_caption(ch: &mut Chrome, label: &str) {
        let cp = match ch.captions.get_mut(label) {
            Some(c) => c,
            None => return,
        };
        if !IsWindow(Some(cp.owner)).as_bool() {
            return;
        }
        if IsZoomed(cp.owner).as_bool() {
            let _ = ShowWindow(cp.hwnd, SW_HIDE);
            return;
        }
        let _ = ShowWindow(cp.hwnd, SW_SHOWNOACTIVATE);
        let mut orc = RECT::default();
        if GetWindowRect(cp.owner, &mut orc).is_err() {
            return;
        }
        let h = (BAR_H_LOGICAL * cp.scale).round() as i32;
        let w = orc.right - orc.left;
        let (tx, ty) = (orc.left, orc.top - h);
        // Never strand the strip off the top of the screen: nudge the
        // owner down so the strip fits. The owner's Moved event re-enters
        // here and then stabilizes (ty becomes 0).
        let (tx, ty) = if ty < 0 {
            let _ = SetWindowPos(
                cp.owner,
                None,
                orc.left,
                h,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let mut orc2 = RECT::default();
            if GetWindowRect(cp.owner, &mut orc2).is_err() {
                return;
            }
            (orc2.left, orc2.top - h)
        } else {
            (tx, ty)
        };
        let mut crc = RECT::default();
        let same = GetWindowRect(cp.hwnd, &mut crc).is_ok()
            && crc.left == tx
            && crc.top == ty
            && crc.right - crc.left == w
            && crc.bottom - crc.top == h;
        if same {
            cp.last_x = tx;
            cp.last_y = ty;
            return;
        }
        cp.syncing = true;
        let _ = SetWindowPos(cp.hwnd, None, tx, ty, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        cp.syncing = false;
        cp.last_x = tx;
        cp.last_y = ty;
    }

    unsafe fn create_caption(ch: &mut Chrome, owner: HWND, label: &str, scale: f64) {
        if ch.captions.contains_key(label) {
            reposition_caption(ch, label);
            return;
        }
        let h = (BAR_H_LOGICAL * scale).round() as i32;
        // WS_POPUP with an owner HWND: an *owned* window — always above its
        // owner in z-order, hidden with it on minimize, no taskbar button.
        // WS_EX_NOACTIVATE keeps page focus when the strip is clicked.
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("AppMakaCaption"),
            w!(""),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            100,
            h,
            Some(owner),
            None,
            None,
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(e) => {
                eprintln!("[appmaka] caption: couldn't create strip: {e}");
                return;
            }
        };
        create_tooltip(hwnd);
        ch.by_hwnd.insert(hwnd.0 as isize, label.to_string());
        ch.captions.insert(
            label.to_string(),
            Caption {
                hwnd,
                owner,
                scale,
                hover_min: false,
                pressed_min: false,
                mouse_in: false,
                syncing: false,
                last_x: 0,
                last_y: 0,
            },
        );
        reposition_caption(ch, label);
        ensure_esc_hook(ch);
    }

    unsafe fn remove_caption(ch: &mut Chrome, label: &str) {
        if let Some(cp) = ch.captions.remove(label) {
            ch.by_hwnd.remove(&(cp.hwnd.0 as isize));
            if IsWindow(Some(cp.hwnd)).as_bool() {
                let _ = DestroyWindow(cp.hwnd);
            }
        }
        maybe_uninstall_esc_hook(ch);
    }

    // ------------------------------------------------------------------
    // Esc+LMB close gesture: WH_KEYBOARD_LL on the chrome thread
    // ------------------------------------------------------------------

    unsafe fn ensure_esc_hook(ch: &mut Chrome) {
        if ch.hook.is_invalid() {
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(esc_proc), None, 0) {
                Ok(hook) => ch.hook = hook,
                Err(e) => eprintln!("[appmaka] caption: Esc-gesture hook failed: {e}"),
            }
        }
    }

    unsafe fn maybe_uninstall_esc_hook(ch: &mut Chrome) {
        if ch.captions.is_empty() && !ch.hook.is_invalid() {
            let _ = UnhookWindowsHookEx(ch.hook);
            ch.hook = HHOOK::default();
        }
    }

    /// Foreground HWND -> page label, if it is one of ours: a caption strip
    /// resolves to its page, a page window to itself.
    fn resolve_page_label(fg: HWND) -> Option<String> {
        CHROME.with(|c| {
            let ch = c.borrow();
            let ch = ch.as_ref()?;
            if let Some(label) = ch.by_hwnd.get(&(fg.0 as isize)) {
                return Some(label.clone());
            }
            ch.captions
                .iter()
                .find(|(_, cp)| cp.owner == fg)
                .map(|(label, _)| label.clone())
        })
    }

    unsafe extern "system" fn esc_proc(n_code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if n_code >= 0 {
            let w = wparam.0 as u32;
            if w == WM_KEYDOWN || w == WM_SYSKEYDOWN {
                let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
                if kb.vkCode == VK_ESCAPE_U32 {
                    let lmb_held = (GetAsyncKeyState(VK_LBUTTON_I32) as u16 & 0x8000) != 0;
                    let label = resolve_page_label(GetForegroundWindow());
                    let g = EscGesture {
                        esc_down: true,
                        lmb_held,
                        foreground_is_page: label.is_some(),
                    };
                    if esc_gesture_closes(g) {
                        if let Some(label) = label {
                            CHROME.with(|c| {
                                if let Some(ch) = c.borrow().as_ref() {
                                    let _ = ch.tx.send(ChromeCmd::ClosePage { label });
                                    let _ = SetEvent(ch.wake);
                                }
                            });
                        }
                        // Swallow: the page never sees the Esc that closed it.
                        return LRESULT(1);
                    }
                }
            }
        }
        CallNextHookEx(None, n_code, wparam, lparam)
    }

    // ------------------------------------------------------------------
    // Chrome thread
    // ------------------------------------------------------------------

    fn chrome_thread_main(
        app: AppHandle,
        tx: mpsc::Sender<ChromeCmd>,
        rx: mpsc::Receiver<ChromeCmd>,
        wake: HANDLE,
    ) {
        unsafe { register_class() };
        CHROME.with(|c| {
            *c.borrow_mut() = Some(Chrome {
                app,
                tx,
                wake,
                hook: HHOOK::default(),
                captions: HashMap::new(),
                by_hwnd: HashMap::new(),
            });
        });

        let handles = [wake];
        loop {
            let waited = unsafe {
                MsgWaitForMultipleObjectsEx(
                    Some(&handles),
                    INFINITE,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                )
            };
            if waited == WAIT_FAILED {
                eprintln!("[appmaka] caption: message wait failed; chrome thread exiting");
                break;
            }
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    ChromeCmd::AddCaption { owner, label, scale } => unsafe {
                        with_chrome(|ch| {
                            create_caption(ch, HWND(owner as *mut _), &label, scale);
                            Some(())
                        });
                    },
                    ChromeCmd::RemoveCaption { label } => unsafe {
                        with_chrome(|ch| {
                            remove_caption(ch, &label);
                            Some(())
                        });
                    },
                    ChromeCmd::Reposition { label } => unsafe {
                        with_chrome(|ch| {
                            reposition_caption(ch, &label);
                            Some(())
                        });
                    },
                    ChromeCmd::ClosePage { label } => {
                        let closed = CHROME.with(|c| {
                            c.borrow()
                                .as_ref()
                                .and_then(|ch| ch.app.get_webview_window(&label))
                                .map(|w| w.close().is_ok())
                                .unwrap_or(false)
                        });
                        let _ = closed;
                    }
                }
            }
            // Pump messages: caption paint/mouse traffic and the hook proc
            // both run on this thread.
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }

    fn ensure_chrome(app: &AppHandle) -> Option<(mpsc::Sender<ChromeCmd>, Wake)> {
        let app_c = app.clone();
        Some(
            CHROME_CTL
                .get_or_init(move || {
                    let (tx, rx) = mpsc::channel::<ChromeCmd>();
                    let wake = unsafe { CreateEventW(None, false, false, None) }
                        .unwrap_or_else(|_| HANDLE::default());
                    let tx_thread = tx.clone();
                    let wake_thread = Wake(wake);
                    let wake_bits = wake.0 as isize;
                    let _ = std::thread::Builder::new()
                        .name("appmaka-caption".to_string())
                        .spawn(move || {
                            chrome_thread_main(
                                app_c,
                                tx_thread,
                                rx,
                                HANDLE(wake_bits as *mut _),
                            )
                        });
                    (tx, wake_thread)
                })
                .clone(),
        )
    }

    /// A page window (account or search) was created: give it a caption
    /// strip. Idempotent per label. Safe to call from any thread.
    pub fn page_window_opened(app: &AppHandle, label: &str, window: &WebviewWindow) {
        let Ok(hwnd) = window.hwnd() else { return };
        let scale = window.scale_factor().unwrap_or(1.0);
        let Some((tx, wake)) = ensure_chrome(app) else {
            return;
        };
        let _ = tx.send(ChromeCmd::AddCaption {
            owner: hwnd.0 as isize,
            label: label.to_string(),
            scale,
        });
        unsafe {
            let _ = SetEvent(wake.0);
        }
    }

    /// The page window moved or resized: re-seat its strip above it.
    pub fn page_window_moved(label: &str) {
        if let Some((tx, wake)) = CHROME_CTL.get() {
            let _ = tx.send(ChromeCmd::Reposition {
                label: label.to_string(),
            });
            unsafe {
                let _ = SetEvent(wake.0);
            }
        }
    }

    /// The page window is gone: destroy its strip (no-op if Windows already
    /// took the owned window down with its owner).
    pub fn page_window_closed(label: &str) {
        if let Some((tx, wake)) = CHROME_CTL.get() {
            let _ = tx.send(ChromeCmd::RemoveCaption {
                label: label.to_string(),
            });
            unsafe {
                let _ = SetEvent(wake.0);
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{page_window_closed, page_window_moved, page_window_opened};

#[cfg(not(windows))]
pub fn page_window_opened(
    _app: &tauri::AppHandle,
    _label: &str,
    _window: &tauri::WebviewWindow,
) {
    // Linux keeps native decorations: no caption strips, no gesture hook.
}

#[cfg(not(windows))]
pub fn page_window_moved(_label: &str) {}

#[cfg(not(windows))]
pub fn page_window_closed(_label: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn gesture(esc_down: bool, lmb_held: bool, foreground_is_page: bool) -> EscGesture {
        EscGesture {
            esc_down,
            lmb_held,
            foreground_is_page,
        }
    }

    #[test]
    fn esc_plus_held_lmb_on_page_closes() {
        assert!(esc_gesture_closes(gesture(true, true, true)));
    }

    #[test]
    fn esc_alone_never_closes() {
        // Esc without the held button: page keeps its own Esc handling
        // (dialogs, menus, blur).
        assert!(!esc_gesture_closes(gesture(true, false, true)));
    }

    #[test]
    fn lmb_held_but_foreground_is_not_a_page_never_closes() {
        // The launcher, Settings, and the clipboard popup keep their Esc
        // behaviors: holding LMB there and pressing Esc does nothing new.
        assert!(!esc_gesture_closes(gesture(true, true, false)));
    }

    #[test]
    fn no_esc_event_never_closes() {
        // Other keys (or key-up) with LMB held: untouched.
        assert!(!esc_gesture_closes(gesture(false, true, true)));
        assert!(!esc_gesture_closes(gesture(false, false, false)));
    }

    #[test]
    fn minimized_window_cannot_match() {
        // A minimized window can never be the foreground window, so the
        // classifier's foreground gate already excludes it: it arrives here
        // as foreground_is_page = false and passes through.
        assert!(!esc_gesture_closes(gesture(true, true, false)));
    }

    #[test]
    fn fullscreen_page_still_closes() {
        // Fullscreen keeps the page in the foreground, so the gesture
        // applies there too — documented, not special-cased.
        assert!(esc_gesture_closes(gesture(true, true, true)));
    }
}
