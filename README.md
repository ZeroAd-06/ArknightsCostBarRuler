# Arknights Cost Bar Ruler (明日方舟费用条尺子)


一个为《明日方舟》设计的悬浮窗工具，通过观察费用条的变化来测量当前关卡内的帧数，帮助你复刻那些卡帧的极限操作。

现在使用 Rust 完全重写！

## 特性

- **自适应费用条速度变化**:
  - 校准系统可以轻松处理三减费、二减费等环境。
  - 可以为不同环境保存多份校准配置，随时一键切换。
  - 校准只需数秒即可完成。
  - **现在支持可露希尔带来的负费系统！**
- **多种连接方式**:
  - **MuMu模拟器12**: 通过其官方截图接口高速取图。
  - **雷电模拟器9**: 通过其官方截图接口高速取图。
  - **Windows 明日方舟 (电脑版)**: 直接捕获官方 PC 端窗口。
  - **通用 ADB**: 走标准 `adb` 截图，兼容大部分模拟器 / 真机 (速度非常慢)。
- **原生悬浮窗**: 半透明、可拖动的覆盖层，不遮挡游戏画面。
- **计时与停表**:
  - 左侧是全局计时器 (`分:秒:帧`)，底下的按钮点击可以弹出**停表**显示相对帧数，再点一下收起。
  - 右键菜单里可以按帧 / 按整循环微调计时器，重置，以及撤销上一次重置。
- **帧数显示模式**: 支持 `0/n-1`、`0/n`、`1/n` 三种数法，看你习惯哪种。
- **首次使用向导**: 自动扫描可用目标、实时预览截图、测量截图延迟，照着点就能配好。
- **多语言**: 简体中文 / English。
- **本地 API**: 在 `127.0.0.1:2606` 上同时提供 WebSocket 推送和 HTTP 快照，方便第三方工具 (比如对轴器) 读取帧数和计时数据。详见 [API 文档](docs/API.md)。

## 快速开始

### 下载运行

