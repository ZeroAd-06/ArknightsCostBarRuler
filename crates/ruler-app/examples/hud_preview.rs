//! Offline visual preview of the HUD + menu: renders the Slint components to PNGs
//! using the software renderer (no Win32 window), so layout/colors/typography can
//! be eyeballed without launching the full app + capture backend.
//!
//! Run with: cargo run -p ruler-app --example hud_preview

slint::include_modules!();

use std::cell::RefCell;
use std::rc::Rc;

use slint::platform::{
    software_renderer::{
        MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, SoftwareRenderer,
        TargetPixel,
    },
    Platform, WindowAdapter, WindowEvent,
};
use slint::{ComponentHandle, ModelRc, PhysicalSize, PlatformError, VecModel};

/// Opaque preview pixel that composites the (partly translucent) HUD over a
/// representative game-ish backdrop so the panel translucency is visible.
#[derive(Clone, Copy)]
struct PreviewPixel {
    r: u8,
    g: u8,
    b: u8,
}

impl TargetPixel for PreviewPixel {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let inv = (u8::MAX - c.alpha) as u16;
        self.r = (self.r as u16 * inv / 255) as u8 + c.red;
        self.g = (self.g as u16 * inv / 255) as u8 + c.green;
        self.b = (self.b as u16 * inv / 255) as u8 + c.blue;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
    fn background() -> Self {
        Self {
            r: 64,
            g: 74,
            b: 68,
        }
    }
}

/// Factory platform: each component instantiation gets a brand-new window, so we
/// can preview multiple top-level components (HUD + menu) in one process. This
/// mirrors the multi-window platform used by the real overlay.
struct PreviewPlatform {
    last: Rc<RefCell<Option<Rc<MinimalSoftwareWindow>>>>,
}

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        *self.last.borrow_mut() = Some(window.clone());
        Ok(window)
    }
}

fn register_fonts() {
    use slint::fontique_08::fontique::Blob;
    let mut collection = slint::fontique_08::shared_collection();
    for face in [
        include_bytes!("../assets/fonts/Bender-Regular.otf").as_slice(),
        include_bytes!("../assets/fonts/Bender-Bold.otf").as_slice(),
        include_bytes!("../assets/fonts/Bender-Black.otf").as_slice(),
    ] {
        let _ = collection.register_fonts(Blob::new(std::sync::Arc::new(face.to_vec())), None);
    }
}

fn render_png(window: &Rc<MinimalSoftwareWindow>, w: usize, h: usize, path: &str) {
    let mut buffer = vec![PreviewPixel::from_rgb(64, 74, 68); w * h];
    // Pump real time so opacity cross-fades settle before we capture.
    for _ in 0..28 {
        std::thread::sleep(std::time::Duration::from_millis(16));
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|renderer: &SoftwareRenderer| {
            renderer.render(buffer.as_mut_slice(), w);
        });
    }

    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in &buffer {
        rgb.push(px.r);
        rgb.push(px.g);
        rgb.push(px.b);
    }
    image::save_buffer(path, &rgb, w as u32, h as u32, image::ColorType::Rgb8)
        .expect("save preview png");
    println!("wrote {path} ({w}x{h})");
}

