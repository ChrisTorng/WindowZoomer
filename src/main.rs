#![windows_subsystem = "windows"]
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use chrono::Local;
use image::{ImageBuffer, Rgba};
use windows::core::{PCWSTR, PWSTR, Result as WinResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetSysColorBrush, IntersectClipRect, InvalidateRect, PatBlt, RestoreDC,
    SaveDC, ScreenToClient, SetStretchBltMode, StretchDIBits, UpdateWindow, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLACKNESS, COLORONCOLOR, COLOR_BTNFACE, DIB_RGB_COLORS,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, INITCOMMONCONTROLSEX, ICC_WIN95_CLASSES, TOOLTIPS_CLASSW,
    TTM_ADDTOOLW, TTS_ALWAYSTIP,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_ADD, VK_CONTROL, VK_DOWN, VK_LEFT,
    VK_OEM_MINUS, VK_OEM_PLUS, VK_RIGHT, VK_SHIFT, VK_SUBTRACT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetForegroundWindow, GetMessageW, GetWindowPlacement, IsChild, IsWindow, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetTimer, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    GWLP_USERDATA, HMENU, IDC_ARROW, MSG, SW_SHOW, SW_SHOWMAXIMIZED, WINDOW_EX_STYLE,
    WINDOWPLACEMENT, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SIZE, WM_TIMER,
    WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
};
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

const CLASS_NAME: &str = "WindowZoomerMainWindow";
const APP_TITLE: &str = "WindowZoomer";
const TOOLBAR_H: i32 = 42;
const TIMER_FOREGROUND: usize = 1;
const FOREGROUND_POLL_MS: u32 = 100;
const WM_FRAME_READY: u32 = WM_APP + 1;

const ID_ZOOM_IN: usize = 1001;
const ID_ZOOM_OUT: usize = 1002;
const ID_100: usize = 1003;
const ID_FIT: usize = 1004;
const ID_SCREENSHOT: usize = 1005;
const ID_FOLDER: usize = 1006;
const ID_LAST: usize = 1007;

const TTF_IDISHWND_RAW: u32 = 0x0001;
const TTF_SUBCLASS_RAW: u32 = 0x0010;

#[repr(C)]
struct ToolInfoW {
    cb_size: u32,
    u_flags: u32,
    hwnd: HWND,
    u_id: usize,
    rect: RECT,
    hinst: HINSTANCE,
    lpsz_text: PWSTR,
    l_param: LPARAM,
    lp_reserved: *mut c_void,
}

#[derive(Default)]
struct FrameData {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

#[derive(Clone)]
struct CaptureFlags {
    shared: Arc<Mutex<FrameData>>,
    notify_hwnd: isize,
}

struct CaptureHandler {
    flags: CaptureFlags,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = CaptureFlags;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            flags: ctx.flags,
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame<'_>,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let mut buffer = frame.buffer().map_err(|e| e.to_string())?;
        let width = buffer.width();
        let height = buffer.height();
        let bytes: &[u8] = if buffer.has_padding() {
            buffer.as_nopadding_buffer(&mut self.scratch)
        } else {
            buffer.as_raw_buffer()
        };

        {
            let mut dst = self
                .flags
                .shared
                .lock()
                .map_err(|_| "frame mutex poisoned".to_string())?;
            dst.width = width;
            dst.height = height;
            dst.pixels.clear();
            dst.pixels.extend_from_slice(bytes);
        }

        let hwnd = HWND(self.flags.notify_hwnd as *mut c_void);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_FRAME_READY, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ZoomMode {
    Fit,
    Absolute(f64),
}

struct AppState {
    hwnd: HWND,
    shared: Arc<Mutex<FrameData>>,
    capture: Option<CaptureControl<CaptureHandler, String>>,
    target: Option<Window>,
    zoom_mode: ZoomMode,
    pan_x: f64,
    pan_y: f64,
    dragging: bool,
    drag_last_x: i32,
    drag_last_y: i32,
    last_screenshot: Option<PathBuf>,
}

impl AppState {
    fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            shared: Arc::new(Mutex::new(FrameData::default())),
            capture: None,
            target: None,
            zoom_mode: ZoomMode::Fit,
            pan_x: 0.0,
            pan_y: 0.0,
            dragging: false,
            drag_last_x: 0,
            drag_last_y: 0,
            last_screenshot: None,
        }
    }

    fn current_frame_size(&self) -> Option<(u32, u32)> {
        let frame = self.shared.lock().ok()?;
        (frame.width > 0 && frame.height > 0).then_some((frame.width, frame.height))
    }

