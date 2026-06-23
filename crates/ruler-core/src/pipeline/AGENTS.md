# PIPELINE KNOWLEDGE BASE

## OVERVIEW
这里是 capture 和消费者之间的帧运输层：负责 frame id、缓存/落盘、named pipe 协议，以及 `InOrder` / `SkipToLatest` 消费语义。

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 生命周期总控、消费者连接、capture loop | `mod.rs` | 管线主入口 |
| 帧结构、序号、spill 元数据 | `frame.rs` | 线协议基础类型 |
| 消费者游标 | `cursor.rs` | 释放帧时机依赖它 |
| named pipe 读写协议 | `pipe.rs` | 外部消费者和进程内消费者共用 |
| 缓存/落盘策略 | `store.rs` | 慢消费者靠它保命 |

## CONVENTIONS
- capture 线程和消费者线程必须继续解耦；分析慢不应拖慢截图。
- `SkipToLatest` 供实时分析使用，`InOrder` 供 recorder / calibration / 外部统计使用；不要偷换两者语义。
- spill-to-disk 目录和会话日志目录绑定，改路径或文件格式前先考虑调试产物如何一起打包复现。
- 如果 named pipe 协议有变更，同步更新 `docs/ARCHITECTURE.md` 和任何直接消费协议的代码路径。

## ANTI-PATTERNS
- 不要让慢消费者反向阻塞 capture loop。
- 不要绕过 `FrameStore` 自己缓存帧；释放时机、spill 和多消费者引用计数都在这里统一维护。
- 不要随意改 `RFM1` / `RACK` / `RPUL` 等线协议细节；这不是纯内部实现。

## NOTES
- 这里的 bug 往往表现成“延迟飘”“帧丢失”“内存爆”“外部消费者卡死”，不一定第一时间看起来像 pipeline 问题。
- `docs/ARCHITECTURE.md` 已经把这一层画成一等公民；如果实现和文档分叉，优先修分叉而不是继续叠注释。
- 想确认问题是不是 transport 层，优先同时看 capture 速率、consumer 策略和 spill 日志，而不是只盯某个调用栈。
- 如果外部消费者要接 pipe，协议字段和 ack 时机要先定，再谈局部实现细节。
