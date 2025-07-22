import ctypes
import queue
import sys
import threading
import time

from calibration_manager import load_calibration_data, calibrate, save_calibration_data, remove_calibration_data
from config_manager import load_config, create_config_with_gui
from controllers import create_capture_controller
from overlay_window import OverlayWindow
from utils import find_cost_bar_roi, get_logical_frame_from_calibration


def analysis_worker(config: dict, ui_queue: queue.Queue, command_queue: queue.Queue):
    """
    在工作线程中运行的分析循环。
    它负责捕获屏幕、分析并把结果发送给UI线程。
    """
    try:
        controller = create_capture_controller(config)
        with controller as cap:
            # 获取截图尺寸并通知UI
            temp_frame = cap.capture_frame()
            width, height = temp_frame.size
            ui_queue.put({"type": "geometry", "width": width, "height": height})

            # 应用主状态机循环
            while True:
                # 检查是否存在校准数据
                calibration_data = load_calibration_data(width, height)

                if calibration_data:
                    # --- 状态: 运行中 ---
                    total_frames = calibration_data.get('total_frames', 30)
                    ui_queue.put({"type": "state_change", "state": "running", "total_frames": total_frames})

                    while True:  # 内部运行循环
                        # 检查来自UI的命令
                        try:
                            command = command_queue.get_nowait()
                            if command == "delete_calibration":
                                remove_calibration_data(width, height)
                                break  # 跳出内部循环，外部循环会重新检测并进入pre-cal状态
                        except queue.Empty:
                            pass

                        # 执行分析
                        frame = cap.capture_frame()
                        roi = find_cost_bar_roi(width, height)
                        logical_frame = get_logical_frame_from_calibration(frame, roi, calibration_data)

                        # 将结果发送给UI
                        ui_queue.put({"type": "update", "frame": logical_frame})


                else:
                    # --- 状态: 等待校准 ---
                    ui_queue.put({"type": "state_change", "state": "pre_calibration"})
                    print("等待用户发起校准...")

                    # 阻塞等待 "start_calibration" 命令
                    command = command_queue.get()
                    if command == "start_calibration":
                        ui_queue.put({"type": "state_change", "state": "calibrating"})
                        try:
                            # 校准进度回调：将进度更新任务放入UI队列
                            def progress_callback_for_ui(progress):
                                ui_queue.put({"type": "calibration_progress", "progress": progress})

                            new_cal_data = calibrate(cap, progress_callback=progress_callback_for_ui)
                            save_calibration_data(new_cal_data, width, height)
                            # 校准成功后，外部循环将自动加载新数据并进入运行状态
                        except RuntimeError as e:
                            print(f"校准失败: {e}")
                            # 保持在 pre-calibration 状态，外部循环会重新设置
                            time.sleep(5)

    except (ValueError, FileNotFoundError, ConnectionError, RuntimeError) as e:
        print(f"\n!!! 工作线程出错: {e}")
        ui_queue.put({"type": "error", "message": str(e)}) # 通知UI线程出错了
    except KeyboardInterrupt:
        # 主线程退出时，守护线程会自动终止
        pass


def main_loop():
    """主应用循环"""
    if sys.platform == "win32":
        try:
            # SHCORE.dll存在于Windows 8.1+
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except (AttributeError, OSError):
            # USER32.dll兼容旧版Windows
            try:
                ctypes.windll.user32.SetProcessDPIAware()
            except (AttributeError, OSError):
                pass # 在非常旧或非Windows系统上，优雅地失败

    # 1. 加载或创建配置 (在主线程中安全执行)
    config = load_config()
    if not config:
        print("未找到配置文件，启动首次设置向导...")
        config = create_config_with_gui() # 这个调用现在是安全的
        if not config:
            print("配置未完成，程序退出。")
            return

    # 2. 初始化UI和通信队列
    # ui_queue: 工作线程 -> UI线程 (数据和状态更新)
    # command_queue: UI线程 -> 工作线程 (用户命令)
    ui_queue = queue.Queue(maxsize=1)
    command_queue = queue.Queue()
    overlay = OverlayWindow(
        master_callback=lambda cmd: command_queue.put(cmd),
        ui_queue=ui_queue
    )

    # 3. 启动后台工作线程
    worker = threading.Thread(
        target=analysis_worker,
        args=(config, ui_queue, command_queue),
        daemon=True
    )
    worker.start()
    print("分析工作线程已启动...")

    # 4. 在主线程中运行UI
    # 这会阻塞主线程，直到UI窗口关闭，这是正确的行为
    try:
        overlay.run()
    except KeyboardInterrupt:
        print("\n\n程序已退出。")


if __name__ == "__main__":
    main_loop()