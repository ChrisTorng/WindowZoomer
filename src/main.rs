#![windows_subsystem = "windows"]

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Local;
use image::{ImageBuffer, Rgba};
use windows::core::{PCWSTR, Result as WinResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetSysColorBrush, IntersectClipRect, PatBlt, RestoreDC, SaveDC,
    ScreenToClient, SetStretchBltMode, StretchDIBits, UpdateWindow, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLACKNESS, COLORONCOLOR, COLOR_BTNFACE, DIB_RGB_COLORS,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_ADD, VK_CONTROL, VK_DOWN, VK_LEFT,
    VK_OEM_MINUS, VK_OEM_PLUS, VK_RIGHT, VK_SHIFT, VK_SUBTRACT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetForegroundWindow, GetMessageW, InvalidateRect, IsChild, IsWindow, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, SetWindowLongPtrW, SetWindowTextW,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HMENU,
    IDC_ARROW, MSG, SW_SHOW, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
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
const ID_FIT: usize = 1003;
const ID_SCREENSHOT: usize = 1004;

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
            let mut dst = self.flags.shared.lock().map_err(|_| "frame mutex poisoned".to_string())?;
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

struct AppState {
    hwnd: HWND,
    shared: Arc<Mutex<FrameData>>,
    capture: Option<CaptureControl<CaptureHandler, String>>,
    target: Option<Window>,
    zoom: f64,
    pan_x: f64,
    pan_y: f64,
    dragging: bool,
    drag_last_x: i32,
    drag_last_y: i32,
}

impl AppState {
    fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            shared: Arc::new(Mutex::new(FrameData::default())),
            capture: None,
            target: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            dragging: false,
            drag_last_x: 0,
            drag_last_y: 0,
        }
    }

    fn current_frame_size(&self) -> Option<(u32, u32)> {
        let frame = self.shared.lock().ok()?;
        if frame.width == 0 || frame.height == 0 {
            None
        } else {
            Some((frame.width, frame.height))
        }
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
        self.fit_scale(canvas, fw, fh) * self.zoom
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
        let sw = fw as f64 * scale;
        let sh = fh as f64 * scale;

        let max_x = ((sw - cw) / 2.0).max(0.0);
        let max_y = ((sh - ch) / 2.0).max(0.0);
        self.pan_x = self.pan_x.clamp(-max_x, max_x);
        self.pan_y = self.pan_y.clamp(-max_y, max_y);
    }

    fn reset_fit(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.refresh_title();
        self.repaint();
    }

    fn repaint(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn zoom_centered(&mut self, factor: f64) {
        let canvas = self.canvas_rect();
        let x = ((canvas.left + canvas.right) / 2) as f64;
        let y = ((canvas.top + canvas.bottom) / 2) as f64;
        self.zoom_at(factor, x, y);
    }

    fn zoom_at(&mut self, factor: f64, px: f64, py: f64) {
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
        let old_sw = fw as f64 * old_scale;
        let old_sh = fh as f64 * old_scale;
        let old_left = canvas.left as f64 + (cw - old_sw) / 2.0 + self.pan_x;
        let old_top = canvas.top as f64 + (ch - old_sh) / 2.0 + self.pan_y;

        let sx = (px - old_left) / old_scale;
        let sy = (py - old_top) / old_scale;

        self.zoom = (self.zoom * factor).clamp(0.10, 32.0);

        let new_scale = self.effective_scale(canvas, fw, fh);
        let new_sw = fw as f64 * new_scale;
        let new_sh = fh as f64 * new_scale;
        let base_left = canvas.left as f64 + (cw - new_sw) / 2.0;
        let base_top = canvas.top as f64 + (ch - new_sh) / 2.0;

        self.pan_x = px - base_left - sx * new_scale;
        self.pan_y = py - base_top - sy * new_scale;
        self.clamp_pan();
        self.refresh_title();
        self.repaint();
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

        let flags = CaptureFlags {
            shared: Arc::clone(&self.shared),
            notify_hwnd: self.hwnd.0 as isize,
        };
        let settings = Settings::new(
            window,
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        );

        match CaptureHandler::start_free_threaded(settings) {
            Ok(control) => {
                self.capture = Some(control);
                self.target = Some(window);
                self.zoom = 1.0;
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
            Err(_) => {}
        }
    }

    fn refresh_title(&self) {
        let target_title = self
            .target
            .and_then(|w| w.title().ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "No target".to_string());
        let title = format!("{APP_TITLE} — {:.0}% — {target_title}", self.zoom * 100.0);
        let wide = wide_null(&title);
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr()));
        }
    }

    fn save_screenshot(&self) {
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
        if frame.pixels.len() < (fw as usize * fh as usize * 4) {
            return;
        }

        let sw = fw as f64 * scale;
        let sh = fh as f64 * scale;
        let left = (cw as f64 - sw) / 2.0 + self.pan_x;
        let top = (ch as f64 - sh) / 2.0 + self.pan_y;

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
        let _ = image.save(path);
    }
}