    fn canvas_rect(&self) -> RECT {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        rc.top = TOOLBAR_H.min(rc.bottom);
        rc
    }

    fn fit_scale(&self, canvas: RECT, fw: u32, fh: u32) -> f64 {
        let cw = (canvas.right - canvas.left).max(1) as f64;
        let ch = (canvas.bottom - canvas.top).max(1) as f64;
        (cw / fw.max(1) as f64).min(ch / fh.max(1) as f64)
    }

    fn effective_scale(&self, canvas: RECT, fw: u32, fh: u32) -> f64 {
        match self.zoom_mode {
            ZoomMode::Fit => self.fit_scale(canvas, fw, fh),
            ZoomMode::Absolute(scale) => scale,
        }
    }

    fn current_scale(&self) -> Option<f64> {
        let (fw, fh) = self.current_frame_size()?;
        Some(self.effective_scale(self.canvas_rect(), fw, fh))
    }

    fn clamp_pan(&mut self) {
        let Some((fw, fh)) = self.current_frame_size() else {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
            return;
        };
        let canvas = self.canvas_rect();
        let cw = (canvas.right - canvas.left).max(1) as f64;
        let ch = (canvas.bottom - canvas.top).max(1) as f64;
        let scale = self.effective_scale(canvas, fw, fh);
        let max_x = ((fw as f64 * scale - cw) / 2.0).max(0.0);
        let max_y = ((fh as f64 * scale - ch) / 2.0).max(0.0);
        self.pan_x = self.pan_x.clamp(-max_x, max_x);
        self.pan_y = self.pan_y.clamp(-max_y, max_y);
    }

