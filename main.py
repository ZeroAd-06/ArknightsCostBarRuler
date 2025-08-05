import argparse
import ctypes
import logging
import queue
import sys
import threading
import time
import os
import ttkbootstrap as ttk

from calibration_manager import (load_calibration_by_filename, calibrate, save_calibration_data,
                                 remove_calibration_file, get_calibration_basename,
                                 CALIBRATION_DIR)
from config_manager import load_config, create_config_with_gui, save_config
from controllers import create_capture_controller
from overlay_window import OverlayWindow
from utils import find_cost_bar_roi, get_logical_frame_from_calibration
from api_server import start_server_in_thread
from logger_setup import setup_logging

# 在所有其他导入之后获取logger实例
logger = logging.getLogger(__name__)

FRAMES_PER_SECOND = 30


def format_time_from_frames(total_frames: int) -> str:
    """将总逻辑帧数格式化为 MM:SS:FF 字符串。"""
    if total_frames < 0: return "00:00:00"
    frames = total_frames % FRAMES_PER_SECOND
    total_seconds = total_frames // FRAMES_PER_SECOND
    minutes = total_seconds // 60
    seconds = total_seconds % 60
    return f"{minutes:02d}:{seconds:02d}:{frames:02d}"


def analysis_worker(config: dict, ui_queue: queue.Queue, command_queue: queue.Queue, api_queue: queue.Queue):
    """在工作线程中运行的分析循环。"""
    worker_logger = logging.getLogger("AnalysisWorker")
    worker_logger.info("分析工作线程已启动。")

    controller = None
    cap = None
    width, height = 0, 0
    current_profile_filename = None
    calibration_data = None
    cycle_counter = 0
    previous_logical_frame = -1
    last_detection_time = time.time()
    RESET_TIMEOUT = 3.0
    timer_offset_frames = 0
    last_known_total_frames = 0
    lap_timer_active = False
    lap_start_frame = 0

    try:
        worker_logger.info("正在创建截图控制器...")
        controller = create_capture_controller(config) # config中已包含instance_index
        worker_logger.info("正在连接到设备...")
        cap = controller.connect()
        # ... (后续代码无需修改) ...
        worker_logger.info("连接成功，捕获测试帧以获取分辨率...")
        temp_frame = cap.capture_frame()
        width, height = temp_frame.size
        worker_logger.info(f"获取到模拟器分辨率: {width}x{height}")

        ui_queue.put({"type": "geometry", "width": width, "height": height})

        initial_profile = config.get("active_calibration_profile")
        if initial_profile and os.path.exists(os.path.join(CALIBRATION_DIR, initial_profile)):
            worker_logger.info(f"找到上次使用的有效配置文件: {initial_profile}，将自动加载。")
            command_queue.put({"type": "use_profile", "filename": initial_profile})
        else:
            worker_logger.info("未找到上次使用的配置文件，进入空闲状态。")
            ui_queue.put({"type": "state_change", "state": "idle"})
            ui_queue.put({"type": "profiles_changed"})

        while True:
            worker_logger.debug("等待下一条指令...")
            command = command_queue.get()
            worker_logger.info(f"收到指令: {command}")

            if command["type"] in ["prepare_calibration", "start_calibration"]:
                worker_logger.debug("重置计时器状态以进行校准。")
                timer_offset_frames = 0;
                cycle_counter = 0;
                previous_logical_frame = -1;
                last_known_total_frames = 0;
                lap_timer_active = False

            if command["type"] == "prepare_calibration":
                current_profile_filename = None;
                calibration_data = None
                ui_queue.put({"type": "state_change", "state": "pre_calibration"})
                continue
            elif command["type"] == "delete_profile":
                filename_to_delete = command["filename"]
                if remove_calibration_file(filename_to_delete):
                    if filename_to_delete == current_profile_filename:
                        worker_logger.info("删除了当前正在使用的配置文件，重置状态。")
                        current_profile_filename = None;
                        calibration_data = None;
                        config["active_calibration_profile"] = None
                        save_config(config);
                        timer_offset_frames = 0;
                        lap_timer_active = False
                        ui_queue.put({"type": "state_change", "state": "idle"})
                    ui_queue.put({"type": "profiles_changed"})
                continue
            elif command["type"] == "rename_profile":
                old_filename, new_basename = command["old"], command["new_base"]
                worker_logger.info(f"准备重命名 '{old_filename}' 为 '{new_basename}'")
                try:
                    loaded_data = load_calibration_by_filename(old_filename)
                    if loaded_data:
                        new_filename = save_calibration_data(loaded_data, loaded_data['screen_width'],
                                                             loaded_data['screen_height'], basename=new_basename)
                        remove_calibration_file(old_filename)
                        worker_logger.info(f"重命名成功，新文件为 '{new_filename}'")
                        if old_filename == current_profile_filename:
                            command_queue.put({"type": "use_profile", "filename": new_filename})
                        ui_queue.put({"type": "profiles_changed"})
                except Exception as e:
                    worker_logger.exception(f"重命名失败: {e}")
                continue
            elif command["type"] == "start_calibration":
                ui_queue.put({"type": "state_change", "state": "calibrating"})
                try:
                    def progress_callback_for_ui(progress):
                        ui_queue.put({"type": "calibration_progress", "progress": progress})

                    new_cal_data = calibrate(cap, progress_callback=progress_callback_for_ui)
                    should_replace_old = False
                    if calibration_data and current_profile_filename:
                        if time.time() - calibration_data.get('calibration_time', 0) < 60: should_replace_old = True
                    if should_replace_old:
                        worker_logger.info("检测到快速重校准，将覆盖旧配置文件。")
                        old_basename = get_calibration_basename(current_profile_filename)
                        remove_calibration_file(current_profile_filename)
                        new_filename = save_calibration_data(new_cal_data, width, height, basename=old_basename)
                    else:
                        new_basename = f"profile_{int(time.time())}";
                        new_filename = save_calibration_data(new_cal_data, width, height, basename=new_basename)
                    ui_queue.put({"type": "profiles_changed"})
                    command_queue.put({"type": "use_profile", "filename": new_filename})
                except RuntimeError as e:
                    worker_logger.error(f"校准失败: {e}")
                    if current_profile_filename:
                        command_queue.put({"type": "use_profile", "filename": current_profile_filename})
                    else:
                        ui_queue.put({"type": "state_change", "state": "idle"})
                continue
            elif command["type"] == "use_profile":
                if calibration_data:
                    old_total_frames = calibration_data.get('total_frames', 30)
                    offset = cycle_counter * old_total_frames
                    timer_offset_frames += offset
                    worker_logger.info(f"切换配置: 保存了 {offset} 帧的偏移量。当前总偏移: {timer_offset_frames}")
                filename = command["filename"]
                new_data = load_calibration_by_filename(filename)
                if new_data:
                    calibration_data = new_data;
                    current_profile_filename = filename;
                    config["active_calibration_profile"] = filename;
                    save_config(config)
                    ui_queue.put({"type": "state_change", "state": "running",
                                  "total_frames": calibration_data.get('total_frames', 30),
                                  "active_profile": current_profile_filename})
                    worker_logger.info(f"已切换到配置: {filename}, 开始持续分析...")
                    cycle_counter = 0;
                    previous_logical_frame = -1;
                    last_detection_time = time.time();
                    last_known_total_frames = timer_offset_frames;
                    lap_timer_active = False;
                    frame_counter = 0
                    while True:
                        try:
                            cmd = command_queue.get_nowait()
                            worker_logger.info(f"分析循环中收到新指令: {cmd}，中断当前分析。")
                            if cmd.get("type") == "toggle_lap_timer":
                                if not lap_timer_active:
                                    lap_timer_active = True;
                                    lap_start_frame = last_known_total_frames
                                    worker_logger.info(f"单圈计时器启动，起始帧: {lap_start_frame}")
                                else:
                                    lap_timer_active = False;
                                    worker_logger.info("单圈计时器停止。")
                            else:
                                command_queue.put(cmd);
                                break
                        except queue.Empty:
                            pass

                        frame = cap.capture_frame()
                        frame_counter += 1
                        roi = find_cost_bar_roi(width, height)

                        logical_frame = get_logical_frame_from_calibration(
                            frame, roi, calibration_data,
                            dump_prefix=f"run_frame_{frame_counter}"
                        )

                        if logical_frame is not None:
                            last_detection_time = time.time()
                            total_frames_per_cycle = calibration_data.get('total_frames', 30)
                            HIGH_ZONE_THRESHOLD, LOW_ZONE_THRESHOLD = 0.90, 0.10
                            if previous_logical_frame > total_frames_per_cycle * HIGH_ZONE_THRESHOLD and logical_frame < total_frames_per_cycle * LOW_ZONE_THRESHOLD:
                                cycle_counter += 1;
                                worker_logger.info(f"费用条循环完成! 新计数值: {cycle_counter}")
                            current_total_frames = timer_offset_frames + (
                                        cycle_counter * total_frames_per_cycle) + logical_frame
                            last_known_total_frames = current_total_frames
                            previous_logical_frame = logical_frame
                        else:
                            previous_logical_frame = -1
                            if time.time() - last_detection_time > RESET_TIMEOUT:
                                if cycle_counter != 0 or last_known_total_frames != 0 or timer_offset_frames != 0:
                                    worker_logger.warning("长时间未检测到费用条，重置循环计数器和计时器。")
                                    cycle_counter = 0;
                                    last_known_total_frames = 0;
                                    timer_offset_frames = 0;
                                    lap_timer_active = False
                        time_str = format_time_from_frames(last_known_total_frames)
                        lap_frames_to_display = None
                        if lap_timer_active: lap_frames_to_display = last_known_total_frames - lap_start_frame
                        is_running = logical_frame is not None

                        # --- UI 更新 ---
                        ui_update_data = {"type": "update", "frame": logical_frame, "time_str": time_str,
                                          "lap_frames": lap_frames_to_display}
                        try:
                            ui_queue.put_nowait(ui_update_data)
                        except queue.Full:
                            # UI处理不过来，丢弃一些帧以避免卡顿
                            pass

                        # --- API 更新 ---
                        api_update_data = {
                            "isRunning": is_running,
                            "currentFrame": logical_frame,
                            "totalFramesInCycle": calibration_data.get('total_frames', 0) if is_running else 0,
                            "totalElapsedFrames": last_known_total_frames,
                            "activeProfile": get_calibration_basename(
                                current_profile_filename) if current_profile_filename else None
                        }
                        try:
                            api_queue.put_nowait(api_update_data)
                        except queue.Full:
                            pass  # API服务器处理不过来，也丢弃一些帧
                else:
                    worker_logger.error(f"无法加载配置文件 {filename}")
                    current_profile_filename = None;
                    calibration_data = None;
                    config["active_calibration_profile"] = None;
                    save_config(config)
                    ui_queue.put({"type": "state_change", "state": "idle"});
                    ui_queue.put({"type": "profiles_changed"})
    except (ValueError, FileNotFoundError, ConnectionError, RuntimeError) as e:
        worker_logger.exception(f"工作线程发生严重错误，即将终止: {e}")
        ui_queue.put({"type": "error", "message": str(e)})
    except KeyboardInterrupt:
        worker_logger.info("工作线程被键盘中断。")
    finally:
        if controller:
            worker_logger.info("正在断开控制器连接...")
            controller.disconnect()
        worker_logger.info("分析工作线程已结束。")


