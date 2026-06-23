# ANALYSIS KNOWLEDGE BASE

## OVERVIEW
这里把截图帧解释成“费用条像素宽度、战斗状态、逻辑帧号、边界周期和负费状态”；这是 timing、ui scaler、校准精度问题的主战场。

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 像素扫描、颜色/格式、原始宽度提取 | `scanner.rs` | 行数最大，最容易引入回归 |
| 校准模型、profile 读写、边界信息 | `calibration.rs` | 和旧文件兼容性绑得很紧 |
| 宽度到逻辑帧的映射 | `mapping.rs` | 正负费、离散/插值语义在这里汇合 |
| 结果合成、战斗状态、周期输出 | `synthesis.rs` | 最终对外展示的计时语义 |
| ROI 与 PC `ui_scaler` | `roi.rs` | PC 端缩放适配从这里入场 |
| 模块总线 | `mod.rs` | 导出面很薄，主要做组织 |

## CONVENTIONS
- timing、边界周期、ui scaler 相关改动，先对照 `docs/cost-bar-boundary-cycle.md`、相关录制和 calibration profile，再决定模型变更点。
- `roi.rs` 里的 `ui_scaler` 语义要和 `RulerConfig::effective_ui_scaler()` 保持一致；不要在别处偷偷加第二套 clamp 或默认值。
- `scanner.rs` 给出的是“证据”，`mapping.rs` / `synthesis.rs` 给出的是“解释”；不要把解释性补丁塞回扫描器。
- 旧校准文件仍需可读；新增字段或新语义时，先想清楚默认值和回放兼容路径。

## ANTI-PATTERNS
- 不要只靠扩大阈值或放宽判断来糊 timing 问题；先解释哪类宽度/状态被误判。
- 不要改边界周期或负费行为却不同时看 replay / verifier / fixtures；这类回归通常不是单点症状。
- 不要在 `scanner.rs` 里埋平台或 UI 特例；分辨率、比例、后端差异应通过 ROI、capture metadata 或校准面进入。

## NOTES
- `scanner.rs`、`calibration.rs`、`mapping.rs` 加起来就是“原始像素到逻辑帧”的完整链路；查 bug 时别只盯一个文件。
- 如果症状只在 PC 版或某个 `ui_scaler` 档位出现，先把 `roi.rs` 和输入录制对上，再怀疑扫描阈值。
- 改完分析模型后，优先找现成录制或测试面证明“为什么现在更对”，而不是只证明“还能编译”。
