import ctypes
import logging
import sys

import ttkbootstrap as ttkb

# 确保在导入其他自定义模块前设置好日志
import utils

utils.setup_logging()

# 从项目中导入主应用类
from app import TimelineApp


def main():
    """程序主函数"""
    logger = logging.getLogger(__name__)
    logger.info("应用程序启动...")

    if sys.platform == "win32":
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
            logger.debug("设置DPI感知成功 (shcore)。")
        except (AttributeError, OSError):
            try:
                ctypes.windll.user32.SetProcessDPIAware(); logger.debug("设置DPI感知成功 (user32)。")
            except (AttributeError, OSError):
                logger.warning("设置DPI感知失败。")

    try:
        root = ttkb.Window(themename="darkly")
        app = TimelineApp(root)
        root.mainloop()
        logger.info("应用程序正常关闭。")
    except Exception as e:
        logger.critical(f"应用程序因未捕获的异常而崩溃: {e}", exc_info=True)


if __name__ == "__main__":
    main()

