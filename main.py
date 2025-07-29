import ctypes
import queue
import sys
import threading
import time
import os

from calibration_manager import (load_calibration_by_filename, calibrate, save_calibration_data,
                                 remove_calibration_file, get_calibration_basename,
                                 CALIBRATION_DIR)
from config_manager import load_config, create_config_with_gui, save_config
from controllers import create_capture_controller
from overlay_window import OverlayWindow
from utils import find_cost_bar_roi, get_logical_frame_from_calibration


def analysis_worker(config: dict, ui_queue: queue.Queue, command_queue: queue.Queue):
    """
    在工作线程中运行的分析循环。
    它负责捕获屏幕、分析并把结果发送给UI线程。
    """
    controller = None
    cap = None
    width, height = 0, 0
    current_profile_filename = None
    calibration_data = None

    try:
        # --- 阶段 1: 初始化 ---
        controller = create_capture_controller(config)
        cap = controller.connect()
        temp_frame = cap.capture_frame()
        width, height = temp_frame.size
        ui_queue.put({"type": "geometry", "width": width, "height": height})

        # --- 阶段 2: 尝试加载上次使用的配置 ---
        initial_profile = config.get("active_calibration_profile")
        if initial_profile and os.path.exists(os.path.join(CALIBRATION_DIR, initial_profile)):
            command_queue.put({"type": "use_profile", "filename": initial_profile})
        else:
            ui_queue.put({"type": "state_change", "state": "idle"})
            ui_queue.put({"type": "profiles_changed"})

        # --- 阶段 3: 主事件循环 ---
        while True:
            # 阻塞等待一个指令，这是事件驱动的起点
            command = command_queue.get()

            # --- 指令处理 ---

            # --- 指令: 删除校准配置 ---
            if command["type"] == "delete_profile":
                filename_to_delete = command["filename"]
                if remove_calibration_file(filename_to_delete):
                    print(f"已删除配置: {filename_to_delete}")
                    if filename_to_delete == current_profile_filename:
                        current_profile_filename = None
                        calibration_data = None
                        config["active_calibration_profile"] = None
                        save_config(config)
                        ui_queue.put({"type": "state_change", "state": "idle"})
                    ui_queue.put({"type": "profiles_changed"})
                continue  # 处理完指令后，等待下一个

            # --- 指令: 重命名校准配置 ---
            elif command["type"] == "rename_profile":
                old_filename = command["old"]
                new_basename = command["new_base"]
                try:
                    loaded_data = load_calibration_by_filename(old_filename)
                    if loaded_data:
                        new_filename = save_calibration_data(
                            loaded_data,
                            loaded_data['screen_width'], loaded_data['screen_height'], basename=new_basename
                        )
                        remove_calibration_file(old_filename)
                        print(f"'{old_filename}' 已重命名为 '{new_filename}'")
                        if old_filename == current_profile_filename:
                            command_queue.put({"type": "use_profile", "filename": new_filename})
                        ui_queue.put({"type": "profiles_changed"})
                except Exception as e:
                    print(f"重命名失败: {e}")
                continue  # 处理完指令后，等待下一个

            # --- 指令: 开始校准 ---
            elif command["type"] == "start_calibration":
                ui_queue.put({"type": "state_change", "state": "calibrating"})
                try:
                    def progress_callback_for_ui(progress):
                        ui_queue.put({"type": "calibration_progress", "progress": progress})

                    new_cal_data = calibrate(cap, progress_callback=progress_callback_for_ui)

                    should_replace_old = False
                    if calibration_data:
                        time_diff = time.time() - calibration_data.get('calibration_time', 0)
                        if time_diff < 60:
                            should_replace_old = True

                    if should_replace_old:
                        old_basename = get_calibration_basename(current_profile_filename)
                        remove_calibration_file(current_profile_filename)
                        new_filename = save_calibration_data(new_cal_data, width, height, basename=old_basename)
                    else:
                        new_basename = f"profile_{int(time.time())}"
                        new_filename = save_calibration_data(new_cal_data, width, height, basename=new_basename)

                    ui_queue.put({"type": "profiles_changed"})
                    # 校准后自动选用
                    command_queue.put({"type": "use_profile", "filename": new_filename})

                except RuntimeError as e:
                    print(f"校准失败: {e}")
                    if current_profile_filename:
                        command_queue.put({"type": "use_profile", "filename": current_profile_filename})
                    else:
                        ui_queue.put({"type": "state_change", "state": "idle"})
                continue  # 处理完指令后，等待下一个

            # --- 指令: 使用校准配置 (这将启动持续分析) ---
            elif command["type"] == "use_profile":
                filename = command["filename"]
                new_data = load_calibration_by_filename(filename)
                if new_data:
                    calibration_data = new_data
                    current_profile_filename = filename
                    config["active_calibration_profile"] = filename
                    save_config(config)
                    ui_queue.put({
                        "type": "state_change",
                        "state": "running",
                        "total_frames": calibration_data.get('total_frames', 30),
                        "active_profile": current_profile_filename
                    })
                    print(f"已切换到配置: {filename}, 开始持续分析...")

                    # --- 持续分析的内部循环 ---
                    while True:
                        # 检查是否有新的指令来中断分析
                        try:
                            interrupt_command = command_queue.get_nowait()
                            command_queue.put(interrupt_command)  # 把指令放回去，让外层循环处理
                            print("分析被中断，处理新指令...")
                            break  # 跳出内部循环
                        except queue.Empty:
                            pass  # 没有新指令，继续分析

                        # 执行分析
                        frame = cap.capture_frame()
                        roi = find_cost_bar_roi(width, height)
                        logical_frame = get_logical_frame_from_calibration(frame, roi, calibration_data)
                        ui_queue.put({"type": "update", "frame": logical_frame})

                        time.sleep(0.02)  # 控制刷新率，避免CPU占用过高
                else:
                    # 加载失败
                    print(f"错误: 无法加载配置 {filename}")
                    current_profile_filename = None
                    calibration_data = None
                    config["active_calibration_profile"] = None
                    save_config(config)
                    ui_queue.put({"type": "state_change", "state": "idle"})
                    ui_queue.put({"type": "profiles_changed"})


    except (ValueError, FileNotFoundError, ConnectionError, RuntimeError) as e:
        print(f"\n!!! 工作线程出错: {e}")
        ui_queue.put({"type": "error", "message": str(e)})
    except KeyboardInterrupt:
        pass
    finally:
        if controller:  # 使用controller来断开，因为它肯定存在
            controller.disconnect()


def main_loop():
    """主应用循环"""
    if sys.platform == "win32":
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except (AttributeError, OSError):
            try:
                ctypes.windll.user32.SetProcessDPIAware()
            except (AttributeError, OSError):
                pass

    config = load_config()
    if not config:
        print("未找到配置文件，启动首次设置向导...")
        config = create_config_with_gui()
        if not config:
            print("配置未完成，程序退出。")
            return

    ui_queue = queue.Queue(maxsize=1)
    command_queue = queue.Queue()
    overlay = OverlayWindow(
        master_callback=command_queue.put,
        ui_queue=ui_queue
    )

    worker = threading.Thread(
        target=analysis_worker,
        args=(config, ui_queue, command_queue),
        daemon=True
    )
    worker.start()
    print("分析工作线程已启动...")

    try:
        overlay.run()
    except KeyboardInterrupt:
        print("\n\n程序已退出。")


if __name__ == "__main__":
    main_loop()