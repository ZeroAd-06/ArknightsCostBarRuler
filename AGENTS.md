# PROJECT KNOWLEDGE BASE

**Generated:** 2026-06-24
**Commit:** `41f7eb7`
**Branch:** `rust-rewrite`

## OVERVIEW
Windows-only Rust workspace for the Arknights cost-bar ruler. `ruler-core` owns capture/analysis/timing, `ruler-app` owns the desktop UX and local API, `ruler-recorder` drives offline evidence capture and verification.

## STRUCTURE
```text
ArknightsCostBarRuler/
├── crates/
│   ├── ruler-app/        # HUD overlay, tray, config wizard, worker, API server
│   ├── ruler-core/       # shared capture, analysis, pipeline, config, fixtures
│   ├── ruler-recorder/   # ffmpeg-backed recorder + ruler-verifier CLI
│   └── ruler-pyo3/       # Python bridge to ruler-core
├── docs/                 # architecture, API, boundary-cycle writeups
├── calibration/          # persisted calibration profiles
├── recordings/           # captured videos and offline replay inputs
├── cost_data/            # width/model datasets used during analysis work
├── log/                  # per-run logs and debug artifacts
├── ruler/locales/        # zh_CN / en_US strings shipped with the app
└── build.ps1             # build + dist packaging entry
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 启动流程、托盘、HUD 生命周期 | `crates/ruler-app/src/{main,app,worker,overlay}.rs` | UI 只读共享状态，核心逻辑不在这里算 |
| 配置向导、目标扫描、截图延迟探测 | `crates/ruler-app/src/{config_wizard,target_discovery}.rs` | 向导是单 UI 线程 + 后台 probe worker |
| 费用条识别、ROI、边界周期、负费 | `crates/ruler-core/src/analysis/` + `engine.rs` | 改这里前先看录制样本和 `docs/cost-bar-boundary-cycle.md` |
| 截图后端 / 回放后端 | `crates/ruler-core/src/capture/` | MuMu / LDPlayer / Windows / ADB / Replay 共存 |
| 帧管线与消费者协议 | `crates/ruler-core/src/pipeline/` + `docs/ARCHITECTURE.md` | `SkipToLatest` 和 `InOrder` 语义不能混 |
| 配置、工作目录、路径解析 | `crates/ruler-core/src/config.rs` + `README.md` | 当前工作目录就是默认配置根 |
| 录制、离线校验、复现问题 | `crates/ruler-recorder/src/` + `recordings/` | `ruler-verifier` 是 timing 变更的重要回归面 |
| 打包和发行目录布局 | `build.ps1` + `dist/ArknightsCostBarRuler/` | `icons/`、`ruler/locales/`、许可证必须随 exe 分发 |

## CODE MAP
| Symbol | Type | Location | Refs | Role |
|--------|------|----------|------|------|
| `RulerConfig` | struct | `crates/ruler-core/src/config.rs` | 27 | 连接工作目录配置、校准选择、UI scaler、调试开关 |
| `SharedAppState` | struct | `crates/ruler-app/src/worker.rs` | 17 | app 层的唯一共享状态面，HUD/API 只读它 |
| `CaptureConfig` | struct | `crates/ruler-core/src/capture/mod.rs` | 9 | 所有截图后端的统一配置入口 |
| `create_backend` | function | `crates/ruler-core/src/capture/mod.rs` | 7 | backend 工厂，被 engine / pipeline / probe 共同复用 |
| `WizardCore` | struct | `crates/ruler-app/src/config_wizard.rs` | 11 | 配置向导运行时状态和 probe 协调中心 |
| `FrameStore` | struct | `crates/ruler-core/src/pipeline/store.rs` | 1 direct | 帧缓存/落盘层，撑住多消费者和慢消费者 |

## CONVENTIONS
- 修 timing、ui scaler、battle-state、startup selector 之前，优先从 `log/`、`recordings/`、`cost_data/`、相关文档和现有校准文件取证，再改规则。
- 默认配置根就是进程启动时的当前工作目录；不要无意改成“永远相对 exe”或“永远相对仓库根”。
- `build.ps1` 定义了发行目录布局；新增运行时资源时，要么纳入脚本，要么明确说明为何不随包分发。
- `docs/ARCHITECTURE.md` 描述的三层边界是活文档：capture/pipeline、analysis、UI/API 之间不要偷穿透。
- 用户向 README/发行说明看到的是中文、可直接复制的命令和路径；文档改动保持这个风格。

## ANTI-PATTERNS (THIS PROJECT)
- 不要只靠“调大容差”修费用条识别；如果仓库里已有录制/CSV/分析文档，先让模型解释这些证据。
- 不要把调试入口藏成 `--debug` 专属功能；`--debug` 的职责是强制开向导并默认展开调试区，不是唯一入口。
- 不要让 HUD、托盘或 API 直接碰 capture backend / analyzer；它们应该继续只消费 `SharedAppState`。
- 不要在 app、core、pyo3 各自复制一套 capture 选择逻辑；统一从 `RulerConfig` / `CaptureConfig` 走。
- 不要改协议、校准语义或边界周期语义却不更新对应文档、验证器或测试面。

## UNIQUE STYLES
- 调试产物按会话落在 `log/<timestamp>_<pid>/`，CSV / MKV / app.log 应该能一起打包给人复现。
- README 既是用户指南，也是开发 smoke-test 清单；常用命令优先保证 README 里的写法仍然成立。
- 这个仓库长期接受“录制驱动”的修复方式：回放后端、`ruler-recorder`、`ruler-verifier` 都是日常开发面，不是边角工具。

## COMMANDS
```powershell
cargo test --workspace
cargo run --release -p ruler-app
cargo run --release -p ruler-app -- --debug
cargo run --release -p ruler-recorder -- -c config.json -o recordings -d 60
cargo run --release -p ruler-recorder --bin ruler-verifier -- <video-file>
.\build.ps1
.\build.ps1 debug
```

## NOTES
- 仓库内 `calibration/`、`cost_data/`、`recordings/`、`log/` 都可能是当前问题的证据，不是普通杂物目录。
- `crates/ruler-app/src/config_wizard.rs` 常处于高频迭代区；动向导前先看 worktree 是否已有未提交修改。
