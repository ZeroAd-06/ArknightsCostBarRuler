# RULER-CORE KNOWLEDGE BASE

## OVERVIEW
共享核心 crate：定义配置、截图后端、帧管线、费用条分析、计时引擎，并为 app、recorder 暴露统一能力。

## STRUCTURE
```text
crates/ruler-core/
├── src/analysis/   # ROI, scanner, calibration, synthesis
├── src/capture/    # adb / mumu / ldplayer / windows / replay backends
├── src/pipeline/   # frame store, named pipe transport, consumer cursors
├── src/engine.rs   # Analyzer: 战斗状态机 + fp24 前向重同步计时核
├── src/fp24.rs     # 定点数 (2^-24) 费用累加器，复刻游戏内部计时
├── src/config.rs   # persisted config + CaptureConfig conversion
├── examples/       # bench_real_env / collect_cost_data / infer_calibration
└── tests/          # fixture-backed regression tests
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 配置读写、路径兼容、capture 类型解析 | `src/config.rs` | app / recorder 都走这里 |
| 单帧分析、计时累计、相位重同步 | `src/engine.rs` (Analyzer) + `src/fp24.rs` | 每帧把 fp24 累加器前向模拟到与观测像素最匹配处 |
| 费用条模型和 ROI | `src/analysis/` | timing/scaler/几何改动先看这里;边界周期/交替由 `fp24.rs` 自然产生 |
| 截图后端和回放后端 | `src/capture/` | target discovery 与 replay 都复用 |
| 多消费者帧管线 | `src/pipeline/` | `SkipToLatest` / `InOrder` 是基础契约 |
| 可复现实验与回归样本 | `examples/`, `tests/fixtures/` | 改核心规则时优先补这里 |

## CONVENTIONS
- `RulerConfig` / `CaptureConfig` 是跨 crate 的单一配置面；新增 capture 选项时先打通这里，再接 app 或 recorder。
- 能用 replay、fixture、example 驱动的问题，优先走这些可重复面，不要一上来改线上 heuristics。
- 三层架构边界保持明确：capture/pipeline 只产帧，analysis/engine 只解释帧，app/recorder 只消费结果。
- 回放后端不是临时调试分支，而是正式后端之一；改 capture/analysis 时要考虑 replay 是否仍能复现同样结果。
- 新的核心行为如果改变文档化语义，更新 `docs/ARCHITECTURE.md` 或相关说明，而不是只留在代码注释里。

## ANTI-PATTERNS
- 不要在 app、recorder 分别拼接一套 backend 初始化逻辑；统一复用 core 工厂。
- 不要把“某条录制能过”当成算法正确；看 `tests/fixtures/`、`cost_data/`、回放验证是否一起成立。
- 不要让 UI 或 Windows-only 细节渗进 `analysis/`；平台分支应该停在 capture 层或更外侧。
- 校准 JSON 按 `format_version`(当前为 4)校验;只存一个标量 `required`(费用回复所需数值)。旧/无版本格式不再支持(加载失败的文件会被跳过、不进 UI)。改 schema 时升版本号,不要加旧格式兼容分支。

## NOTES
- 大文件热点是 `engine.rs`、`analysis/scanner/`、`pipeline/mod.rs`、`capture/windows.rs`；这里最容易藏全局副作用。
- 如果一个核心改动需要 recorder、verifier 同时适配，先把接口面稳定下来，再分别补外层调用。
