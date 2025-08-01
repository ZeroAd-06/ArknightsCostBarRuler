import asyncio
import json
import websockets
import threading
from queue import Queue, Empty

# 存储所有连接的客户端
connected_clients = set()


async def handler(websocket):
    """处理新的客户端连接"""
    print(f"新客户端连接: {websocket.remote_address}")
    connected_clients.add(websocket)
    try:
        # 等待客户端连接关闭
        await websocket.wait_closed()
    finally:
        print(f"客户端断开连接: {websocket.remote_address}")
        connected_clients.remove(websocket)


async def broadcast_loop(data_queue: Queue):
    """循环广播数据给所有客户端"""
    while True:
        try:
            data = data_queue.get_nowait()
            message = json.dumps(data)

            if connected_clients:
                websockets.broadcast(connected_clients, message)

        except Empty:
            pass

        # 维持循环并让出控制权给事件循环的其他任务
        await asyncio.sleep(1 / 120)


async def main_server(data_queue: Queue, host: str, port: int):
    """服务器主函数"""
    print(f"WebSocket API 服务器正在启动于 ws://{host}:{port}")
    async with websockets.serve(handler, host, port):
        await broadcast_loop(data_queue)


def start_server_in_thread(data_queue: Queue, host='localhost', port=2606):
    """在独立的线程中启动服务器"""

    def run_server():
        asyncio.set_event_loop(asyncio.new_event_loop())
        loop = asyncio.get_event_loop()
        try:
            loop.run_until_complete(main_server(data_queue, host, port))
        finally:
            loop.close()

    thread = threading.Thread(target=run_server, daemon=True)
    thread.start()
    return thread