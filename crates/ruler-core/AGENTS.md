# RULER-CORE KNOWLEDGE BASE

## OVERVIEW
共享核心 crate：定义配置、截图后端、帧管线、费用条分析、计时引擎，并为 app、recorder、PyO3 暴露统一能力。

## STRUCTURE
```text
crates/ruler-core/
├── src/analysis/   # ROI, scanner, calibration, mapping, synthesis
├── src/capture/    # adb / mumu / ldplayer / windows / replay backends
├── src/pipeline/   # frame store, named pipe transport, consumer cursors
├── src/engine.rs   # RulerEngine facade and runtime orchestration
├── src/config.rs   # persisted config + CaptureConfig conversion
├── examples/       # bench_real_env / collect_cost_data / infer_calibration
└── tests/          # fixture-backed regression tests
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 配置读写、路径兼容、capture 类型解析 | `src/config.rs` | app / recorder / pyo3 都走这里 |
| 高层引擎入口、连接、单帧分析、计时累计 | `src/engine.rs` | app 和 recorder 都依赖它 |
| 费用条模型和 ROI | `src/analysis/` | timing/scaler/boundary-cycle 改动先看这里 |
| 截图后端和回放后端 | `src/capture/` | target discovery 与 replay 都复用 |
| 多消费者帧管线 | `src/pipeline/` | `SkipToLatest` / `InOrder` 是基础契约 |
| 可复现实验与回归样本 | `examples/`, `tests/fixtures/` | 改核心规则时优先补这里 |

## CONVENTIONS
- `RulerConfig` / `CaptureConfig` 是跨 crate 的单一配置面；新增 capture 选项时先打通这里，再接 app 或 pyo3。
- 能用 replay、fixture、example 驱动的问题，优先走这些可重复面，不要一上来改线上 heuristics。
- 三层架构边界保持明确：capture/pipeline 只产帧，analysis/engine 只解释帧，app/recorder 只消费结果。
- 回放后端不是临时调试分支，而是正式后端之一；改 capture/analysis 时要考虑 replay 是否仍能复现同样结果。
- 新的核心行为如果改变文档化语义，更新 `docs/ARCHITECTURE.md` 或相关说明，而不是只留在代码注释里。

## ANTI-PATTERNS
- 不要在 app、recorder、pyo3 分别拼接一套 backend 初始化逻辑；统一复用 core 工厂。
- 不要把“某条录制能过”当成算法正确；看 `tests/fixtures/`、`cost_data/`、回放验证是否一起成立。
- 不要让 UI 或 Windows-only 细节渗进 `analysis/`；平台分支应该停在 capture 层或更外侧。
- 不要随意改 calibration JSON 兼容路径；仓库明确要求旧校准继续可读。

## NOTES
- 大文件热点是 `engine.rs`、`analysis/scanner.rs`、`pipeline/mod.rs`、`capture/windows.rs`；这里最容易藏全局副作用。
- 如果一个核心改动需要 recorder、verifier、pyo3 同时适配，先把接口面稳定下来，再分别补外层调用。
