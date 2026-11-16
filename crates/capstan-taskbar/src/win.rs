//! Win32 实现。FFI 与子类化的 unsafe 全部集中在这个文件。

use std::{
    cell::Cell,
    collections::HashMap,
    ffi::c_void,
    mem::{size_of, transmute},
    ptr,
    sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError},
};

use image::RgbaImage;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM},
        Graphics::{
            Dwm::{
                DWMWA_FORCE_ICONIC_REPRESENTATION, DWMWA_HAS_ICONIC_BITMAP,
                DwmInvalidateIconicBitmaps, DwmSetIconicLivePreviewBitmap, DwmSetIconicThumbnail,
                DwmSetWindowAttribute,
            },
            Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC,
                CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, SelectObject,
            },
        },
        Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow},
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::{
            HiDpi::GetDpiForWindow,
            Shell::{
                ITaskbarList3, THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_DISABLED, THBF_ENABLED,
                THUMBBUTTON, TaskbarList,
            },
            WindowsAndMessaging::{
                CallWindowProcW, CreateIconIndirect, DefWindowProcW, DestroyIcon, GWLP_WNDPROC,
                GetClientRect, HICON, HTMINBUTTON, ICONINFO, IsIconic, SC_MINIMIZE,
                SetWindowLongPtrW, SetWindowTextW, WA_INACTIVE, WM_ACTIVATE, WM_COMMAND,
                WM_DWMSENDICONICLIVEPREVIEWBITMAP, WM_DWMSENDICONICTHUMBNAIL, WM_NCDESTROY,
                WM_NCLBUTTONDOWN, WM_SIZE, WM_SYSCOMMAND, WNDPROC,
            },
        },
    },
    core::{BOOL, HSTRING, IUnknown, PCWSTR},
};

use crate::{Art, Labels, TaskbarAction};

const ID_PREV: u32 = 0x4A11;
const ID_PLAY: u32 = 0x4A12;
const ID_NEXT: u32 = 0x4A13;
const ID_LIKE: u32 = 0x4A14;

/// 缩略图留白，BGRA。深色在浅色任务栏上同样可读。
const BACKGROUND: [u8; 4] = [0x0D, 0x0D, 0x0D, 0xFF];

/// `PW_CLIENTONLY | PW_RENDERFULLCONTENT`：只画客户区，且带上 DirectComposition 内容。
/// 少了 CLIENTONLY 会把非客户区也画进客户区尺寸的位图，预览左侧出现黑边。
const PRINT_CLIENT_FULL_CONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(3);

const PREV_PNG: &[u8] = include_bytes!("../assets/prev.png");
const PLAY_PNG: &[u8] = include_bytes!("../assets/play.png");
const PAUSE_PNG: &[u8] = include_bytes!("../assets/pause.png");
const NEXT_PNG: &[u8] = include_bytes!("../assets/next.png");
const HEART_PNG: &[u8] = include_bytes!("../assets/heart.png");
const HEART_FILLED_PNG: &[u8] = include_bytes!("../assets/heart_filled.png");

pub struct Taskbar {
    hwnd: HWND,
    list: ITaskbarList3,
    icons: Icons,
    labels: Labels,
    state: Cell<State>,
    shared: Arc<Shared>,
}