前往 [Releases 页面](https://github.com/ZeroAd-06/ArknightsCostBarRuler/releases) 下载最新发行版，解压后直接运行里面的 `ruler-app.exe` 即可。

### 从源码构建

**前置要求:**

- 较新的 Rust 稳定版工具链 (`edition 2021`)
- Windows 10 / 11 (本项目仅支持 Windows)

**构建:**

```powershell
cargo build --release -p ruler-app
```

产物在 `target/release/ruler-app.exe`。

也可以用仓库自带的打包脚本，它会顺手把 `icons/`、`ruler/locales/` 和许可证文件一起塞进 `dist/`:

```powershell
.\build.ps1            # release
.\build.ps1 debug      # debug
```

**直接运行 (开发时):**

```powershell
cargo run --release -p ruler-app
```

配置和校准文件默认放在启动时的当前工作目录，也就是 `config.json` 和同目录的 `calibration\`。日志和调试产物默认放在同一根目录下的 `log\`，每次启动都会新建一个 `log\<timestamp>_<pid>\` 会话目录。用 `cargo run -p ruler-app` 从仓库根目录启动时会使用仓库根目录;用户解压发行包后直接运行时会使用解压目录。打包分发时，请保证 `icons/`、`ruler/locales/` 和许可证文件跟在 `.exe` 旁边 (`build.ps1` 已经帮你做好了)。

## 使用说明

### 首次配置

第一次启动会弹出配置向导:

1. 选择你的**连接类型** (MuMu模拟器12 / 雷电模拟器 / 通用ADB / Windows 电脑版)。
2. 按提示填好安装路径、实例索引或 ADB Device ID;PC 版则点"扫描窗口"选中明日方舟窗口。
3. 向导会扫描当前可用目标、给出实时预览和平均截图延迟，选一个能用的。
4. 点"保存并启动"。配置会写进当前工作目录下的 `config.json`。

### 首次校准

1. 启动后，右键托盘图标或悬浮窗，在"校准配置"里点 `-- 新建 --`。
2. 进入任意关卡，**保持费用条可见** (正常速度就行，不用进慢速模式)。
3. 点悬浮窗开始校准，等进度跑完。
4. 校准完成后，悬浮窗就会实时显示当前帧数了。

> 如果费用回复速率变了，右键菜单里换一份校准配置，或者新建一份重新校准即可。
> 旧版校准文件会继续按原来的映射方式读取，不会自动改写。重新校准生成的新配置会启用边界周期修正；在基础 `N` 帧周期跨过初始回费边界时，当前循环可能会显示为 `N+1` 帧。
> 想了解“前 10 费不准 / 第 11 费 31 帧”这个现象，可以看 [费用条边界周期分析](docs/cost-bar-boundary-cycle.md)。

### 悬浮窗与右键菜单

- 悬浮窗可以拖动到任意位置，位置和缩放都会被记住。
- 右键悬浮窗 (或托盘图标) 打开菜单，里面可以:
  - **校准配置**: 新建 / 选用 / 重命名 / 删除校准文件。
  - **帧数显示**: 在 `0/n-1`、`0/n`、`1/n` 之间切换。
  - **缩放**: 75% / 100% / 125% / 150%。
  - **调节计时器**: 按帧 / 按整循环前进后退、重置、撤销重置。
  - **关于 / 退出**。

## 调试与录制

这部分是给排查问题、或者想离线复现 bug 的人用的，普通使用用不到。

- 配置向导右下角可以展开 **调试选项**:
  - **下次录制一次截图视频** (MKV，体积非常大)、**录制分析数据** (CSV)、**超级详细日志**。
  - **虚拟截图器**: 不连模拟器，直接从一段录好的视频里重新跑分析,方便反复调试。
- 带 `--debug` (或 `-d`) 启动 `ruler-app` 时，会强制打开配置向导，并且默认展开这部分调试选项。
- `ruler-recorder`: 独立的录制小工具，以后端能跑到的最高帧率同时录制无损 HEVC 视频 + 逐帧分析 CSV (依赖 `ffmpeg` 在 PATH 里)。

  ```powershell
  cargo run --release -p ruler-recorder -- -c config.json -o recordings -d 60
  ```

- `ruler-app` 的配置路径可以用环境变量覆盖: `ARKNIGHTS_RULER_CONFIG_PATH`、`ARKNIGHTS_RULER_CONFIG_DIR`、`ARKNIGHTS_RULER_CALIBRATION_DIR`、`ARKNIGHTS_RULER_DATA_DIR`、`ARKNIGHTS_RULER_LOG_DIR`。旧的 `ARKNIGHTS_RULER_RECORDINGS_DIR` 仍可兼容使用。`log_output_dir` 若写相对路径，会相对配置根目录解析;写绝对路径则原样使用。

- `ruler-verifier`: 离线校验器，拿录制好的视频跑一遍分析，用来核对结果。

  ```powershell
  cargo run --release -p ruler-recorder --bin ruler-verifier -- <视频文件>
  ```

- 想看界面长啥样又不想开模拟器，可以跑 HUD 预览示例 (会在仓库根目录生成 `hud_*.png` / `wizard.png`):

  ```powershell
  cargo run -p ruler-app --example hud_preview
  ```

- 默认每次启动都会写 `log\<timestamp>_<pid>\app.log`。如果同时开启 CSV / MKV，也会落在同一个会话目录里，方便整包发给我远程排查。

- `RUST_LOG` 现在主要是开发者覆盖手段；如果你只是排查普通问题，优先用 `--debug` 向导里的超级详细日志开关。

  ```powershell
  $env:RUST_LOG = "debug"; cargo run --release -p ruler-app
  ```

## 本地 API

运行中的尺子会在 `127.0.0.1:2606` 上对外提供本地 JSON API:

- **WebSocket** (`ws://127.0.0.1:2606/`): 状态变化时主动推送兼容旧客户端的顶层快照，也可以接收 `getSnapshot`、`getFrame`、校准、配置、计时器、显示模式、悬浮窗和退出等控制命令。
- **HTTP 快照** (`http://127.0.0.1:2606/`): 一次性 `GET` 拿到当前状态的 JSON，适合不想常驻连接的场景。

> 完整字段、命令和 fallback 规则见 **[API.md](docs/API.md)**。

> 之前 Python 版自带的打轴 / 对轴器之后会挪到单独的仓库去，但它就是基于这个 API 工作的;你也可以照着 API 文档写自己的联动工具。

## 项目结构

这是一个 Cargo workspace，拆成四个 crate:

```
ArknightsCostBarRuler/
├── Cargo.toml              # workspace 根配置
├── build.ps1               # 构建 + 打包脚本
├── docs/                   # 延伸文档与 API 说明
├── crates/
│   ├── ruler-core/         # 核心库: 截图、分析、引擎 (跨二进制复用)
│   │   └── src/
│   │       ├── capture/    # 截图后端: adb / mumu / ldplayer / windows / replay
│   │       ├── analysis/   # 费用条分析: 校准、扫描、ROI、映射、战斗状态
│   │       ├── config.rs   # 配置读写
│   │       └── engine.rs   # 帧检测引擎
│   ├── ruler-app/          # 桌面应用: Slint + 原生 Win32
│   │   ├── ui/             # Slint 界面 (hud / menu / wizard / theme)
│   │   └── src/            # 悬浮窗、托盘、worker、API、配置向导、调试录制…
│   ├── ruler-recorder/     # 调试录制器 + ruler-verifier 离线校验器
│   └── ruler-pyo3/         # PyO3 绑定: 把核心暴露给 Python (cdylib: ruler_rust)
├── icons/                  # 托盘 / HUD 图标 (含 Material Symbols)
├── ruler/locales/          # 本地化翻译 (zh_CN / en_US)
└── LICENSES/               # 第三方许可证原文
```

## 注意

- 该程序在以下环境下不可用:
  - 部署费用已到达上限 (费用条满了就不动了，没法测)。
  - 部署费用无法自然回复 (如剿灭作战)。
  - 部署费用自然回复被锁定 (如第 15 章那个"活性态萨卡兹术师结晶"，就是会甩锁链锁费用的那玩意儿)。
- 如果程序行为异常，或者干脆不工作，可以试试:
  - 关掉重开。
  - 删掉当前工作目录里的 `config.json` 重新走一遍配置向导。
  - 用 `--debug` 打开调试向导，按需开启 **超级详细日志**、CSV，必要时再勾一次性 MKV。
  - 带上最新的 `log\<timestamp>_<pid>\` 会话目录，到 [Issue 页面](https://github.com/ZeroAd-06/ArknightsCostBarRuler/issues) 给我报个问题。
- 关于作者本人:
  - 我是个 6 周年才入坑的小登，游戏理解顶多算个中杯，只打过一次合约，也不确定这工具是否真的符合极限玩家的需求。
  - 所以如果你觉得它显示的帧数跟你预期的对不上，那很有可能是我的问题 (・_・;)

## 许可 & 致谢

本项目在 `MIT License` 下开源 (见根目录 `LICENSE`)。

也感谢以下优秀的开源项目:

- **[MaaFramework](https://github.com/MaaXYZ/MaaFramework)**: 本项目**并不**由 MaaFramework 驱动，但如果没有它的代码作参考，MuMu模拟器12 和 雷电模拟器9 的截图增强适配是写不出来的。相关参考遵循 `GNU Lesser General Public License v3.0`。
- **[Google Material Symbols](https://fonts.google.com/icons)**: 界面和图标用到的图标资源。在 `Apache License 2.0` 下使用。
- **[Bender 字体](https://www.fontsquirrel.com/fonts/bender)**: HUD 与界面字体，遵循 `SIL Open Font License`。

相关协议的副本都放在仓库的 `LICENSES` 文件夹里。
