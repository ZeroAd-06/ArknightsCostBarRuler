//! Slint-generated UI components.
//!
//! `hud.slint` is compiled by `build.rs` (via `slint-build`) into Rust code that
//! this macro pulls into the crate. The generated `Hud` component and `HudMode`
//! enum are re-exported for the overlay platform layer.

slint::include_modules!();