impl Taskbar {
    /// 挂到主窗口。任何一步失败都返回 `None`，调用方降级为没有任务栏集成。
    pub fn attach(hwnd: isize, labels: Labels) -> Option<Self> {
        let hwnd = HWND(hwnd as *mut c_void);
        if hwnd.is_invalid() {
            return None;
        }

        let list: ITaskbarList3 =
            unsafe { CoCreateInstance(&TaskbarList, None::<&IUnknown>, CLSCTX_INPROC_SERVER) }
                .ok()?;
        unsafe { list.HrInit() }.ok()?;

        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let icon_px = if dpi == 0 {
            16
        } else {
            ((u64::from(16 * dpi) + 48) / 96).clamp(16, 48) as u32
        };
        let icons = Icons::load(icon_px)?;

        let initial_client = {
            let mut rect = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rect) };
            (rect.right.max(1) as u32, rect.bottom.max(1) as u32)
        };
        let shared = Arc::new(Shared {
            art: Mutex::new(None),
            actions: Arc::new(Mutex::new(Vec::new())),
            last_client: Mutex::new(initial_client),
            snapshot: Mutex::new(None),
        });
        let old_proc = unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWLP_WNDPROC,
                taskbar_proc as *const () as usize as isize,
            )
        };
        if old_proc == 0 {
            log::warn!("任务栏集成：替换窗口过程失败");
            return None;
        }
        lock(subclasses()).insert(
            hwnd.0 as isize,
            Subclass {
                old_proc: old_proc as usize,
                shared: Arc::clone(&shared),
            },
        );

        let taskbar = Self {
            hwnd,
            list,
            icons,
            labels,
            state: Cell::new(State::default()),
            shared,
        };
        let buttons = taskbar.buttons();
        if let Err(err) = unsafe { taskbar.list.ThumbBarAddButtons(hwnd, &buttons) } {
            log::warn!("任务栏集成：添加缩略图按钮失败：{err}");
            return None;
        }

        // 强制标志性表示，任务栏悬停缩略图才会用封面而不是实时窗口；
        // 速览预览另给一张实时窗口截图，避免拿封面去铺窗口比例。
        unsafe {
            let on = BOOL(1);
            for attribute in [DWMWA_HAS_ICONIC_BITMAP, DWMWA_FORCE_ICONIC_REPRESENTATION] {
                if let Err(err) = DwmSetWindowAttribute(
                    hwnd,
                    attribute,
                    (&raw const on).cast(),
                    size_of::<BOOL>() as u32,
                ) {
                    log::warn!("任务栏集成：DWM 属性设置失败：{err}");
                }
            }
        }

        Some(taskbar)
    }

    /// 按钮点击的管理线程可轮询队列。队列与窗口过程共享，克隆开销只有一次锁。
    pub fn actions(&self) -> Arc<Mutex<Vec<TaskbarAction>>> {
        Arc::clone(&self.shared.actions)
    }

    /// 换缩略图内容。`None` 会退回纯色底，调用方应传品牌标当兜底。
    pub fn set_art(&self, art: Option<Art>) {
        *lock(&self.shared.art) = art.map(Arc::new);
        unsafe {
            let _ = DwmInvalidateIconicBitmaps(self.hwnd);
        }
    }

    /// 窗口标题、缩略图 tooltip 与按钮可用状态。标题同时是预览头部的名字。
    pub fn set_track(&self, title: &str, enabled: bool) {
        let wide = HSTRING::from(title);
        unsafe {
            if let Err(err) = SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr())) {
                log::warn!("任务栏集成：设置窗口标题失败：{err}");
            }
            if let Err(err) = self
                .list
                .SetThumbnailTooltip(self.hwnd, PCWSTR(wide.as_ptr()))
            {
                log::debug!("任务栏集成：设置缩略图 tooltip 失败：{err}");
            }
        }
        self.update_state(State {
            enabled,
            ..self.state.get()
        });
    }

    pub fn set_playing(&self, playing: bool) {
        self.update_state(State {
            playing,
            ..self.state.get()
        });
    }

    pub fn set_liked(&self, liked: bool) {
        self.update_state(State {
            liked,
            ..self.state.get()
        });
    }

    fn update_state(&self, next: State) {
        if next == self.state.get() {
            return;
        }
        self.state.set(next);
        let buttons = self.buttons();
        if let Err(err) = unsafe { self.list.ThumbBarUpdateButtons(self.hwnd, &buttons) } {
            log::debug!("任务栏集成：更新缩略图按钮失败：{err}");
        }
    }

    fn buttons(&self) -> [THUMBBUTTON; 4] {
        let state = self.state.get();
        let (play_icon, play_tip) = if state.playing {
            (self.icons.pause, self.labels.pause)
        } else {
            (self.icons.play, self.labels.play)
        };
        let (like_icon, like_tip) = if state.liked {
            (self.icons.heart_filled, self.labels.unlike)
        } else {
            (self.icons.heart, self.labels.like)
        };
        [
            button(ID_PREV, self.icons.prev, self.labels.prev, state.enabled),
            button(ID_PLAY, play_icon, play_tip, state.enabled),
            button(ID_NEXT, self.icons.next, self.labels.next, state.enabled),
            button(ID_LIKE, like_icon, like_tip, state.enabled),
        ]
    }
}

