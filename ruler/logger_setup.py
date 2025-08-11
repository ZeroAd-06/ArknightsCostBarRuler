import logging
import os
import sys
import datetime
from typing import Optional

# 全局变量，用于控制是否启用图像转储功能
DEBUG_IMAGE_MODE = False
LOG_DIR = "../logs"
IMG_DUMP_DIR: Optional[str] = None


def setup_logging(debug_image_mode: bool = False):
    """
    配置全局日志记录器。

    Args:
        debug_image_mode (bool): 是否启用详细的图像转储日志模式。
    """
    global DEBUG_IMAGE_MODE, IMG_DUMP_DIR
    DEBUG_IMAGE_MODE = debug_image_mode

    # 创建日志目录
    os.makedirs(LOG_DIR, exist_ok=True)
    if DEBUG_IMAGE_MODE:
        IMG_DUMP_DIR = os.path.join(LOG_DIR, "img_dumps")
        os.makedirs(IMG_DUMP_DIR, exist_ok=True)

    # 定义日志格式
    formatter = logging.Formatter(
        '%(asctime)s - %(name)-35s - %(levelname)-8s - %(message)s',
        datefmt='%Y-%m-%d %H:%M:%S'
    )

    # 获取根日志记录器
    root_logger = logging.getLogger()
    root_logger.setLevel(logging.DEBUG)  # 设置最低级别为DEBUG，以捕获所有级别的日志

    # --- 文件处理器 ---
    # 创建带时间戳的日志文件名
    log_filename = f"run_{datetime.datetime.now().strftime('%Y%m%d_%H%M%S')}.log"
    file_handler = logging.FileHandler(os.path.join(LOG_DIR, log_filename), encoding='utf-8')
    file_handler.setLevel(logging.DEBUG)  # 文件中记录所有DEBUG及以上级别的信息
    file_handler.setFormatter(formatter)
    root_logger.addHandler(file_handler)

    # --- 控制台处理器 ---
    console_handler = logging.StreamHandler(sys.stdout)
    # 控制台只显示INFO及以上级别的信息，避免过于嘈杂
    console_handler.setLevel(logging.INFO)
    console_handler.setFormatter(formatter)
    root_logger.addHandler(console_handler)

    initial_log = logging.getLogger("LoggerSetup")
    initial_log.info("=" * 60)
    initial_log.info("日志系统已初始化。")
    initial_log.info(f"日志文件位置: {os.path.join(LOG_DIR, log_filename)}")
    if DEBUG_IMAGE_MODE:
        initial_log.warning("!!! 图像转储调试模式已启用 !!!")
        initial_log.info(f"调试图像将保存到: {IMG_DUMP_DIR}")
    else:
        initial_log.info("图像转储调试模式已禁用。")
    initial_log.info("=" * 60)