def main():
    """程序主入口。"""
    parser = argparse.ArgumentParser(description="Arknights Cost Bar Ruler")
    parser.add_argument(
        '--debug-img',
        action='store_true',
        help='启用详细的图像转储日志功能，用于调试。图像将保存到 logs/img_dumps/ 目录。'
    )
    args = parser.parse_args()

    # **必须在所有其他操作之前设置日志记录**
    setup_logging(debug_image_mode=args.debug_img)

    logger.info("程序启动...")

    if sys.platform == "win32":
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
            logger.debug("设置DPI感知成功 (shcore)。")
        except (AttributeError, OSError):
            try:
                ctypes.windll.user32.SetProcessDPIAware()
                logger.debug("设置DPI感知成功 (user32)。")
            except (AttributeError, OSError):
                logger.warning("设置DPI感知失败。")

    root = ttk.Window(themename="litera")
    root.withdraw()

    config = load_config()
    if not config:
        logger.info("未找到配置文件，启动首次设置向导...")
        config = create_config_with_gui(root)
        if not config:
            logger.info("配置未完成，程序退出。")
            root.destroy()
            return

    logger.info("正在初始化队列和窗口...")
    ui_queue = queue.Queue(maxsize=1)
    command_queue = queue.Queue()
    api_data_queue = queue.Queue(maxsize=1)
    overlay = OverlayWindow(master_callback=command_queue.put, ui_queue=ui_queue, parent_root=root)

    logger.info("正在启动API服务器线程...")
    # 从配置中读取端口号，如果不存在则使用默认值 2606
    api_port = config.get("api_port", 2606)
    start_server_in_thread(api_data_queue, port=api_port)

    logger.info("正在启动分析工作线程...")
    worker = threading.Thread(
        target=analysis_worker,
        args=(config, ui_queue, command_queue, api_data_queue),
        daemon=True,
        name="AnalysisWorkerThread"
    )
    worker.start()

    logger.info("启动主事件循环 (Overlay)...")
    try:
        overlay.run()
    except KeyboardInterrupt:
        logger.info("检测到键盘中断，正在退出程序...")
    except Exception as e:
        logger.exception("主线程发生未捕获的异常！")
    finally:
        logger.info("程序已结束。")


if __name__ == "__main__":
    main()

