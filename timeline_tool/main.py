import ctypes
import logging
import sys

import ttkbootstrap as ttkb

import utils

utils.setup_logging()


from i18n import i18n
from app import TimelineApp



def main():
    """程序主函数"""
    logger = logging.getLogger(__name__)
    i18n.load_locale(i18n.auto_detect_language())
    logger.info(f"应用程序启动... Lang: {i18n.current_locale}")

    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
        logger.debug("设置DPI感知成功 (Per Monitor v2)。")
    except (AttributeError, OSError):
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
            logger.debug("设置DPI感知成功 (System DPI Aware)。")
        except (AttributeError, OSError):
            try:
                ctypes.windll.user32.SetProcessDPIAware()
                logger.debug("设置DPI感知成功 (兼容模式)。")
            except (AttributeError, OSError):
                logger.warning("设置DPI感知失败。")

    if sys.platform == "win32":
        try:
            is_admin = ctypes.windll.shell32.IsUserAnAdmin() != 0
        except Exception:
            is_admin = False

        if not is_admin:
            logger.info("权限不足，正在尝试以管理员身份重新启动...")
            try:
                params = " ".join(sys.argv)
                ret = ctypes.windll.shell32.ShellExecuteW(
                    None, "runas", sys.executable, params, None, 1
                )
                if int(ret) > 32:
                    sys.exit(0)
            except Exception as e:
                logger.error(f"自动提权失败: {e}")
                sys.exit(0)

    try:
        root = ttkb.Window(themename="darkly")

        scaling_factor = root.tk.call('tk', 'scaling')
        logger.info(f"检测到系统DPI缩放比例为: {scaling_factor:.2f} ({int(scaling_factor * 100)}%)")
        root.tk.call('tk', 'scaling', scaling_factor)

        # 将缩放比例传递给主应用类
        app = TimelineApp(root, scaling_factor=scaling_factor)

        root.mainloop()
        logger.info("应用程序正常关闭。")
    except Exception as e:
        logger.critical(f"应用程序因未捕获的异常而崩溃: {e}", exc_info=True)


if __name__ == "__main__":
    main()

