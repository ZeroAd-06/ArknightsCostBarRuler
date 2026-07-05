# ANALYSIS KNOWLEDGE BASE

## OVERVIEW
这里把截图帧解释成“费用条像素宽度、战斗状态、逻辑帧号、边界周期和负费状态”；这是 timing、ui scaler、校准精度问题的主战场。

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 费用条宽度提取、负费(负号)检测 | `scanner/cost_bar.rs` | 直接读 ROI 像素 |
| 战斗状态识别(速度/暂停/开局/善后) | `scanner/battle_state.rs` | 右上角 HUD 分类 |
| 校准模型(标量 `required`)、profile 读写、推断 | `calibration.rs` | 按 `format_version` 校验，只认当前格式 |
| 相位→像素宽度几何、pixel_map 合成 | `synthesis.rs` | `synthesized_width_for_phase` 是运行时渲染与校准合成共用的核 |
| 宽度→逻辑帧、正负费、边界周期 | `../engine.rs` + `../fp24.rs` | fp24 前向重同步核；边界/交替由定点截断自然产生 |
| ROI 与 PC `ui_scaler` | `roi.rs` | PC 端缩放适配从这里入场 |
| 模块总线 | `mod.rs` | 导出面很薄，主要做组织 |

## CONVENTIONS
- timing、边界周期、ui scaler 相关改动，先对照 `docs/cost-bar-boundary-cycle.md`、相关录制和 calibration profile，再决定模型变更点。边界周期/交替现在由 `../fp24.rs` 定点累加器自然产生，不再有魔数或多 profile。
- `roi.rs` 里的 `ui_scaler` 语义要和 `RulerConfig::effective_ui_scaler()` 保持一致；不要在别处偷偷加第二套 clamp 或默认值。
- `scanner/` 给出的是“证据”，`synthesis.rs`(几何)+ `../fp24.rs`/`../engine.rs`(计时)给出的是“解释”；不要把解释性补丁塞回扫描器。
- 校准文件按 `format_version`(当前为 4)校验，只存标量 `required`；不再兼容旧/无版本格式，改 schema 时升版本号，加载失败的文件会被跳过、不进 UI。

## ANTI-PATTERNS
- 不要只靠扩大阈值或放宽判断来糊 timing 问题；先解释哪类宽度/状态被误判。
- 不要改边界周期或负费行为却不同时看 replay / verifier / fixtures；这类回归通常不是单点症状。
- 不要在 `scanner/` 里埋平台或 UI 特例；分辨率、比例、后端差异应通过 ROI、capture metadata 或校准面进入。

## NOTES
- `scanner/`(取证)、`synthesis.rs`(几何)、`../fp24.rs` + `../engine.rs`(计时)加起来就是“原始像素到逻辑帧”的完整链路；查 bug 时别只盯一个文件。
- 如果症状只在 PC 版或某个 `ui_scaler` 档位出现，先把 `roi.rs` 和输入录制对上，再怀疑扫描阈值。
- 改完分析模型后，优先找现成录制或测试面证明“为什么现在更对”，而不是只证明“还能编译”。