impl Drop for Taskbar {
    fn drop(&mut self) {
        // 窗口先销毁时子类化表已被 WM_NCDESTROY 清掉，这里只恢复还活着的窗口。
        let subclass = lock(subclasses()).remove(&(self.hwnd.0 as isize));
        if let Some(subclass) = subclass {
            unsafe { SetWindowLongPtrW(self.hwnd, GWLP_WNDPROC, subclass.old_proc as isize) };
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct State {
    enabled: bool,
    playing: bool,
    liked: bool,
}

struct Shared {
    art: Mutex<Option<Arc<Art>>>,
    actions: Arc<Mutex<Vec<TaskbarAction>>>,
    /// 最小化后 `GetClientRect` 为 0×0，速览要用最小化前的客户区尺寸。
    last_client: Mutex<(u32, u32)>,
    /// 失焦 / 最小化前的最后一帧。最小化窗口无法 PrintWindow，速览只能回放这张图。
    snapshot: Mutex<Option<Arc<Art>>>,
}

struct Subclass {
    old_proc: usize,
    shared: Arc<Shared>,
}

fn subclasses() -> &'static Mutex<HashMap<isize, Subclass>> {
    static CLASSES: OnceLock<Mutex<HashMap<isize, Subclass>>> = OnceLock::new();
    CLASSES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn button(id: u32, icon: HICON, tip: &str, enabled: bool) -> THUMBBUTTON {
    THUMBBUTTON {
        dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
        iId: id,
        iBitmap: 0,
        hIcon: icon,
        szTip: wide_tip(tip),
        dwFlags: if enabled { THBF_ENABLED } else { THBF_DISABLED },
    }
}

fn wide_tip(text: &str) -> [u16; 260] {
    let mut tip = [0u16; 260];
    for (slot, unit) in tip.iter_mut().zip(text.encode_utf16().take(259)) {
        *slot = unit;
    }
    tip
}

unsafe extern "system" fn taskbar_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let key = hwnd.0 as isize;
    let (old_proc, shared) = {
        let classes = lock(subclasses());
        match classes.get(&key) {
            Some(subclass) => (subclass.old_proc, Arc::clone(&subclass.shared)),
            None => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    };

    match msg {
        WM_COMMAND => {
            let action = match (wparam.0 as u32) & 0xFFFF {
                ID_PREV => Some(TaskbarAction::Prev),
                ID_PLAY => Some(TaskbarAction::TogglePlay),
                ID_NEXT => Some(TaskbarAction::Next),
                ID_LIKE => Some(TaskbarAction::ToggleLike),
                _ => None,
            };
            if let Some(action) = action {
                lock(&shared.actions).push(action);
                return LRESULT(0);
            }
        }
        WM_DWMSENDICONICTHUMBNAIL => {
            let raw = lparam.0 as u32;
            let (max_width, max_height) = clamp_request((raw >> 16) & 0xFFFF, raw & 0xFFFF);
            // 任务栏按位图的长宽比排预览区。用窗口比例会得到一条宽幅，方形位图才让封面区保持正方形；
            // 取两个上限里的短边，保证不越过 DWM 给的最大尺寸。
            let side = max_width.min(max_height).max(1);
            let art = lock(&shared.art).clone();
            if let Some(art) = art
                && let Some(bitmap) = compose_bitmap(&art, side, side)
            {
                unsafe {
                    let _ = DwmSetIconicThumbnail(hwnd, bitmap, 0);
                    let _ = DeleteObject(bitmap.into());
                }
            }
            return LRESULT(0);
        }
        WM_DWMSENDICONICLIVEPREVIEWBITMAP => {
            let iconic = unsafe { IsIconic(hwnd) }.as_bool();
            let snapshot = lock(&shared.snapshot).is_some();
            log::debug!("任务栏速览：iconic={iconic} snapshot={snapshot}");
            let bitmap = if iconic {
                // 最小化后 PrintWindow 抓不到画面，回放失焦 / 最小化前的最后一帧。
                lock(&shared.snapshot)
                    .clone()
                    .and_then(|snapshot| {
                        create_dib(snapshot.width, snapshot.height, &snapshot.bgra)
                    })
                    .or_else(|| {
                        let (width, height) = preview_size(hwnd, &shared);
                        fallback_preview(&shared, width, height)
                    })
            } else {
                let (width, height) = preview_size(hwnd, &shared);
                capture_window(hwnd, width, height)
                    .or_else(|| fallback_preview(&shared, width, height))
            };
            if let Some(bitmap) = bitmap {
                unsafe {
                    let _ = DwmSetIconicLivePreviewBitmap(hwnd, bitmap, None, 0);
                    let _ = DeleteObject(bitmap.into());
                }
            }
            return LRESULT(0);
        }
        WM_NCLBUTTONDOWN => {
            if (wparam.0 as u32) & 0xFFFF == HTMINBUTTON {
                remember_snapshot(hwnd, &shared);
            }
        }
        WM_SYSCOMMAND => {
            if (wparam.0 as u32) & 0xFFF0 == SC_MINIMIZE {
                remember_snapshot(hwnd, &shared);
            }
        }
        WM_ACTIVATE => {
            if (wparam.0 as u32) & 0xFFFF == WA_INACTIVE {
                remember_snapshot(hwnd, &shared);
            }
        }
        WM_SIZE => {
            let width = (lparam.0 as u32) & 0xFFFF;
            let height = ((lparam.0 as u32) >> 16) & 0xFFFF;
            if width > 0 && height > 0 {
                *lock(&shared.last_client) = (width, height);
            }
        }
        WM_NCDESTROY => {
            lock(subclasses()).remove(&key);
        }
        _ => {}
    }

    let old: WNDPROC = unsafe { transmute(old_proc) };
    unsafe { CallWindowProcW(old, hwnd, msg, wparam, lparam) }
}

fn clamp_request(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        (256, 256)
    } else {
        (width.min(1024), height.min(1024))
    }
}

/// 速览位图尺寸。最小化后客户区是 0×0，回退到最近一次非零尺寸。
fn preview_size(hwnd: HWND, shared: &Shared) -> (u32, u32) {
    let mut rect = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_ok() && rect.right > 0 && rect.bottom > 0 {
        let size = (rect.right as u32, rect.bottom as u32);
        *lock(&shared.last_client) = size;
        return clamp_preview(size);
    }
    clamp_preview(*lock(&shared.last_client))
}

fn clamp_preview((width, height): (u32, u32)) -> (u32, u32) {
    (width.clamp(1, 4096), height.clamp(1, 4096))
}

fn fallback_preview(shared: &Shared, width: u32, height: u32) -> Option<HBITMAP> {
    let art = lock(&shared.art).clone();
    art.as_ref()
        .and_then(|art| compose_bitmap(art, width, height))
}

/// 失焦或最小化前留一帧。窗口一旦最小化，PrintWindow 就抓不到画面了；
/// 抓不到时保留上一张，不能拿空白覆盖。
fn remember_snapshot(hwnd: HWND, shared: &Shared) {
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return;
    }
    let (width, height) = preview_size(hwnd, shared);
    if let Some(art) = capture_art(hwnd, width, height) {
        log::debug!("任务栏快照：缓存 {}×{}", art.width, art.height);
        *lock(&shared.snapshot) = Some(Arc::new(art));
    }
}

fn compose_bitmap(art: &Art, width: u32, height: u32) -> Option<HBITMAP> {
    let pixels = compose_pixels(Some(art), width, height)?;
    create_dib(width.max(1), height.max(1), &pixels)
}

/// 把当前窗口画到一张 32 位 DIB 上，供速览预览使用。
fn capture_window(hwnd: HWND, width: u32, height: u32) -> Option<HBITMAP> {
    unsafe {
        let (bitmap, _) = create_blank_dib(width, height)?;
        let memory = CreateCompatibleDC(None);
        if memory.is_invalid() {
            let _ = DeleteObject(bitmap.into());
            return None;
        }
        let previous = SelectObject(memory, bitmap.into());
        let printed = PrintWindow(hwnd, memory, PRINT_CLIENT_FULL_CONTENT);
        SelectObject(memory, previous);
        let _ = DeleteDC(memory);
        if !printed.as_bool() {
            let _ = DeleteObject(bitmap.into());
            return None;
        }
        Some(bitmap)
    }
}

/// 抓一帧并拷成 BGRA，供最小化后的速览回放。
fn capture_art(hwnd: HWND, width: u32, height: u32) -> Option<Art> {
    unsafe {
        let (bitmap, bits) = create_blank_dib(width, height)?;
        let memory = CreateCompatibleDC(None);
        if memory.is_invalid() {
            let _ = DeleteObject(bitmap.into());
            return None;
        }
        let previous = SelectObject(memory, bitmap.into());
        let printed = PrintWindow(hwnd, memory, PRINT_CLIENT_FULL_CONTENT);
        SelectObject(memory, previous);
        let _ = DeleteDC(memory);
        if !printed.as_bool() {
            let _ = DeleteObject(bitmap.into());
            return None;
        }
        let length = width as usize * height as usize * 4;
        let mut bgra = std::slice::from_raw_parts(bits.cast::<u8>(), length).to_vec();
        let _ = DeleteObject(bitmap.into());
        // PrintWindow 不保证 alpha，DWM 会把它当透明度，统一压成不透明。
        for pixel in bgra.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Some(Art {
            width,
            height,
            bgra,
        })
    }
}

/// 完整放下封面，四周留底色。缩略图是正方形，方封面的常见情况会正好铺满。
fn compose_pixels(art: Option<&Art>, width: u32, height: u32) -> Option<Vec<u8>> {
    let width = width.max(1);
    let height = height.max(1);
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&BACKGROUND);
    }

    let Some(art) = art else {
        return Some(pixels);
    };
    if art.width == 0 || art.height == 0 {
        return Some(pixels);
    }
    let source = RgbaImage::from_raw(art.width, art.height, art.bgra.clone())?;

    let scale = (width as f32 / art.width as f32).min(height as f32 / art.height as f32);
    let fitted_width = ((art.width as f32 * scale).round() as u32).clamp(1, width);
    let fitted_height = ((art.height as f32 * scale).round() as u32).clamp(1, height);
    let left = (width - fitted_width) / 2;
    let top = (height - fitted_height) / 2;
    let fitted = image::imageops::resize(
        &source,
        fitted_width,
        fitted_height,
        image::imageops::FilterType::Triangle,
    );

    for (x, y, pixel) in fitted.enumerate_pixels() {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            continue;
        }
        let inverse = 255 - alpha;
        let index = (((top + y) * width + left + x) * 4) as usize;
        for channel in 0..3 {
            pixels[index + channel] = ((u32::from(pixel[channel]) * alpha
                + u32::from(pixels[index + channel]) * inverse)
                / 255) as u8;
        }
        pixels[index + 3] = 255;
    }

    Some(pixels)
}