    fn repaint(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn set_fit(&mut self) {
        self.zoom_mode = ZoomMode::Fit;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.refresh_title();
        self.repaint();
    }

    fn set_100(&mut self) {
        self.set_scale_centered(1.0);
    }

    fn set_scale_centered(&mut self, scale: f64) {
        let canvas = self.canvas_rect();
        let x = ((canvas.left + canvas.right) / 2) as f64;
        let y = ((canvas.top + canvas.bottom) / 2) as f64;
        self.set_scale_at(scale, x, y);
    }

    fn set_scale_at(&mut self, new_scale: f64, px: f64, py: f64) {
        let Some((fw, fh)) = self.current_frame_size() else {
            return;
        };
        let canvas = self.canvas_rect();
        let old_scale = self.effective_scale(canvas, fw, fh);
        if old_scale <= 0.0 {
            return;
        }

        let cw = (canvas.right - canvas.left).max(1) as f64;
        let ch = (canvas.bottom - canvas.top).max(1) as f64;
        let old_left =
            canvas.left as f64 + (cw - fw as f64 * old_scale) / 2.0 + self.pan_x;
        let old_top =
            canvas.top as f64 + (ch - fh as f64 * old_scale) / 2.0 + self.pan_y;
        let sx = (px - old_left) / old_scale;
        let sy = (py - old_top) / old_scale;

        let scale = new_scale.clamp(0.10, 32.0);
        self.zoom_mode = ZoomMode::Absolute(scale);

        let base_left = canvas.left as f64 + (cw - fw as f64 * scale) / 2.0;
        let base_top = canvas.top as f64 + (ch - fh as f64 * scale) / 2.0;
        self.pan_x = px - base_left - sx * scale;
        self.pan_y = py - base_top - sy * scale;
        self.clamp_pan();
        self.refresh_title();
        self.repaint();
    }

    fn wheel_zoom_at(&mut self, delta: i32, px: f64, py: f64) {
        let Some(current) = self.current_scale() else {
            return;
        };
        // 5% per standard wheel notch, proportional for precision touchpads.
        let factor = 1.05_f64.powf(delta as f64 / 120.0);
        self.set_scale_at(current * factor, px, py);
    }

    fn step_zoom(&mut self, direction: i32) {
        let Some((fw, fh)) = self.current_frame_size() else {
            return;
        };
        let canvas = self.canvas_rect();
        let current = self.effective_scale(canvas, fw, fh);
        let fit = self.fit_scale(canvas, fw, fh);

        let mut candidates: Vec<f64> = (1..=128).map(|n| n as f64 * 0.25).collect();
        if (0.10..=32.0).contains(&fit) {
            candidates.push(fit);
        }
        candidates.sort_by(|a, b| a.total_cmp(b));
        candidates.dedup_by(|a, b| (*a - *b).abs() < 0.0005);

        let epsilon = 0.002;
        let next = if direction > 0 {
            candidates.into_iter().find(|v| *v > current + epsilon)
        } else {
            candidates
                .into_iter()
                .rev()
                .find(|v| *v < current - epsilon)
        };

        if let Some(scale) = next {
            self.set_scale_centered(scale);
        }
    }

    fn pan_viewport(&mut self, dx: f64, dy: f64) {
        self.pan_x += dx;
        self.pan_y += dy;
        self.clamp_pan();
        self.repaint();
    }

    fn target_is_ours(&self, hwnd: HWND) -> bool {
        if hwnd.0.is_null() || hwnd == self.hwnd {
            return true;
        }
        unsafe { IsChild(self.hwnd, hwnd).as_bool() }
    }

    fn switch_target(&mut self, hwnd: HWND) {
        if self.target_is_ours(hwnd) {
            return;
        }

        let window = Window::from_raw_hwnd(hwnd.0);
        if !window.is_valid() || self.target.as_ref() == Some(&window) {
            return;
        }

        if let Some(old) = self.capture.take() {
            let _ = old.stop();
        }

        let settings = Settings::new(
            window,
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            CaptureFlags {
                shared: Arc::clone(&self.shared),
                notify_hwnd: self.hwnd.0 as isize,
            },
        );

        if let Ok(control) = CaptureHandler::start_free_threaded(settings) {
            self.capture = Some(control);
            self.target = Some(window);
            self.zoom_mode = ZoomMode::Fit;
            self.pan_x = 0.0;
            self.pan_y = 0.0;
            if let Ok(mut frame) = self.shared.lock() {
                frame.pixels.clear();
                frame.width = 0;
                frame.height = 0;
            }
            self.refresh_title();
            self.repaint();
        }
    }

    fn refresh_title(&self) {
        let target_title = self
            .target
            .and_then(|w| w.title().ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "No target".to_string());

        let scale = self.current_scale().unwrap_or(1.0);
        let fit_suffix = if matches!(self.zoom_mode, ZoomMode::Fit) {
            " · Fit"
        } else {
            ""
        };
        let title = format!(
            "{APP_TITLE} — {:.1}%{fit_suffix} — {target_title}",
            scale * 100.0
        );
        let wide = wide_null(&title);
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr()));
        }
    }

    fn save_screenshot(&mut self) {
        let Some((fw, fh)) = self.current_frame_size() else {
            return;
        };
        let canvas = self.canvas_rect();
        let cw = (canvas.right - canvas.left).max(1) as u32;
        let ch = (canvas.bottom - canvas.top).max(1) as u32;
        let scale = self.effective_scale(canvas, fw, fh);

        let frame = match self.shared.lock() {
            Ok(f) => f,
            Err(_) => return,
        };
        if frame.pixels.len() < fw as usize * fh as usize * 4 {
            return;
        }

        let left = (cw as f64 - fw as f64 * scale) / 2.0 + self.pan_x;
        let top = (ch as f64 - fh as f64 * scale) / 2.0 + self.pan_y;
        let mut out = vec![0u8; cw as usize * ch as usize * 4];
        for px in out.chunks_exact_mut(4) {
            px[3] = 255;
        }

        for y in 0..ch {
            let sy = ((y as f64 - top) / scale).floor() as i64;
            if sy < 0 || sy >= fh as i64 {
                continue;
            }
            for x in 0..cw {
                let sx = ((x as f64 - left) / scale).floor() as i64;
                if sx < 0 || sx >= fw as i64 {
                    continue;
                }
                let src = (sy as usize * fw as usize + sx as usize) * 4;
                let dst = (y as usize * cw as usize + x as usize) * 4;
                out[dst] = frame.pixels[src + 2];
                out[dst + 1] = frame.pixels[src + 1];
                out[dst + 2] = frame.pixels[src];
                out[dst + 3] = 255;
            }
        }
        drop(frame);

        let Some(image) = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(cw, ch, out) else {
            return;
        };
        let Some(path) = next_screenshot_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if image.save(&path).is_ok() {
            self.last_screenshot = Some(path);
        }
    }

    fn open_screenshot_folder(&self) {
        if let Some(dir) = screenshot_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = Command::new("explorer.exe").arg(dir).spawn();
        }
    }

    fn reveal_last_screenshot(&self) {
        if let Some(path) = self.last_screenshot.as_ref().filter(|p| p.exists()) {
            let arg = format!("/select,\"{}\"", path.display());
            let _ = Command::new("explorer.exe").arg(arg).spawn();
        }
    }
}