fn next_screenshot_path() -> Option<PathBuf> {
    let pictures = dirs::picture_dir().or_else(|| {
        std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Pictures"))
    })?;
    let dir = pictures.join("Screenshots");
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
    let ptr = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA)
    };
    if ptr == 0 {
        None
    } else {
        Some(unsafe { &mut *(ptr as *mut AppState) })
    }
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

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            if let Some(state) = state_mut(hwnd) {
                match loword(wparam.0) as usize {
                    ID_ZOOM_IN => state.zoom_centered(1.25),
                    ID_ZOOM_OUT => state.zoom_centered(0.8),
                    ID_FIT => state.reset_fit(),
                    ID_SCREENSHOT => state.save_screenshot(),
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
                state.repaint();
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state_mut(hwnd) {
                state.clamp_pan();
                state.repaint();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if let Some(state) = state_mut(hwnd) {
                let fast = GetKeyState(VK_SHIFT.0 as i32) < 0;
                let step = if fast { 120.0 } else { 36.0 };
                let key = wparam.0 as u16;
                match key {
                    k if k == VK_OEM_PLUS.0 || k == VK_ADD.0 || k == b'=' as u16 => {
                        state.zoom_centered(1.25)
                    }
                    k if k == VK_OEM_MINUS.0 || k == VK_SUBTRACT.0 => {
                        state.zoom_centered(0.8)
                    }
                    k if k == b'0' as u16 => state.reset_fit(),
                    k if k == VK_LEFT.0 => state.pan_viewport(step, 0.0),
                    k if k == VK_RIGHT.0 => state.pan_viewport(-step, 0.0),
                    k if k == VK_UP.0 => state.pan_viewport(0.0, step),
                    k if k == VK_DOWN.0 => state.pan_viewport(0.0, -step),
                    k if k == b'S' as u16 && GetKeyState(VK_CONTROL.0 as i32) < 0 => {
                        state.save_screenshot()
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(state) = state_mut(hwnd) {
                let delta = signed_word(hiword(wparam.0)) as i32;
                let x_screen = signed_word(loword(lparam.0 as usize)) as i32;
                let y_screen = signed_word(hiword(lparam.0 as usize)) as i32;
                let mut pt = windows::Win32::Foundation::POINT {
                    x: x_screen,
                    y: y_screen,
                };
                let _ = ScreenToClient(hwnd, &mut pt);
                if pt.y >= TOOLBAR_H {
                    let factor = if delta > 0 { 1.25 } else { 0.8 };
                    state.zoom_at(factor, pt.x as f64, pt.y as f64);
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
                    let dx = (x - state.drag_last_x) as f64;
                    let dy = (y - state.drag_last_y) as f64;
                    state.drag_last_x = x;
                    state.drag_last_y = y;
                    state.pan_x += dx;
                    state.pan_y += dy;
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
                    let _ = IntersectClipRect(hdc, canvas.left, canvas.top, canvas.right, canvas.bottom);
                    let _ = PatBlt(hdc, canvas.left, canvas.top, cw, ch, BLACKNESS);

                    if let Ok(frame) = state.shared.lock() {
                        if frame.width > 0
                            && frame.height > 0
                            && frame.pixels.len()
                                >= frame.width as usize * frame.height as usize * 4
                        {
                            let scale = state.effective_scale(canvas, frame.width, frame.height);
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
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ptr != 0 {
                let mut state = Box::from_raw(ptr as *mut AppState);
                if let Some(control) = state.capture.take() {
                    let _ = control.stop();
                }
                let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(state);
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

fn main() -> WinResult<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let initial_target = unsafe { GetForegroundWindow() };

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

    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1100,
            780,
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

    create_button(hwnd, hinstance, ID_ZOOM_IN, "+", 6, 44)?;
    create_button(hwnd, hinstance, ID_ZOOM_OUT, "−", 56, 44)?;
    create_button(hwnd, hinstance, ID_FIT, "Fit", 106, 60)?;
    create_button(hwnd, hinstance, ID_SCREENSHOT, "Screenshot", 172, 100)?;

    if !initial_target.0.is_null() && initial_target != hwnd {
        unsafe {
            if let Some(state) = state_mut(hwnd) {
                state.switch_target(initial_target);
            }
        }
    }

    unsafe {
        ShowWindow(hwnd, SW_SHOW);
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
