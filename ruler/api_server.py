import asyncio
import json
import logging
import websockets
import threading
from queue import Queue, Empty

logger = logging.getLogger(__name__)

# 存储所有连接的客户端
connected_clients = set()


async def handler(websocket):
    """处理新的客户端连接"""
    logger.info(f"新客户端连接: {websocket.remote_address}")
    connected_clients.add(websocket)
    try:
        # 等待客户端连接关闭
        await websocket.wait_closed()
    finally:
        logger.info(f"客户端断开连接: {websocket.remote_address}")
        connected_clients.remove(websocket)


async def broadcast_loop(data_queue: Queue):
    """循环广播数据给所有客户端"""
    logger.debug("广播循环已启动，等待数据...")
    while True:
        try:
            # 使用 get_nowait 避免阻塞事件循环
            data = data_queue.get_nowait()
            message = json.dumps(data)
            logger.debug(f"从队列获取到数据，准备广播: {message}")

            if connected_clients:
                websockets.broadcast(connected_clients, message)
                logger.debug(f"已向 {len(connected_clients)} 个客户端广播消息。")

        except Empty:
            # 队列为空是正常情况，短暂休眠以让出CPU
            pass
        except Exception as e:
            logger.error(f"广播循环中发生错误: {e}")

        # 维持循环并让出控制权给事件循环的其他任务
        # 120Hz 的休眠时间
        await asyncio.sleep(1 / 120)


async def main_server(data_queue: Queue, host: str, port: int):
    """服务器主函数"""
    logger.info(f"WebSocket API 服务器正在启动于 ws://{host}:{port}")
    try:
        async with websockets.serve(handler, host, port):
            await broadcast_loop(data_queue)
    except Exception as e:
        logger.exception(f"WebSocket 服务器启动或运行时发生严重错误: {e}")


def start_server_in_thread(data_queue: Queue, host='localhost', port=2606):
    """在独立的线程中启动服务器"""

    def run_server():
        logger.debug("API服务器线程已启动。")
        # 为新线程设置新的事件循环
        asyncio.set_event_loop(asyncio.new_event_loop())
        loop = asyncio.get_event_loop()
        try:
            loop.run_until_complete(main_server(data_queue, host, port))
        finally:
            logger.info("API服务器事件循环已关闭。")
            loop.close()

    thread = threading.Thread(target=run_server, daemon=True, name="ApiServerThread")
    thread.start()
    logger.info("API 服务器线程已成功启动。")
    return thread