#[derive(Clone, Copy)]
struct WindowState {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    maximized: bool,
}

fn config_file() -> Option<PathBuf> {
    dirs::config_local_dir()
        .or_else(dirs::config_dir)
        .map(|p| p.join("WindowZoomer").join("window_state.txt"))
}

fn load_window_state() -> Option<WindowState> {
    let text = std::fs::read_to_string(config_file()?).ok()?;
    let mut it = text.split_whitespace();
    Some(WindowState {
        x: it.next()?.parse().ok()?,
        y: it.next()?.parse().ok()?,
        width: it.next()?.parse().ok()?,
        height: it.next()?.parse().ok()?,
        maximized: it.next()? == "1",
    })
}

fn save_window_state(hwnd: HWND) {
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetWindowPlacement(hwnd, &mut placement).is_err() {
            return;
        }
    }
    let r = placement.rcNormalPosition;
    let Some(path) = config_file() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let maximized = placement.showCmd == SW_SHOWMAXIMIZED.0 as u32;
    let text = format!(
        "{} {} {} {} {}",
        r.left,
        r.top,
        (r.right - r.left).max(400),
        (r.bottom - r.top).max(300),
        if maximized { 1 } else { 0 }
    );
    let _ = std::fs::write(path, text);
}

fn screenshot_dir() -> Option<PathBuf> {
    dirs::picture_dir()
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Pictures"))
        })
        .map(|p| p.join("Screenshots"))
}

fn next_screenshot_path() -> Option<PathBuf> {
    let dir = screenshot_dir()?;
    let timestamp = Local::now().format("%Y-%m-%d %H%M%S").to_string();
    let base = dir.join(format!("Screenshot {timestamp}.png"));
    if !base.exists() {
        return Some(base);
    }
    for n in 2..1000 {
        let candidate = dir.join(format!("Screenshot {timestamp} ({n}).png"));
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn state_mut(hwnd: HWND) -> Option<&'static mut AppState> {
    let ptr =
        windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    (ptr != 0).then(|| &mut *(ptr as *mut AppState))
}

fn loword(v: usize) -> u16 {
    (v & 0xffff) as u16
}

fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xffff) as u16
}

fn signed_word(v: u16) -> i16 {
    v as i16
}

fn ctrl_down() -> bool {
    unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 }
}

