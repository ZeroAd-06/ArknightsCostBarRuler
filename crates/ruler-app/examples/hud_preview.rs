//! Offline visual preview of the HUD: renders the Slint component to PNGs using
//! the software renderer (no Win32 window), so the layout/colors/typography can
//! be eyeballed without launching the full app + capture backend.
//!
//! Run with: cargo run -p ruler-app --example hud_preview

slint::include_modules!();

use std::rc::Rc;

use slint::platform::{
    software_renderer::{
        MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, SoftwareRenderer,
        TargetPixel,
    },
    Platform, WindowAdapter, WindowEvent,
};
use slint::{ComponentHandle, PhysicalSize, PlatformError};

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
        // mimic the Arknights HUD area behind the overlay (mid tone so the
        // panel translucency + cyan hairline are visible in the preview)
        Self {
            r: 64,
            g: 74,
            b: 68,
        }
    }
}

struct PreviewPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
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
    let w = (210.0 * scale).round() as usize;
    let h = (56.0 * scale).round() as usize;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform {
        window: window.clone(),
    }))
    .unwrap();
    register_fonts();

    let hud = Hud::new().unwrap();
    window
        .window()
        .try_dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        })
        .unwrap();
    window.set_size(PhysicalSize::new(w as u32, h as u32));
    hud.show().unwrap();

    // Running state
    hud.set_mode(HudMode::Running);
    hud.set_time_str("01:23:45".into());
    hud.set_frame_str("12".into());
    hud.set_total_str("/30".into());
    hud.set_lap_str("48".into());
    hud.set_cost_negative(false);
    render_png(&window, w, h, "hud_running.png");

    // Running with hover control toolbar revealed
    hud.set_force_controls(true);
    render_png(&window, w, h, "hud_controls.png");
    hud.set_force_controls(false);

    // Negative-cost running
    hud.set_total_str("/30".into());
    hud.set_cost_negative(true);
    hud.set_lap_str("".into());
    render_png(&window, w, h, "hud_running_neg.png");

    // Calibrating
    hud.set_mode(HudMode::Calibrating);
    hud.set_progress(62.0);
    hud.set_progress_str("62%".into());
    render_png(&window, w, h, "hud_calibrating.png");

    // Pre-calibration CTA
    hud.set_mode(HudMode::Precal);
    hud.set_message("进入关卡后\n点击此处校准".into());
    render_png(&window, w, h, "hud_precal.png");

    // Error
    hud.set_mode(HudMode::Error);
    hud.set_message("capture error: device offline".into());
    render_png(&window, w, h, "hud_error.png");
}