fn bitmap_info(width: u32, height: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn create_dib(width: u32, height: u32, pixels: &[u8]) -> Option<HBITMAP> {
    unsafe {
        let info = bitmap_info(width, height);
        let mut bits: *mut c_void = ptr::null_mut();
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() || pixels.len() < width as usize * height as usize * 4 {
            let _ = DeleteObject(bitmap.into());
            return None;
        }
        ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
        Some(bitmap)
    }
}

/// 只分配画布，内容交给 `PrintWindow` 画。返回值同时带出像素指针，供回放缓存拷贝。
fn create_blank_dib(width: u32, height: u32) -> Option<(HBITMAP, *mut c_void)> {
    unsafe {
        let info = bitmap_info(width, height);
        let mut bits: *mut c_void = ptr::null_mut();
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(bitmap.into());
            return None;
        }
        Some((bitmap, bits))
    }
}

struct Icons {
    prev: HICON,
    play: HICON,
    pause: HICON,
    next: HICON,
    heart: HICON,
    heart_filled: HICON,
}

impl Icons {
    fn load(px: u32) -> Option<Self> {
        let sources = [
            PREV_PNG,
            PLAY_PNG,
            PAUSE_PNG,
            NEXT_PNG,
            HEART_PNG,
            HEART_FILLED_PNG,
        ];
        let mut loaded = Vec::with_capacity(sources.len());
        for source in sources {
            match icon_from_png(source, px) {
                Some(icon) => loaded.push(icon),
                None => {
                    for icon in loaded {
                        unsafe {
                            let _ = DestroyIcon(icon);
                        }
                    }
                    return None;
                }
            }
        }
        Some(Self {
            prev: loaded[0],
            play: loaded[1],
            pause: loaded[2],
            next: loaded[3],
            heart: loaded[4],
            heart_filled: loaded[5],
        })
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        for icon in [
            self.prev,
            self.play,
            self.pause,
            self.next,
            self.heart,
            self.heart_filled,
        ] {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

fn icon_from_png(bytes: &[u8], px: u32) -> Option<HICON> {
    let source = image::load_from_memory(bytes).ok()?.to_rgba8();
    let source = image::imageops::resize(&source, px, px, image::imageops::FilterType::Lanczos3);
    // 白色按 alpha 预乘；任务栏给按钮画自己的底，图标只需 alpha 形状。
    let mut pixels = vec![0u8; (px * px * 4) as usize];
    for (index, pixel) in source.pixels().enumerate() {
        let alpha = pixel[3];
        pixels[index * 4..index * 4 + 4].copy_from_slice(&[alpha, alpha, alpha, alpha]);
    }
    let color = create_dib(px, px, &pixels)?;
    let mask_bytes = vec![0u8; (px as usize).div_ceil(8) * px as usize];

    unsafe {
        let mask = CreateBitmap(px as i32, px as i32, 1, 1, Some(mask_bytes.as_ptr().cast()));
        if mask.is_invalid() {
            let _ = DeleteObject(color.into());
            return None;
        }
        let info = ICONINFO {
            fIcon: TRUE,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::{Art, BACKGROUND, clamp_preview, clamp_request, compose_pixels, wide_tip};

    fn art(width: u32, height: u32, color: [u8; 4]) -> Art {
        Art {
            width,
            height,
            bgra: color.repeat(width as usize * height as usize),
        }
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * width + x) * 4) as usize;
        pixels[index..index + 4].try_into().expect("四个通道")
    }

    #[test]
    fn contain_letterboxes_instead_of_cropping() {
        let source = art(100, 50, [0, 0, 255, 255]);
        let pixels = compose_pixels(Some(&source), 200, 200).expect("应能合成");

        assert_eq!(pixel(&pixels, 200, 0, 0), BACKGROUND, "上边应留底色");
        assert_eq!(pixel(&pixels, 200, 0, 49), BACKGROUND, "上边留白 50px");
        assert_eq!(
            pixel(&pixels, 200, 100, 50),
            [0, 0, 255, 255],
            "封面应从第 50 行开始，颜色不能被底色带偏"
        );
        assert_eq!(pixel(&pixels, 200, 100, 149), [0, 0, 255, 255]);
        assert_eq!(pixel(&pixels, 200, 0, 150), BACKGROUND, "下边留白 50px");
    }

    #[test]
    fn square_art_fills_a_square_thumbnail() {
        let source = art(100, 100, [0, 0, 255, 255]);
        let pixels = compose_pixels(Some(&source), 200, 200).expect("应能合成");

        for (x, y) in [(0, 0), (199, 0), (0, 199), (199, 199)] {
            assert_eq!(
                pixel(&pixels, 200, x, y),
                [0, 0, 255, 255],
                "四角 {x},{y} 应被方形封面铺满"
            );
        }
    }

    #[test]
    fn translucent_art_is_composited_over_the_background() {
        let source = art(10, 10, [0, 0, 0, 128]);
        let pixels = compose_pixels(Some(&source), 10, 10).expect("应能合成");

        let expected = (u32::from(BACKGROUND[0]) * 127 / 255) as u8;
        assert_eq!(
            pixel(&pixels, 10, 0, 0),
            [expected, expected, expected, 255]
        );
    }

    #[test]
    fn missing_art_is_pure_background() {
        let pixels = compose_pixels(None, 8, 8).expect("应能合成");
        assert!(pixels.as_chunks::<4>().0.iter().all(|px| *px == BACKGROUND));
    }

    #[test]
    fn tips_are_nul_terminated_and_truncated() {
        let tip = wide_tip("上一曲");
        assert_eq!(&tip[..3], &[0x4E0A, 0x4E00, 0x66F2]);
        assert_eq!(tip[3], 0, "tooltip 必须以 NUL 结尾");

        let tip = wide_tip(&"字".repeat(300));
        assert_eq!(tip[258], 0x5B57, "第 259 个字符应保留");
        assert_eq!(tip[259], 0, "超长 tooltip 必须留 NUL");
    }

    #[test]
    fn odd_requests_fall_back_to_a_usable_size() {
        assert_eq!(clamp_request(0, 120), (256, 256));
        assert_eq!(clamp_request(120, 0), (256, 256));
        assert_eq!(clamp_request(4000, 3000), (1024, 1024));
        assert_eq!(clamp_request(160, 120), (160, 120));
    }

    #[test]
    fn preview_sizes_are_clamped() {
        assert_eq!(clamp_preview((0, 0)), (1, 1));
        assert_eq!(clamp_preview((8000, 3000)), (4096, 3000));
        assert_eq!(clamp_preview((1276, 769)), (1276, 769));
    }
}
