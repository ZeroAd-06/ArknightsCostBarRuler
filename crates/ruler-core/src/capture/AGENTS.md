# CAPTURE KNOWLEDGE BASE

## OVERVIEW
这里负责把不同来源的画面统一成 `CapturedFrame`：模拟器官方接口、Windows PC 端窗口、通用 ADB，以及离线 replay。

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 工厂入口与统一类型面 | `mod.rs` | `CaptureConfig` / `CaptureType` / `create_backend` |
| MuMu / 雷电实现 | `mumu.rs`, `ldplayer.rs` | 安装路径、实例索引、设备信息都在这里落地 |
| 通用 ADB 路径 | `adb.rs`, `android_settings.rs` | 最慢，但也是兜底兼容面 |
| Windows PC 捕获 | `windows.rs` | 只在 Windows 编译/运行 |
| 离线视频回放 | `replay.rs` | 调试和验证的重要复现面 |

## CONVENTIONS
- 新增或调整后端选项时，先保持 `CaptureConfig` 为唯一共享配置入口，再扩散到 app / recorder。
- replay 后端必须尽量复用真实分析链路；它是排查 timing / scaler / startup 问题的常规工具，不是一次性 hack。
- `#[cfg(windows)]` 只能包平台实现，不要把非 Windows 编译路径做坏；无法运行时返回明确错误。
- target discovery、worker、recorder 都会复用这些后端；错误信息和连接语义要能被上层直接消费。

## ANTI-PATTERNS
- 不要把窗口标题、类名、设备路径等探测规则复制到多个层；共享事实应该从这里或上层 discovery 面统一维护。
- 不要吞掉连接失败原因；向导和 recorder 需要把原始失败暴露给用户或日志。
- 不要让某个后端悄悄偏离 `CapturedFrame` 契约；宽高、格式、时间语义必须一致。

## NOTES
- `create_backend()` 当前同时服务于 probe、engine 和 pipeline；这里的行为变化通常会跨多个运行面扩散。
- startup selector / probe 相关问题常常一半在这里，一半在 `crates/ruler-app/src/target_discovery/`；排查时两边一起看。
- 如果线上设备不好复现，优先保住 replay 路径可用，这通常是唯一稳定的二次验证面。
- 新增后端前先确认 recorder、app 向导和 replay 需要共用哪些能力，避免后面再回填接口。
