# Material Symbols 图标

`sharp/` 目录下的 SVG 文件来自 Google Material Symbols (Sharp 风格):

`https://github.com/google/material-design-icons/tree/master/symbols/web`

许可证为 Apache-2.0，原文见 `LICENSES/LICENSE-material-design-icons.txt`。

顶层 `icons/*.png` 里，都是同一套 Material Symbols Sharp 的 60x60 PNG 渲染，使用界面主文字色 `#eef6fb`。

## Slint 图标映射

界面 (Slint) 里的图标以 SVG path 形式内嵌在 `crates/ruler-app/ui/theme.slint` 的 `Icons` 全局对象中，对应关系如下:

| `Icons` 属性 | Material Symbol |
| --- | --- |
| `prev` | `skip_previous` |
| `next` | `skip_next` |
| `chev-left` | `chevron_left` |
| `chev-right` | `chevron_right` |
| `flag` | `flag` |
| `undo-reset` | `undo` |
| `reset` | `restart_alt` |
| `plus` | `add` |
| `play` | `play_arrow` |
| `pencil` | `edit` |
| `cross` | `close` |
| `check` | `check` |
| `info` | `info` |
| `power` | `power_settings_new` |

## PNG 图标映射

当前应用实际加载的 PNG 只有 `deco`、`start`、`wait`、`timer` 四个 (其中 `deco.png` 用作托盘图标)。

下表里 `add` / `color` / `magnet_*` / `node` / `open` / `remove` / `rename` / `save` / `sound_*` / `visual_*` 这些是**打轴 / 对轴器**用的遗留资产，本体应用并不使用;

| 文件 | Material Symbol |
| --- | --- |
| `add.png` | `bookmark_add` |
| `color.png` | `palette` |
| `magnet_off.png` | `link_off` |
| `magnet_on.png` | `link` |
| `node.png` | `open_with` |
| `open.png` | `file_open` |
| `remove.png` | `bookmark_remove` |
| `rename.png` | `edit` |
| `save.png` | `save` |
| `sound_off.png` | `volume_off` |
| `sound_on.png` | `volume_up` |
| `start.png` | `play_circle` |
| `timer.png` | `hourglass_empty` |
| `wait.png` | `timer` |
| `visual_off.png` | `visibility_off` |
| `visual_on.png` | `visibility` |
