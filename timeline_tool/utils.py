import sys
import os
import logging
import datetime
from config import LOG_DIR, FPS

def resource_path(relative_path: str) -> str:
    """
    获取资源的绝对路径，无论是从源码运行还是从打包后的exe运行。
    """
    try:
        # PyInstaller 创建一个临时文件夹，并把路径存储在 _MEIPASS 中
        base_path = sys._MEIPASS
    except Exception:
        # 在开发模式下：
        # 1. os.path.abspath(__file__) 获取当前文件(utils.py)的绝对路径
        # 2. os.path.dirname(...) 获取该文件所在的目录 (ruler/)
        # 3. 再用一次 os.path.dirname(...) 返回上一级目录，即项目根目录
        script_dir = os.path.dirname(os.path.abspath(__file__))
        base_path = os.path.dirname(script_dir) if os.path.basename(script_dir) == "timeline_tool" else script_dir

    return os.path.join(base_path, relative_path)

# --- 日志系统 ---
def setup_logging(debug_image_mode=False):
    """
    配置全局日志记录器。
    """
    # 在未来的扩展中使用
    IMG_DUMP_DIR = ""
    DEBUG_IMAGE_MODE = debug_image_mode

    # 创建日志目录
    os.makedirs(LOG_DIR, exist_ok=True)
    if DEBUG_IMAGE_MODE:
        IMG_DUMP_DIR = os.path.join(LOG_DIR, "img_dumps")
        os.makedirs(IMG_DUMP_DIR, exist_ok=True)

    # 定义日志格式
    formatter = logging.Formatter(
        '%(asctime)s - %(name)-25s - %(levelname)-8s - %(message)s',
        datefmt='%Y-%m-%d %H:%M:%S'
    )

    # 获取根日志记录器
    root_logger = logging.getLogger()
    root_logger.setLevel(logging.DEBUG)

    # --- 文件处理器 ---
    log_filename = f"run_{datetime.datetime.now().strftime('%Y%m%d_%H%M%S')}.log"
    log_filepath = os.path.join(LOG_DIR, log_filename)
    file_handler = logging.FileHandler(log_filepath, encoding='utf-8')
    file_handler.setLevel(logging.DEBUG)
    file_handler.setFormatter(formatter)
    root_logger.addHandler(file_handler)

    # --- 控制台处理器 ---
    console_handler = logging.StreamHandler(sys.stdout)
    console_handler.setLevel(logging.INFO)
    console_handler.setFormatter(formatter)
    root_logger.addHandler(console_handler)

    initial_log = logging.getLogger("LoggerSetup")
    initial_log.info("=" * 60)
    initial_log.info("日志系统已初始化。")
    initial_log.info(f"日志文件将保存在: {os.path.abspath(log_filepath)}")
    if DEBUG_IMAGE_MODE:
        initial_log.info(f"调试图像将保存在: {os.path.abspath(IMG_DUMP_DIR)}")

# --- 通用工具函数 ---
def format_frame_time(total_frames):
    """将总帧数格式化为 MM:SS:FF 格式"""
    if not isinstance(total_frames, int) or total_frames < 0:
        return "--:--:--"
    total_seconds = total_frames // FPS
    minutes = total_seconds // 60
    seconds = total_seconds % 60
    frames = total_frames % FPS
    return f"{minutes:02d}:{seconds:02d}:{frames:02d}"