fn shift_down() -> bool {
    unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            if let Some(state) = state_mut(hwnd) {
                match loword(wparam.0) as usize {
                    ID_ZOOM_IN => state.step_zoom(1),
                    ID_ZOOM_OUT => state.step_zoom(-1),
                    ID_100 => state.set_100(),
                    ID_FIT => state.set_fit(),
                    ID_SCREENSHOT => state.save_screenshot(),
                    ID_FOLDER => state.open_screenshot_folder(),
                    ID_LAST => state.reveal_last_screenshot(),
                    _ => {}
                }
                let _ = SetFocus(Some(hwnd));
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_FOREGROUND {
                if let Some(state) = state_mut(hwnd) {
                    let fg = GetForegroundWindow();
                    if !fg.0.is_null() && !state.target_is_ours(fg) {
                        state.switch_target(fg);
                    } else if let Some(target) = state.target {
                        let target_hwnd = HWND(target.as_raw_hwnd());
                        if !IsWindow(Some(target_hwnd)).as_bool() {
                            if let Some(c) = state.capture.take() {
                                let _ = c.stop();
                            }
                            state.target = None;
                            state.refresh_title();
                            state.repaint();
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_FRAME_READY => {
            if let Some(state) = state_mut(hwnd) {
                state.clamp_pan();
                state.refresh_title();
                state.repaint();
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state_mut(hwnd) {
                state.clamp_pan();
                state.refresh_title();
                state.repaint();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if let Some(state) = state_mut(hwnd) {
                let key = wparam.0 as u16;
                let step = if shift_down() { 120.0 } else { 36.0 };
                match key {
                    k if ctrl_down() && k == b'S' as u16 => state.save_screenshot(),
                    k if ctrl_down() && shift_down() && k == b'O' as u16 => {
                        state.reveal_last_screenshot()
                    }
                    k if ctrl_down() && k == b'O' as u16 => state.open_screenshot_folder(),
                    k if k == VK_OEM_PLUS.0 || k == VK_ADD.0 || k == b'=' as u16 => {
                        state.step_zoom(1)
                    }
                    k if k == VK_OEM_MINUS.0 || k == VK_SUBTRACT.0 => state.step_zoom(-1),
                    k if k == b'0' as u16 || k == b'F' as u16 => state.set_fit(),
                    k if k == b'1' as u16 => state.set_100(),
                    k if k == VK_LEFT.0 => state.pan_viewport(step, 0.0),
                    k if k == VK_RIGHT.0 => state.pan_viewport(-step, 0.0),
                    k if k == VK_UP.0 => state.pan_viewport(0.0, step),
                    k if k == VK_DOWN.0 => state.pan_viewport(0.0, -step),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(state) = state_mut(hwnd) {
                let delta = signed_word(hiword(wparam.0)) as i32;
                let mut pt = windows::Win32::Foundation::POINT {
                    x: signed_word(loword(lparam.0 as usize)) as i32,
                    y: signed_word(hiword(lparam.0 as usize)) as i32,
                };
                let _ = ScreenToClient(hwnd, &mut pt);
                if pt.y >= TOOLBAR_H {
                    state.wheel_zoom_at(delta, pt.x as f64, pt.y as f64);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(state) = state_mut(hwnd) {
                let x = signed_word(loword(lparam.0 as usize)) as i32;
                let y = signed_word(hiword(lparam.0 as usize)) as i32;
                if y >= TOOLBAR_H {
                    state.dragging = true;
                    state.drag_last_x = x;
                    state.drag_last_y = y;
                    let _ = SetCapture(hwnd);
                    let _ = SetFocus(Some(hwnd));
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(state) = state_mut(hwnd) {
                if state.dragging {
                    let x = signed_word(loword(lparam.0 as usize)) as i32;
                    let y = signed_word(hiword(lparam.0 as usize)) as i32;
                    state.pan_x += (x - state.drag_last_x) as f64;
                    state.pan_y += (y - state.drag_last_y) as f64;
                    state.drag_last_x = x;
                    state.drag_last_y = y;
                    state.clamp_pan();
                    state.repaint();
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = state_mut(hwnd) {
                if state.dragging {
                    state.dragging = false;
                    let _ = ReleaseCapture();
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some(state) = state_mut(hwnd) {
                let canvas = state.canvas_rect();
                let cw = (canvas.right - canvas.left).max(0);
                let ch = (canvas.bottom - canvas.top).max(0);
                if cw > 0 && ch > 0 {
                    let saved = SaveDC(hdc);
                    let _ =
                        IntersectClipRect(hdc, canvas.left, canvas.top, canvas.right, canvas.bottom);
                    let _ = PatBlt(hdc, canvas.left, canvas.top, cw, ch, BLACKNESS);

                    if let Ok(frame) = state.shared.lock() {
                        if frame.width > 0
                            && frame.height > 0
                            && frame.pixels.len()
                                >= frame.width as usize * frame.height as usize * 4
                        {
                            let scale =
                                state.effective_scale(canvas, frame.width, frame.height);
                            let dw = (frame.width as f64 * scale).round() as i32;
                            let dh = (frame.height as f64 * scale).round() as i32;
                            let dx = canvas.left
                                + ((cw - dw) as f64 / 2.0 + state.pan_x).round() as i32;
                            let dy = canvas.top
                                + ((ch - dh) as f64 / 2.0 + state.pan_y).round() as i32;

                            let info = BITMAPINFO {
                                bmiHeader: BITMAPINFOHEADER {
                                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                                    biWidth: frame.width as i32,
                                    biHeight: -(frame.height as i32),
                                    biPlanes: 1,
                                    biBitCount: 32,
                                    biCompression: BI_RGB.0,
                                    ..Default::default()
                                },
                                ..Default::default()
                            };

                            let _ = SetStretchBltMode(hdc, COLORONCOLOR);
                            let _ = StretchDIBits(
                                hdc,
                                dx,
                                dy,
                                dw,
                                dh,
                                0,
                                0,
                                frame.width as i32,
                                frame.height as i32,
                                Some(frame.pixels.as_ptr() as *const c_void),
                                &info,
                                DIB_RGB_COLORS,
                                windows::Win32::Graphics::Gdi::SRCCOPY,
                            );
                        }
                    }
                    let _ = RestoreDC(hdc, saved);
                }
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CLOSE => {
            save_window_state(hwnd);
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr =
                windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ptr != 0 {
                let mut state = Box::from_raw(ptr as *mut AppState);
                if let Some(control) = state.capture.take() {
                    let _ = control.stop();
                }
                let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn create_button(
    parent: HWND,
    hinstance: HINSTANCE,
    id: usize,
    text: &str,
    x: i32,
    width: i32,
) -> WinResult<HWND> {
    let text = wide_null(text);
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("BUTTON"),
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_VISIBLE,
            x,
            6,
            width,
            30,
            Some(parent),
            Some(HMENU(id as *mut c_void)),
            Some(hinstance),
            None,
        )
    }
}

fn add_tooltip(tooltip: HWND, parent: HWND, control: HWND, text: &str) {
    let leaked: &'static mut [u16] = Box::leak(wide_null(text).into_boxed_slice());
    let mut info = ToolInfoW {
        cb_size: std::mem::size_of::<ToolInfoW>() as u32,
        u_flags: TTF_IDISHWND_RAW | TTF_SUBCLASS_RAW,
        hwnd: parent,
        u_id: control.0 as usize,
        rect: RECT::default(),
        hinst: HINSTANCE::default(),
        lpsz_text: PWSTR(leaked.as_mut_ptr()),
        l_param: LPARAM(0),
        lp_reserved: std::ptr::null_mut(),
    };
    unsafe {
        let _ = SendMessageW(
            tooltip,
            TTM_ADDTOOLW,
            Some(WPARAM(0)),
            Some(LPARAM((&mut info as *mut ToolInfoW) as isize)),
        );
    }
}

fn main() -> WinResult<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        };
        let _ = InitCommonControlsEx(&controls);
    }

    let initial_target = unsafe { GetForegroundWindow() };
    let saved = load_window_state();

    let class_name = wide_null(CLASS_NAME);
    let title = wide_null(APP_TITLE);
    let module = unsafe { GetModuleHandleW(None)? };
    let hinstance = HINSTANCE(module.0);

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        hbrBackground: unsafe { GetSysColorBrush(COLOR_BTNFACE) },
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    unsafe {
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::empty());
        }
    }

    let (x, y, width, height) = saved
        .map(|s| (s.x, s.y, s.width.max(400), s.height.max(300)))
        .unwrap_or((CW_USEDEFAULT, CW_USEDEFAULT, 1100, 780));

    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(hinstance),
            None,
        )?
    };

    let state = Box::new(AppState::new(hwnd));
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
    }

    let b_plus = create_button(hwnd, hinstance, ID_ZOOM_IN, "+", 6, 42)?;
    let b_minus = create_button(hwnd, hinstance, ID_ZOOM_OUT, "−", 54, 42)?;
    let b_100 = create_button(hwnd, hinstance, ID_100, "100%", 102, 64)?;
    let b_fit = create_button(hwnd, hinstance, ID_FIT, "Fit", 172, 54)?;
    let b_shot = create_button(hwnd, hinstance, ID_SCREENSHOT, "Screenshot", 232, 94)?;
    let b_folder = create_button(hwnd, hinstance, ID_FOLDER, "Folder", 332, 72)?;
    let b_last = create_button(hwnd, hinstance, ID_LAST, "Last", 410, 62)?;

    let tooltip = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP as u32),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(hwnd),
            None,
            Some(hinstance),
            None,
        )?
    };
    add_tooltip(tooltip, hwnd, b_plus, "Zoom in (+ / = / Numpad +)");
    add_tooltip(tooltip, hwnd, b_minus, "Zoom out (- / Numpad -)");
    add_tooltip(tooltip, hwnd, b_100, "Original size 100% (1)");
    add_tooltip(tooltip, hwnd, b_fit, "Fit to viewer (0 / F)");
    add_tooltip(tooltip, hwnd, b_shot, "Save current viewport (Ctrl+S)");
    add_tooltip(tooltip, hwnd, b_folder, "Open Screenshots folder (Ctrl+O)");
    add_tooltip(
        tooltip,
        hwnd,
        b_last,
        "Select last saved screenshot (Ctrl+Shift+O)",
    );

    if !initial_target.0.is_null() && initial_target != hwnd {
        unsafe {
            if let Some(state) = state_mut(hwnd) {
                state.switch_target(initial_target);
            }
        }
    }

    unsafe {
        let show = if saved.map(|s| s.maximized).unwrap_or(true) {
            SW_SHOWMAXIMIZED
        } else {
            SW_SHOW
        };
        ShowWindow(hwnd, show);
        let _ = UpdateWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        SetTimer(Some(hwnd), TIMER_FOREGROUND, FOREGROUND_POLL_MS, None);
    }

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.into() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
