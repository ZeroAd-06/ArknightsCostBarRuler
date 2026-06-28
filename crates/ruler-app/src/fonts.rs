//! Registers the bundled Bender font (SIL OFL 1.1) into Slint's shared font
//! collection so the HUD can use `font-family: "Bender"`. CJK glyphs fall back
//! to system fonts via fontique. Must be called after `set_platform`.
//!
//! The bundled faces are a modified copy: the HUD counter glyphs (0-9 and `/`)
//! were given a uniform advance (centered into the width of `0`) so timer and
//! frame counters read as tabular and stop jittering. The software renderer
//! ignores OpenType `tnum`, so this has to be baked into the font — see
//! `scripts/mono_digits.py`.

const BENDER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Bender-Regular.otf");
const BENDER_BOLD: &[u8] = include_bytes!("../assets/fonts/Bender-Bold.otf");
const BENDER_BLACK: &[u8] = include_bytes!("../assets/fonts/Bender-Black.otf");

/// Register the bundled Bender faces. Safe to call once after the Slint
/// platform is initialized; failures are logged and otherwise ignored so the
/// HUD degrades to the default font rather than refusing to start.
pub fn register_bundled_fonts() {
    use slint::fontique_08::fontique::Blob;

    let mut collection = slint::fontique_08::shared_collection();
    for face in [BENDER_REGULAR, BENDER_BOLD, BENDER_BLACK] {
        let blob = Blob::new(std::sync::Arc::new(face.to_vec()));
        let registered = collection.register_fonts(blob, None);
        if registered.is_empty() {
            log::warn!("failed to register a bundled Bender face");
        }
    }
}
