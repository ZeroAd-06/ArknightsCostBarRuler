# RULER-APP KNOWLEDGE BASE

## OVERVIEW
Windows 桌面前端 crate：负责启动 UX、配置向导、HUD 悬浮窗、托盘菜单、本地 API，以及把 `ruler-core` 结果变成可交互体验。

## STRUCTURE
```text
crates/ruler-app/
├── src/       # main/app/worker/overlay/wizard/API/tray logic
├── ui/        # Slint UI definitions and theme pieces
├── examples/  # `hud_preview` 等独立界面 smoke surface
└── assets/    # fonts and packaged UI assets
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 启动顺序、参数、向导是否强制弹出 | `src/main.rs`, `src/app.rs` | `--debug` 语义在这里入场 |
| 后台 worker、状态广播、命令调度 | `src/worker.rs`, `src/analyzer_consumer.rs` | app 层真正的状态机 |
| HUD 窗口、窗口位置/缩放、透明层 | `src/overlay/` (platform: hud/menu/geometry), `src/slint_win.rs`, `ui/` | 保持原生窗口和 Slint 分工清晰 |
| 配置向导、截图预览、目标延迟测量 | `src/config_wizard/` (platform: callbacks/sync/probe), `src/target_discovery/` (platform: mumu/ldplayer/windows/adb/process) | 这是当前最复杂的前端热点 |
| 托盘、右键菜单、本地 API | `src/{tray,menu,api,ui_state}.rs` | 都应只读共享状态 |
| 调试录制与日志入口 | `src/debug_recorder.rs`, `src/logging.rs` | 产物最后落到会话日志目录 |

## CONVENTIONS
- UI 层只消费 `SharedAppState`；capture、analysis、校准切换通过 worker / command channel 驱动，不要在控件回调里偷偷直连 core。
- `config_wizard/platform/mod.rs` 的 `WizardCore` 运行在单 UI 线程，靠 `Rc<RefCell<_>>` 管状态；新增并发逻辑时，先确认是否真的需要跨线程。
- 目标列表模型要原地更新，不要为了刷新延迟/预览反复替换整个 model；那会重置 hover/动画并破坏现有手感。
- `--debug` 必须继续表示“强制打开配置向导 + 默认展开调试区”；普通模式下用户仍应能手动展开调试项。
- 任何默认路径、录制输出、配置文件选择，都要尊重“工作目录即配置根”的仓库约定。

## ANTI-PATTERNS
- 不要在 UI 线程里做阻塞截图、校准或慢 IO；交给 probe worker 或后台 worker。
- 不要把 core 的 timing / ROI / calibration 规则复制到 app 做二次判断；app 应该展示和调度，不应该分叉算法。
- 不要让托盘/API/HUD 各自维护一套状态真相；共享状态只能有一个源头。
- 不要把 debug 面板、预览或自动选择写成只对某一后端成立；Windows、MuMu、LDPlayer、Replay 都需要经过同一路径验证。

## NOTES
- `overlay/`、`target_discovery/`、`config_wizard/` 已按职责拆成 `platform/` 子模块（薄壳 `.rs` 留跨平台 API + 非 windows stub）；`worker.rs` 是仍未拆的最大单文件。改这些前先查调用链和现有注释。
- 可视层回归可以用 `cargo run -p ruler-app --example hud_preview` 做不连模拟器的快速 smoke。
