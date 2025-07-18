import io
import socket
import struct
import subprocess
import time
from pathlib import Path
from typing import Optional

from PIL import Image

from .base import BaseCaptureController


class MinicapController(BaseCaptureController):
    """
    一个用于控制和从 Minicap 获取屏幕截图的 Python 类。
    它会自动处理设备属性检测、文件推送、服务启动和图像帧捕获。
    """

    def __init__(self, device_id: Optional[str] = None, minicap_path: str = 'controllers/minicap', local_port: int = 1717):
        """
        初始化 MinicapController。

        Args:
            device_id (str, optional): 目标设备的 ADB序列号。如果为 None，将自动选择第一个设备。
            minicap_path (str): 本地 minicap 预编译文件的根目录路径。
            local_port (int): 用于 ADB 端口转发的本地 TCP 端口。
        """
        self.device_id = device_id
        self.minicap_base_path = Path(minicap_path)
        self.local_port = local_port
        self.remote_path = "/data/local/tmp"

        self.minicap_process: Optional[subprocess.Popen] = None
        self.forward_process: Optional[subprocess.Popen] = None
        self.connection: Optional[socket.socket] = None

        self.device_info = {}
        self.banner = {}

        if not self.minicap_base_path.exists():
            raise FileNotFoundError(f"Minicap 目录未找到: {self.minicap_base_path.resolve()}")

    def _run_adb(self, command: list, check: bool = True) -> str:
        """执行一个ADB命令并返回其输出。"""
        adb_command = ["adb"]
        if self.device_id:
            adb_command.extend(["-s", self.device_id])
        adb_command.extend(command)

        result = subprocess.run(adb_command, capture_output=True, text=True, check=check, encoding='utf-8',
                                errors='ignore')
        return result.stdout.strip()

    def _get_device_properties(self):
        """获取并存储目标设备的关键属性。"""
        print(">>> 正在检测设备属性...")
        if not self.device_id:
            devices_output = self._run_adb(["devices"])
            lines = devices_output.strip().split('\n')[1:]
            if not lines or not lines[0].strip():
                raise ConnectionError("未找到任何ADB设备。请确保模拟器已运行且USB调试已开启。")
            self.device_id = lines[0].split('\t')[0]
            print(f"    自动选择设备: {self.device_id}")

        abi = self._run_adb(["shell", "getprop", "ro.product.cpu.abi"])
        sdk = self._run_adb(["shell", "getprop", "ro.build.version.sdk"])

        size_output = self._run_adb(["shell", "wm", "size"])
        try:
            physical_size_str = next(line for line in size_output.split('\n') if 'Physical size' in line)
            width, height = map(int, physical_size_str.split(':')[-1].strip().split('x'))
        except (StopIteration, ValueError):
            raise RuntimeError(f"无法从 'wm size' 的输出中解析分辨率: {size_output}")

        self.device_info = {
            'abi': abi,
            'sdk': sdk,
            'width': width,
            'height': height
        }
        print(f"    设备属性: ABI={abi}, SDK={sdk}, 分辨率={width}x{height}")

    def _push_minicap_files(self):
        """将正确的 minicap 文件推送到设备。"""
        print(">>> 正在推送 Minicap 文件...")
        abi = self.device_info['abi']
        sdk = self.device_info['sdk']

        # noinspection SpellCheckingInspection
        minicap_exec_name = "minicap-nopie" if int(sdk) < 16 else "minicap"

        local_minicap_path = self.minicap_base_path / abi / "bin" / minicap_exec_name
        local_so_path = self.minicap_base_path / abi / "lib" / f"android-{sdk}" / "minicap.so"

        if not local_minicap_path.exists():
            raise FileNotFoundError(f"Minicap 可执行文件未找到: {local_minicap_path}")
        if not local_so_path.exists():
            raise FileNotFoundError(f"Minicap .so 库文件未找到: {local_so_path}")

        self._run_adb(["push", str(local_minicap_path), f"{self.remote_path}/minicap"])
        self._run_adb(["push", str(local_so_path), f"{self.remote_path}/minicap.so"])
        self._run_adb(["shell", "chmod", "755", f"{self.remote_path}/minicap"])
        print("    文件推送成功。")

    def connect(self):
        """建立到设备的完整连接。"""
        try:
            self._get_device_properties()
            self._push_minicap_files()

            print(">>> 正在启动 Minicap 服务...")
            w, h = self.device_info['width'], self.device_info['height']
            projection = f"{w}x{h}@{w}x{h}/0"

            minicap_cmd = [
                "adb", "-s", self.device_id, "shell",
                f"LD_LIBRARY_PATH={self.remote_path}",
                f"{self.remote_path}/minicap", "-P", projection
            ]
            self.minicap_process = subprocess.Popen(minicap_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

            time.sleep(1)

            print(">>> 正在设置端口转发...")
            # noinspection SpellCheckingInspection
            self._run_adb(["forward", f"tcp:{self.local_port}", "localabstract:minicap"])

            print(">>> 正在连接到 Minicap Socket...")
            self.connection = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            self.connection.connect(("127.0.0.1", self.local_port))
            print("    连接成功！")

            self._read_global_header()

        except (subprocess.CalledProcessError, ConnectionError, FileNotFoundError, RuntimeError) as e:
            print(f"!!! 连接失败: {e}")
            self.disconnect()
            raise

    def _read_global_header(self):
        """读取并解析 Minicap 的全局头部信息。"""
        header_data = self.connection.recv(24)
        if len(header_data) != 24:
            raise ConnectionError(f"读取全局头部失败，期望24字节，实际收到{len(header_data)}字节。")

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
        print(">>> Minicap Banner 信息:")
        for key, value in self.banner.items():
            print(f"    {key}: {value}")

    def capture_frame(self) -> Image.Image:
        """
        从 Minicap 数据流中捕获一帧图像。

        Returns:
            PIL.Image.Image: 捕获到的图像帧。
        """
        if not self.connection:
            raise ConnectionError("未连接到 Minicap。请先调用 connect()。")

        frame_size_data = self.connection.recv(4)
        if not frame_size_data or len(frame_size_data) < 4:
            raise ConnectionError("连接已断开，无法读取帧大小。")
        frame_size = struct.unpack('<I', frame_size_data)[0]

        jpeg_data = b''
        while len(jpeg_data) < frame_size:
            chunk = self.connection.recv(frame_size - len(jpeg_data))
            if not chunk:
                raise ConnectionError("连接已断开，帧数据不完整。")
            jpeg_data += chunk

        return Image.open(io.BytesIO(jpeg_data))

    def disconnect(self):
        """关闭所有连接和进程，清理资源。"""
        print("\n>>> 正在断开连接并清理资源...")
        if self.connection:
            self.connection.close()
            self.connection = None
            print("    Socket 已关闭。")

        if self.minicap_process:
            self.minicap_process.terminate()
            self.minicap_process.wait()
            self.minicap_process = None
            print("    Minicap 远程进程已终止。")

        try:
            self._run_adb(["forward", "--remove", f"tcp:{self.local_port}"], check=False)
            print("    端口转发已移除。")
        except (subprocess.CalledProcessError, FileNotFoundError):
            pass

        print("    清理完成。")

    def __enter__(self):
        self.connect()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.disconnect()


if __name__ == '__main__':
    try:
        with MinicapController(minicap_path='./minicap') as cap:
            for i in range(5):
                print(f"\n--- 正在捕获第 {i + 1} 帧 ---")
                start_time = time.time()

                frame = cap.capture_frame()

                end_time = time.time()
                print(f"帧捕获成功! 分辨率: {frame.size}, 耗时: {end_time - start_time:.3f} 秒")

                save_path = f"capture_{i + 1}.jpg"
                frame.save(save_path)
                print(f"图像已保存到: {save_path}")

                time.sleep(0.5)

    except (ConnectionError, FileNotFoundError, RuntimeError) as e:
        print(f"\n!!! 程序运行出错: {e}")
    except KeyboardInterrupt:
        print("\n用户手动中断程序。")