'''
本模块的实现逻辑参考了 MaaFramework( https://github.com/MaaXYZ/MaaFramework )
因此本文件遵循LGPL-3.0协议
如果你不需要在 雷电模拟器 上使用本程序，你可以删除这一文件。
'''
import ctypes
import logging
import subprocess
import sys
from ctypes import wintypes
from pathlib import Path
from typing import Optional

from PIL import Image

try:
    from .base import BaseCaptureController
except ImportError:
    from base import BaseCaptureController

logger = logging.getLogger(__name__)



class LDPlayerObject(ctypes.Structure):
    pass

class LDPlayerVTable(ctypes.Structure):
    _fields_ = [
        # 第一个虚函数: void release(LDPlayerObject* this);
        ("release", ctypes.CFUNCTYPE(None, ctypes.POINTER(LDPlayerObject))),

        # 第二个虚函数: void* cap(LDPlayerObject* this);
        ("cap", ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.POINTER(LDPlayerObject))),
    ]

LDPlayerObject._fields_ = [
    ("vtable", ctypes.POINTER(LDPlayerVTable))
]


class LDPlayerController(BaseCaptureController):
    """
    通过加载雷电模拟器的 `ldopengl64.dll` 来获取屏幕截图。
    """

    def __init__(self, ld_install_path: str, instance_index: int, device_id: Optional[str] = None):
        if sys.platform != "win32":
            raise NotImplementedError("LDPlayerController 仅支持 Windows 平台。")

        self.install_path = Path(ld_install_path)
        self.instance_index = instance_index
        self.device_id = device_id

        self.dll: Optional[ctypes.WinDLL] = None
        self.handle: Optional[ctypes.POINTER(LDPlayerObject)] = None
        self.pid: int = 0
        self.width: int = 0
        self.height: int = 0

        logger.info(f"LDPlayerController 初始化: path='{ld_install_path}', instance={instance_index}, adb_id='{device_id}'")
        if not self.install_path.is_dir():
            raise FileNotFoundError(f"指定的雷电模拟器路径不存在或不是一个目录: {self.install_path}")

    def _run_command(self, command: list, check: bool = True) -> str:
        logger.debug(f"执行命令: {' '.join(map(str, command))}")
        startupinfo = subprocess.STARTUPINFO()
        startupinfo.dwFlags |= subprocess.STARTF_USESHOWWINDOW

        result = subprocess.run(command, capture_output=True, text=True, check=check, encoding='utf-8', errors='ignore', startupinfo=startupinfo)
        return result.stdout.strip()

    def _get_resolution_from_adb(self):
        logger.info("正在通过ADB获取模拟器分辨率...")
        adb_command_base = ["adb"]
        if self.device_id:
            adb_command_base.extend(["-s", self.device_id])

        if not self.device_id:
            logger.debug("device_id 未提供，尝试自动检测。")
            devices_output = self._run_command(adb_command_base + ["devices"])
            lines = devices_output.strip().split('\n')[1:]
            if not lines or not lines[0].strip():
                raise ConnectionError("未找到任何ADB设备，无法获取分辨率。")
            self.device_id = lines[0].split('\t')[0]
            logger.info(f"自动选择设备: {self.device_id}")
            adb_command_base.extend(["-s", self.device_id])

        size_output = self._run_command(adb_command_base + ["shell", "wm", "size"])
        try:
            physical_size_str = next(line for line in size_output.split('\n') if 'Physical size' in line)
            self.width, self.height = map(int, physical_size_str.split(':')[-1].strip().split('x'))
            logger.info(f"通过ADB获取到分辨率: {self.width}x{self.height}")
        except (StopIteration, ValueError):
            logger.error(f"无法从 'wm size' 的输出中解析分辨率: {size_output}")
            raise RuntimeError("无法通过ADB获取分辨率。")

    def _get_pid_from_dnconsole(self):
        logger.info(f"正在为实例 {self.instance_index} 查询PID...")
        dnconsole_path = self.install_path / "dnconsole.exe"
        if not dnconsole_path.exists():
            raise FileNotFoundError(f"未找到雷电命令行工具: {dnconsole_path}")

        try:
            output = self._run_command([str(dnconsole_path), "list2"])
            logger.debug(f"dnconsole list2 输出: \n{output}")

            for line in output.strip().split('\n'):
                parts = line.split(',')
                if len(parts) >= 6 and parts[0] == str(self.instance_index):
                    self.pid = int(parts[5])
                    logger.info(f"成功为实例 {self.instance_index} 找到PID: {self.pid}")
                    return

            logger.error(f"未在 dnconsole 输出中找到索引为 {self.instance_index} 的正在运行的实例。")
            raise ConnectionError(f"未找到索引为 {self.instance_index} 的正在运行的雷电实例。")

        except (subprocess.CalledProcessError, IndexError, ValueError) as e:
            logger.exception(f"解析 dnconsole.exe 输出时出错: {e}")
            raise RuntimeError("无法自动获取雷电模拟器PID。")

    def connect(self):
        logger.info("开始连接到雷电实例...")
        self._get_resolution_from_adb()
        self._get_pid_from_dnconsole()

        dll_path = self.install_path / "ldopengl64.dll"
        if not dll_path.exists():
            raise FileNotFoundError(f"未在指定路径找到 ldopengl64.dll: {dll_path}")

        logger.info(f"正在加载DLL: {dll_path}")
        self.dll = ctypes.WinDLL(str(dll_path))
        logger.info("DLL加载成功。")

        create_func = self.dll.CreateScreenShotInstance
        create_func.argtypes = [wintypes.UINT, wintypes.UINT]
        create_func.restype = ctypes.POINTER(LDPlayerObject)

        logger.info(f"正在创建雷电截图实例 (index={self.instance_index}, pid={self.pid})...")
        self.handle = create_func(self.instance_index, self.pid)

        if not self.handle:
            raise ConnectionError(f"创建雷电截图实例失败 (handle=NULL)。请检查实例索引({self.instance_index})和PID({self.pid})是否正确。")
        logger.info(f"成功创建截图实例，获得句柄: {self.handle}")

        return self

    def capture_frame(self) -> Image.Image:
        if not self.handle:
            raise ConnectionError("未连接或初始化失败。请先调用 connect()。")

        cap_func = self.handle.contents.vtable.contents.cap
        data_ptr = cap_func(self.handle)

        if not data_ptr:
            raise RuntimeError("截图失败，cap() 返回了空指针。")

        buffer_size = self.width * self.height * 3

        py_buffer = ctypes.create_string_buffer(buffer_size)
        ctypes.memmove(py_buffer, data_ptr, buffer_size)

        image = Image.frombuffer('RGB', (self.width, self.height), py_buffer, 'raw', 'BGR', 0, 1)
        return image.transpose(Image.FLIP_TOP_BOTTOM)

    def disconnect(self):
        if self.handle:
            logger.info("正在释放雷电截图实例...")
            release_func = self.handle.contents.vtable.contents.release
            release_func(self.handle)
            self.handle = None
            logger.info("实例已成功释放。")

    def __enter__(self):
        return self.connect()

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.disconnect()