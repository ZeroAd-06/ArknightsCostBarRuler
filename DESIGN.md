# Arknights Cost Bar Ruler Design System

## 1. Atmosphere & Identity

A compact tactical instrument: cold, sharp, and minimal. The timer is the hero, while configuration and profile controls stay dense and task-focused. The signature is a near-black layered HUD with cyan technical accents and amber warnings.

## 2. Color

### Palette

| Role | Token | Value | Usage |
|------|-------|-------|-------|
| Surface/overlay | `Palette.panel` | `#0a0e12e0` | Main translucent HUD panel |
| Surface/popup | `Palette.panel-solid` | `#0b1016` | Opaque popup base |
| Surface/toolbar | `Palette.toolbar` | `#0d1117f5` | Hover toolbar |
| Border/default | `Palette.hairline` | `#46c4ff8c` | Primary cyan strokes |
| Border/subtle | `Palette.hairline-faint` | `#46c4ff40` | Secondary strokes |
| Accent/primary | `Palette.accent` | `#36c5f0` | Running state, primary actions |
| Accent/dim | `Palette.accent-dim` | `#1c6b86` | Selected rows, primary button fill |
| Status/warning | `Palette.amber` | `#ffb020` | Warnings, negative cost, lap state |
| Status/error | `Palette.danger` | `#ff6b5e` | Delete, unavailable target, destructive controls |
| Text/primary | `Palette.text-primary` | `#eef6fb` | Main timer and selected text |
| Text/secondary | `Palette.text-secondary` | `#7d93a6` | Labels and secondary controls |
| Text/faint | `Palette.text-faint` | `#54677a` | Captions and placeholders |
| Interaction/hover | `Palette.hover-fill` | `#ffffff1a` | Hover wash |

### Rules

- Cyan is functional: active state, selection, primary action, and hairline structure.
- Amber is reserved for caution, negative values, lap state, and cursor blocking.
- Danger red is reserved for destructive or unavailable states.
- New colors must be added to `crates/ruler-app/ui/theme.slint` first.

## 3. Typography

### Scale

| Level | Size | Weight | Usage |
|-------|------|--------|-------|
| Timer | 30px | 700 | HUD time readout |
| Panel status | 18 to 20px | 700 | Reset cover and calibration percent |
| Primary row | 13px | 500 to 700 | Profile and target names |
| Button label | 12px | 500 to 700 | Text buttons and chips |
| Caption | 10 to 11px | 500 to 600 | Section labels and hints |
| Detail | 9px | 400 | Dense secondary detail |

### Font Stack

- Primary: `Bender`, bundled in `crates/ruler-app/assets/fonts/`.
- Fallback: platform default through Slint if Bender cannot load.

### Rules

- Keep letter spacing at `0` except compact uppercase-style section captions, which may use `1px`.
- HUD text should stay short enough to fit fixed logical dimensions.

## 4. Spacing & Layout

### Base Unit

The system is based on 4px increments, with dense controls at 2px increments only inside icon toolbars.

| Token | Value | Usage |
|-------|-------|-------|
| Tight | 2px | Icon toolbar spacing |
| Compact | 4px | Row vertical padding |
| Default | 8px | Inline groups, footer buttons |
| Panel | 12 to 14px | HUD and wizard inner padding |
| Section | 24 to 30px | Fixed menu rows and buttons |

### Layout

- HUD logical size: `210px x 82px`, with a `56px` main panel and `24px` external toolbar.
- Wizard logical width: `560px`, centered on screen, height adjusted only for debug/manual panels.
- Menu width: `300px`, height derived from fixed row metrics.

### Rules

- Keep fixed-format UI dimensions stable. Hover states must not resize controls.
- Dense operational UI should favor rows, chips, and icon buttons over large cards.

## 5. Components

### Icon Button
- **Structure**: square `IconBtn` with Material Symbols Sharp path.
- **States**: default, hover, disabled, strong amber, danger red.
- **Spacing**: 24px width, 20px height, centered glyph.
- **Motion**: hover background at 110ms.

### Chip
- **Structure**: compact text toggle used for display mode and scale.
- **States**: default, hover, selected, disabled.
- **Spacing**: 20px height, 8px horizontal padding.
- **Motion**: background and border at 110ms.

### Popup Row
- **Structure**: fixed-height row with caption, label, inline actions, or compact input.
- **States**: hover, selected, edit, delete confirm.
- **Spacing**: 30px row height in the menu, 38px target rows in the wizard.

## 6. Motion & Interaction

| Type | Duration | Usage |
|------|----------|-------|
| Micro | 90 to 110ms | Row hover, button hover |
| Standard | 140 to 200ms | Toolbar reveal, panel state fade |
| Emphasis | 700ms total | Reset cover sweep |

### Rules

- Animate opacity, color, and small state overlays only.
- Do not animate layout dimensions except existing cursor-warning and window-resize states that are already part of the native window contract.
- All click targets stay stable while hover states change.

## 7. Depth & Surface

### Strategy

Mixed: translucent tonal surfaces plus cyan hairline borders.

### Rules

- HUD and popup depth comes from alpha surfaces, not shadows.
- Do not introduce rounded cards, decorative blobs, or marketing-style panels.
- Repeated controls should reuse `theme.slint` components or extend them first.
