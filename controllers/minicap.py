import io
import logging
import socket
import struct
import subprocess
import time
from pathlib import Path
from typing import Optional

from PIL import Image

from .base import BaseCaptureController
from utils import resource_path

logger = logging.getLogger(__name__)


class MinicapController(BaseCaptureController):
    """
    一个用于控制和从 Minicap 获取屏幕截图的 Python 类。
    它会自动处理设备属性检测、文件推送、服务启动和图像帧捕获。
    """

    def __init__(self, device_id: Optional[str] = None, minicap_path: str = 'controllers/minicap',
                 local_port: int = 1717):
        """
        初始化 MinicapController。

        Args:
            device_id (str, optional): 目标设备的 ADB序列号。如果为 None，将自动选择第一个设备。
            minicap_path (str): 本地 minicap 预编译文件的根目录路径。
            local_port (int): 用于 ADB 端口转发的本地 TCP 端口。
        """
        self.device_id = device_id
        self.minicap_base_path = Path(resource_path(minicap_path))
        self.local_port = local_port
        self.remote_path = "/data/local/tmp"

        self.minicap_process: Optional[subprocess.Popen] = None
        self.forward_process: Optional[subprocess.Popen] = None
        self.connection: Optional[socket.socket] = None

        self.device_info = {}
        self.banner = {}

        logger.info(f"MinicapController 初始化: device_id={device_id}, port={local_port}")
        if not self.minicap_base_path.exists():
            logger.critical(f"Minicap 目录未找到: {self.minicap_base_path.resolve()}")
            raise FileNotFoundError(f"Minicap 目录未找到: {self.minicap_base_path.resolve()}")

    def _run_adb(self, command: list, check: bool = True) -> str:
        """执行一个ADB命令并返回其输出。"""
        adb_command = ["adb"]
        if self.device_id:
            adb_command.extend(["-s", self.device_id])
        adb_command.extend(command)

        logger.debug(f"执行ADB命令: {' '.join(adb_command)}")
        result = subprocess.run(adb_command, capture_output=True, text=True, check=check, encoding='utf-8',
                                errors='ignore')
        logger.debug(f"ADB命令输出: {result.stdout.strip()}")
        return result.stdout.strip()

    def _get_device_properties(self):
        """获取并存储目标设备的关键属性。"""
        logger.info("正在检测设备属性...")
        if not self.device_id:
            logger.debug("device_id 未提供，尝试自动检测。")
            devices_output = self._run_adb(["devices"])
            lines = devices_output.strip().split('\n')[1:]
            if not lines or not lines[0].strip():
                logger.error("未找到任何ADB设备。")
                raise ConnectionError("未找到任何ADB设备。请确保模拟器已运行且USB调试已开启。")
            self.device_id = lines[0].split('\t')[0]
            logger.info(f"自动选择设备: {self.device_id}")

        abi = self._run_adb(["shell", "getprop", "ro.product.cpu.abi"])
        sdk = self._run_adb(["shell", "getprop", "ro.build.version.sdk"])

        size_output = self._run_adb(["shell", "wm", "size"])
        try:
            physical_size_str = next(line for line in size_output.split('\n') if 'Physical size' in line)
            width, height = map(int, physical_size_str.split(':')[-1].strip().split('x'))
        except (StopIteration, ValueError):
            logger.error(f"无法从 'wm size' 的输出中解析分辨率: {size_output}")
            raise RuntimeError(f"无法从 'wm size' 的输出中解析分辨率: {size_output}")

        self.device_info = {
            'abi': abi,
            'sdk': sdk,
            'width': width,
            'height': height
        }
        logger.info(f"设备属性: ABI={abi}, SDK={sdk}, 分辨率={width}x{height}")

    def _push_minicap_files(self):
        """将正确的 minicap 文件推送到设备。"""
        logger.info("正在推送 Minicap 文件...")
        abi = self.device_info['abi']
        sdk = self.device_info['sdk']

        # noinspection SpellCheckingInspection
        minicap_exec_name = "minicap-nopie" if int(sdk) < 16 else "minicap"
        logger.debug(f"根据SDK版本 {sdk} 选择 Minicap 可执行文件: {minicap_exec_name}")

        local_minicap_path = self.minicap_base_path / abi / "bin" / minicap_exec_name
        local_so_path = self.minicap_base_path / abi / "lib" / f"android-{sdk}" / "minicap.so"

        if not local_minicap_path.exists():
            logger.error(f"Minicap 可执行文件未找到: {local_minicap_path}")
            raise FileNotFoundError(f"Minicap 可执行文件未找到: {local_minicap_path}")
        if not local_so_path.exists():
            raise FileNotFoundError(f"Minicap .so 库文件未找到: {local_so_path}")

        logger.debug(f"推送 {local_minicap_path} -> {self.remote_path}/minicap")
        self._run_adb(["push", str(local_minicap_path), f"{self.remote_path}/minicap"])
        logger.debug(f"推送 {local_so_path} -> {self.remote_path}/minicap.so")
        self._run_adb(["push", str(local_so_path), f"{self.remote_path}/minicap.so"])
        logger.debug("设置 Minicap 可执行权限...")
        self._run_adb(["shell", "chmod", "755", f"{self.remote_path}/minicap"])
        logger.info("Minicap 文件推送成功。")

    def connect(self):
        """建立到设备的完整连接。"""
        logger.info("开始建立到 Minicap 的完整连接...")
        try:
            self._get_device_properties()
            self._push_minicap_files()

            logger.info("正在启动 Minicap 服务...")
            w, h = self.device_info['width'], self.device_info['height']
            projection = f"{w}x{h}@{w}x{h}/0"
            logger.debug(f"使用投影参数: {projection}")

            minicap_cmd = [
                "adb", "-s", self.device_id, "shell",
                f"LD_LIBRARY_PATH={self.remote_path}",
                f"{self.remote_path}/minicap", "-P", projection
            ]
            self.minicap_process = subprocess.Popen(minicap_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            logger.debug(f"Minicap 进程已启动，PID: {self.minicap_process.pid}")

            time.sleep(1)  # 等待服务启动

            logger.info("正在设置端口转发...")
            # noinspection SpellCheckingInspection
            self._run_adb(["forward", f"tcp:{self.local_port}", "localabstract:minicap"])
            logger.debug(f"端口转发: tcp:{self.local_port} -> localabstract:minicap")

            logger.info("正在连接到 Minicap Socket...")
            self.connection = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            self.connection.connect(("127.0.0.1", self.local_port))
            logger.info(f"成功连接到 127.0.0.1:{self.local_port}")

            self._read_global_header()

            logger.info("Minicap 连接成功建立！")
            return self

        except (subprocess.CalledProcessError, ConnectionError, FileNotFoundError, RuntimeError) as e:
            logger.exception("Minicap 连接失败！")
            self.disconnect()
            raise

    def _read_global_header(self):
        """读取并解析 Minicap 的全局头部信息。"""
        logger.debug("正在读取 Minicap 全局头部信息 (24字节)...")
        header_data = self.connection.recv(24)
        if len(header_data) != 24:
            msg = f"读取全局头部失败，期望24字节，实际收到{len(header_data)}字节。"
            logger.error(msg)
            raise ConnectionError(msg)
        logger.debug(f"收到的原始头部数据: {header_data.hex()}")

        # '<' 表示小端序
        # B=unsigned char (1), I=unsigned int (4)
        # 修正: 'I' 的数量从4个增加到5个
        # noinspection SpellCheckingInspection
        header_format = '<BBIIIIIBB'
        unpacked_data = struct.unpack(header_format, header_data)

        self.banner = {
            'version': unpacked_data[0],
            'header_size': unpacked_data[1],
            'pid': unpacked_data[2],
            'real_width': unpacked_data[3],
            'real_height': unpacked_data[4],
            'virtual_width': unpacked_data[5],
            'virtual_height': unpacked_data[6],
            'orientation': unpacked_data[7],
            'quirks': unpacked_data[8],
        }
        logger.info("Minicap Banner 信息已解析:")
        for key, value in self.banner.items():
            logger.debug(f"    {key}: {value}")

    def capture_frame(self) -> Image.Image:
        """
        从 Minicap 数据流中捕获一帧图像。

        Returns:
            PIL.Image.Image: 捕获到的图像帧。
        """
        if not self.connection:
            raise ConnectionError("未连接到 Minicap。请先调用 connect()。")

        logger.debug("等待下一帧数据...")
        # 1. 读取帧大小
        frame_size_data = self.connection.recv(4)
        if not frame_size_data or len(frame_size_data) < 4:
            raise ConnectionError("连接已断开，无法读取帧大小。")
        frame_size = struct.unpack('<I', frame_size_data)[0]
        logger.debug(f"接收到帧头，图像大小: {frame_size} 字节")

        # 2. 读取完整的图像数据
        jpeg_data = b''
        bytes_left = frame_size
        while bytes_left > 0:
            chunk = self.connection.recv(bytes_left)
            if not chunk:
                raise ConnectionError("连接已断开，帧数据不完整。")
            jpeg_data += chunk
            bytes_left -= len(chunk)

        logger.debug(f"已接收完整的帧数据 ({len(jpeg_data)} 字节)，正在解码为图像...")
        image = Image.open(io.BytesIO(jpeg_data))
        logger.debug(f"图像解码成功，分辨率: {image.size}")
        return image

    def disconnect(self):
        """关闭所有连接和进程，清理资源。"""
        logger.info("正在断开连接并清理 Minicap 资源...")
        if self.connection:
            try:
                self.connection.close()
                self.connection = None
                logger.debug("Socket 已关闭。")
            except Exception as e:
                logger.warning(f"关闭 socket 时出错: {e}")

        if self.minicap_process:
            try:
                self.minicap_process.terminate()
                self.minicap_process.wait(timeout=2)
                self.minicap_process = None
                logger.debug("Minicap 远程进程已终止。")
            except Exception as e:
                logger.warning(f"终止 Minicap 进程时出错: {e}")

        try:
            logger.debug(f"正在移除端口转发: tcp:{self.local_port}")
            self._run_adb(["forward", "--remove", f"tcp:{self.local_port}"], check=False)
            logger.debug("端口转发已移除。")
        except (subprocess.CalledProcessError, FileNotFoundError):
            logger.warning("移除端口转发失败，可能已被移除或ADB服务已停止。")
        except Exception as e:
            logger.error(f"移除端口转发时发生未知错误: {e}")

        logger.info("Minicap 清理完成。")

    def __enter__(self):
        self.connect()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.disconnect()


if __name__ == '__main__':
    # 注意：为了在独立运行时看到日志，需要手动配置
    from logger_setup import setup_logging

    setup_logging(debug_image_mode=True)
    main_logger = logging.getLogger(__name__)

    try:
        with MinicapController(minicap_path='./minicap') as cap:
            for i in range(5):
                main_logger.info(f"--- 正在捕获第 {i + 1} 帧 ---")
                start_time = time.time()

                frame = cap.capture_frame()

                end_time = time.time()
                main_logger.info(f"帧捕获成功! 分辨率: {frame.size}, 耗时: {end_time - start_time:.3f} 秒")

                save_path = f"capture_{i + 1}.jpg"
                frame.save(save_path)
                main_logger.info(f"图像已保存到: {save_path}")

                time.sleep(0.5)

    except (ConnectionError, FileNotFoundError, RuntimeError) as e:
        main_logger.exception(f"程序运行出错: {e}")
    except KeyboardInterrupt:
        main_logger.info("用户手动中断程序。")
