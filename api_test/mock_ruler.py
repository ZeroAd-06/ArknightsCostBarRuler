import asyncio
import json
import time
import websockets

PORT = 2606


async def mock_data_producer(websocket):
    """模拟生成并发送数据"""
    print(f"客户端 {websocket.remote_address} 已连接到模拟桩。")
    total_frames_per_cycle = 30
    total_elapsed_frames = 0

    try:
        while True:
            current_frame_in_cycle = total_elapsed_frames % total_frames_per_cycle

            data = {
                "isRunning": True,
                "currentFrame": current_frame_in_cycle,
                "totalFramesInCycle": total_frames_per_cycle,
                "totalElapsedFrames": total_elapsed_frames,
                "activeProfile": "mock_profile_30f"
            }

            await websocket.send(json.dumps(data))

            total_elapsed_frames += 1
            await asyncio.sleep(1 / 30)  # 模拟30逻辑帧/秒
    except websockets.exceptions.ConnectionClosed:
        print(f"客户端 {websocket.remote_address} 已断开。")


async def main():
    print(f"模拟费用条尺子 API 服务器正在启动于 ws://localhost:{PORT}")
    async with websockets.serve(mock_data_producer, "localhost", PORT):
        await asyncio.Future()  # run forever


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n模拟桩已关闭。")