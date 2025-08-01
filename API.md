# WebSocket API 文档

你好呀！本文档旨在帮助你构建与“明日方舟费用条尺子”进行交互的应用程序。

## 概述

本工具通过一个本地 WebSocket 服务器，实时广播其分析出的游戏状态数据。任何支持 WebSocket 的程序都可以作为客户端连接并接收这些数据，从而实现与尺子功能的联动。

*   **技术:** WebSocket
*   **数据格式:** JSON
*   **通信模式:** 服务器推送。客户端只需连接并监听。

## 连接详情

*   **Endpoint URL:** `ws://localhost:2606`
*   **端口:** `2606` (TCP)

客户端应该实现断线重连逻辑，以便在尺子程序重启后能自动恢复连接。

## 数据包格式

服务器推送的每一条消息都是一个UTF-8编码的JSON字符串。JSON对象包含以下字段：

| 键 (Key)               | 类型 (Type)       | 描述                                                                             | 示例值 (Running) | 示例值 (Idle) |
| ---------------------- | ----------------- |--------------------------------------------------------------------------------| ---------------- | ------------- |
| `isRunning`            | `boolean`         | 尺子当前是否识别有效的费用条。如果为 `false`，表示费用条可能被锁住、不存在或费用已满。                                | `true`           | `false`         |
| `currentFrame`         | `integer` 或 `null` | 当前费用循环内的逻辑帧数（从0开始）。当 `isRunning` 为 `false` 时，此值为 `null`。                       | `15`             | `null`          |
| `totalFramesInCycle`   | `integer`         | 当前校准配置下的单次费用回复循环总帧数。当 `isRunning` 为 `false` 时，此值为 `0`。                         | `30`             | `0`             |
| `totalElapsedFrames`   | `integer`         | 从当前配置启动开始，累计经过的总逻辑帧数。用于驱动 `分:秒:帧` 计时器。当 `isRunning` 为 `false` 时，此值会保持在上一个有效状态。 | `75`             | `75`            |
| `activeProfile`        | `string` 或 `null` | 当前加载的校准配置的基础名称。如果没有加载配置，此值为 `null`。                                            | `"正常回费"`      | `null`          |

### 消息示例

#### 1. 当尺子正常运行时

```json
{
    "isRunning": true,
    "currentFrame": 15,
    "totalFramesInCycle": 30,
    "totalElapsedFrames": 75,
    "activeProfile": "正常回费"
}
```

#### 2. 当尺子未检测到费用条时

```json
{
    "isRunning": false,
    "currentFrame": null,
    "totalFramesInCycle": 0,
    "totalElapsedFrames": 75,
    "activeProfile": "正常回费"
}
```

## Python 客户端使用示例

你可以使用 `websockets` 库轻松地连接和接收数据。你可以在 `api_test/api_client_example.py` 找到使用 Python 构建的客户端示例。


## 版本与兼容性

本文档适用于 **v1.1.1** 及以上版本的“明日方舟费用条尺子”。
未来的更新可能会在JSON对象中**增加**新的字段，但会尽力保证现有字段的**向后兼容性**。