fn main() {
    let scale = 2.5_f32;
    let last: Rc<RefCell<Option<Rc<MinimalSoftwareWindow>>>> = Rc::new(RefCell::new(None));
    slint::platform::set_platform(Box::new(PreviewPlatform { last: last.clone() })).unwrap();
    register_fonts();

    // ----- HUD -----
    let hud = Hud::new().unwrap();
    let hud_window = last.borrow_mut().take().expect("hud window");
    let hw = (210.0 * scale).round() as usize;
    let hh = (82.0 * scale).round() as usize;
    hud_window
        .window()
        .try_dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        })
        .unwrap();
    hud_window.set_size(PhysicalSize::new(hw as u32, hh as u32));
    hud.show().unwrap();

    hud.set_mode(HudMode::Running);
    hud.set_time_str("01:23:45".into());
    hud.set_frame_str("12".into());
    hud.set_total_str("/30".into());
    hud.set_lap_str("48".into());
    hud.set_cost_negative(false);
    render_png(&hud_window, hw, hh, "hud_running.png");

    hud.set_force_controls(true);
    render_png(&hud_window, hw, hh, "hud_controls.png");

    // ----- Menu -----
    let menu = RulerMenu::new().unwrap();
    let menu_window = last.borrow_mut().take().expect("menu window");

    let profile_rows = vec![
        ProfileRow {
            name: "1-7 三星".into(),
            frames: "29".into(),
            active: true,
        },
        ProfileRow {
            name: "CE-5 速通".into(),
            frames: "30".into(),
            active: false,
        },
    ];
    let n = profile_rows.len();
    menu.set_profiles(ModelRc::new(VecModel::from(profile_rows)));
    menu.set_display_mode(0);
    menu.set_scale_index(1);
    menu.set_timer_enabled(true);
    menu.set_cap_calibration("校准配置".into());
    menu.set_cap_display("帧数显示".into());
    menu.set_cap_scale("缩放".into());
    menu.set_cap_timer("调节计时器".into());
    menu.set_cap_cancel("取消".into());
    menu.set_cap_delete("删除".into());
    menu.set_label_new("新建".into());
    menu.set_about_text("v1.2.1 by Z_06".into());

    let menu_w_logical = 300.0_f32;
    let menu_h_logical = 190.0 + 30.0 * n as f32;
    let mw = (menu_w_logical * scale).round() as usize;
    let mh = (menu_h_logical * scale).round() as usize;
    menu_window
        .window()
        .try_dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        })
        .unwrap();
    menu_window.set_size(PhysicalSize::new(mw as u32, mh as u32));
    menu.show().unwrap();
    // Activate so the inline-rename TextInput can take focus + show its caret.
    let _ = menu_window
        .window()
        .try_dispatch_event(WindowEvent::WindowActiveChanged(true));
    render_png(&menu_window, mw, mh, "hud_menu.png");

    // inline rename state on row 0
    menu.set_deleting_index(-1);
    menu.set_editing_index(0);
    render_png(&menu_window, mw, mh, "hud_menu_rename.png");

    // inline delete-confirm state on row 1
    menu.set_editing_index(-1);
    menu.set_deleting_index(1);
    render_png(&menu_window, mw, mh, "hud_menu_delete.png");

    // ----- Wizard -----
    let wizard = Wizard::new().unwrap();
    let wizard_window = last.borrow_mut().take().expect("wizard window");

    let rows = vec![
        TargetRow {
            name: "MuMu #0".into(),
            detail: "127.0.0.1:16384 | D:\\MuMuPlayer-12.0".into(),
            latency: "12.3 ms".into(),
            latency_class: 1,
            error: false,
            selected: true,
        },
        TargetRow {
            name: "LDPlayer #0".into(),
            detail: "emulator-5554 | D:\\LDPlayer9".into(),
            latency: "45.0 ms".into(),
            latency_class: 3,
            error: false,
            selected: false,
        },
        TargetRow {
            name: "Windows 明日方舟".into(),
            detail: "明日方舟 [UnityWndClass]".into(),
            latency: "connect failed: device offline".into(),
            latency_class: 5,
            error: true,
            selected: false,
        },
    ];
    wizard.set_rows(ModelRc::new(VecModel::from(rows)));
    wizard.set_title_text("首次使用配置向导".into());
    wizard.set_header_text("请选择当前可用的明日方舟目标。当前平均截图延迟: 12.3 ms".into());
    wizard.set_status_text("已选择: MuMu #0\n平均截图延迟: 12.3 ms".into());
    wizard.set_status_error(false);
    wizard.set_cap_preview("实时截图预览".into());
    wizard.set_cap_empty("正在扫描目标并测量截图延迟...".into());
    wizard.set_cap_auto("下次自动选择该实例".into());
    wizard.set_cap_refresh("刷新".into());
    wizard.set_cap_start("保存并启动".into());
    wizard.set_cap_cancel("取消".into());
    wizard.set_preview_placeholder("预览不可用".into());
    wizard.set_auto_checked(true);
    wizard.set_has_preview(true);
    wizard.set_preview(synthetic_preview(160, 90));

    let ww = (560.0 * scale).round() as usize;
    let wh = (404.0 * scale).round() as usize;
    wizard_window
        .window()
        .try_dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        })
        .unwrap();
    wizard_window.set_size(PhysicalSize::new(ww as u32, wh as u32));
    wizard.show().unwrap();
    let _ = wizard_window
        .window()
        .try_dispatch_event(WindowEvent::WindowActiveChanged(true));
    render_png(&wizard_window, ww, wh, "wizard.png");
}

/// A small synthetic RGBA gradient standing in for a live capture preview.
fn synthetic_preview(w: u32, h: u32) -> slint::Image {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
    let bytes = buffer.make_mut_bytes();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            bytes[i] = (x * 255 / w as usize) as u8;
            bytes[i + 1] = (y * 255 / h as usize) as u8;
            bytes[i + 2] = 140;
            bytes[i + 3] = 255;
        }
    }
    slint::Image::from_rgba8(buffer)
}
