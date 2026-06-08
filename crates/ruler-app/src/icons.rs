use std::{collections::HashMap, path::Path};

use crate::resources::ResourceLocator;

#[derive(Clone, Debug)]
pub struct PngImage {
    width: i32,
    height: i32,
    bgra: Vec<u8>,
}

impl PngImage {
    pub fn load(path: &Path) -> Result<Self, String> {
        let image = image::open(path)
            .map_err(|error| format!("failed to load icon '{}': {error}", path.display()))?
            .to_rgba8();
        let (width, height) = image.dimensions();
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        for pixel in image.pixels() {
            let [r, g, b, a] = pixel.0;
            let alpha = u16::from(a);
            bgra.push(((u16::from(b) * alpha) / 255) as u8);
            bgra.push(((u16::from(g) * alpha) / 255) as u8);
            bgra.push(((u16::from(r) * alpha) / 255) as u8);
            bgra.push(a);
        }
        Ok(Self {
            width: width as i32,
            height: height as i32,
            bgra,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct IconSet {
    icons: HashMap<&'static str, PngImage>,
}

impl IconSet {
    #[must_use]
    pub fn load(resources: &ResourceLocator) -> Self {
        let mut icons = HashMap::new();
        for name in ["deco", "start", "wait", "timer"] {
            let filename = format!("{name}.png");
            match PngImage::load(&resources.icon_path(&filename)) {
                Ok(image) => {
                    icons.insert(name, image);
                }
                Err(error) => log::warn!("{error}"),
            }
        }
        Self { icons }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PngImage> {
        self.icons.get(name)
    }
}

#[cfg(windows)]
pub mod win32 {
    use std::{ffi::c_void, mem, ptr};

    use windows::Win32::{
        Foundation::{BOOL, RECT},
        Graphics::Gdi::{
            AlphaBlend, CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
            SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
            BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
        },
        UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO},
    };

    use super::PngImage;

    pub unsafe fn draw_scaled(hdc: HDC, image: &PngImage, rect: RECT, alpha: u8) {
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return;
        }
        let Some(bitmap) = create_bitmap(image) else {
            return;
        };
        let mem_dc = CreateCompatibleDC(hdc);
        if mem_dc.0.is_null() {
            let _ = DeleteObject(bitmap);
            return;
        }

        let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = AlphaBlend(
            hdc,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            mem_dc,
            0,
            0,
            image.width,
            image.height,
            blend,
        );
        let _ = SelectObject(mem_dc, old);
        let _ = DeleteDC(mem_dc);
        let _ = DeleteObject(bitmap);
    }

    pub unsafe fn create_icon(image: &PngImage, size: i32) -> Option<HICON> {
        let scaled = resize_nearest(image, size, size);
        let color = create_bitmap(&scaled)?;
        let mask = CreateBitmap(size, size, 1, 1, None);
        if mask.0.is_null() {
            let _ = DeleteObject(color);
            return None;
        }
        let info = ICONINFO {
            fIcon: BOOL(1),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: HBITMAP(mask.0),
            hbmColor: HBITMAP(color.0),
        };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);
        icon
    }

    unsafe fn create_bitmap(image: &PngImage) -> Option<HGDIOBJ> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: image.width,
                biHeight: -image.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = ptr::null_mut();
        let Ok(bitmap) =
            CreateDIBSection(HDC::default(), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        else {
            return None;
        };
        if bitmap.0.is_null() || bits.is_null() {
            return None;
        }
        ptr::copy_nonoverlapping(image.bgra.as_ptr(), bits.cast::<u8>(), image.bgra.len());
        Some(HGDIOBJ(bitmap.0))
    }

    fn resize_nearest(image: &PngImage, width: i32, height: i32) -> PngImage {
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let src_y = y * image.height / height;
            for x in 0..width {
                let src_x = x * image.width / width;
                let src = ((src_y * image.width + src_x) * 4) as usize;
                bgra.extend_from_slice(&image.bgra[src..src + 4]);
            }
        }
        PngImage {
            width,
            height,
            bgra,
        }
    }
}
