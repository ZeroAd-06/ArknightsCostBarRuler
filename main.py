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

    frame_display_mode = config.get('frame_display_mode', '0_to_n-1')

    controller = None
    cap = None
    width, height = 0, 0

    current_profile_filename = None
    calibration_data = None

    cycle_counter = 0
    cycle_base_frames = 0
    timer_offset_frames = 0
    last_known_total_frames = 0

    lap_timer_active = False
    lap_start_frame = 0

    previous_logical_frame = -1
    last_detection_time = time.time()
    RESET_TIMEOUT = 3.0

    try:
        worker_logger.info("正在创建截图控制器...")
        controller = create_capture_controller(config)
        worker_logger.info("正在连接到设备...")
        cap = controller.connect()
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
            ui_queue.put({"type": "state_change", "state": "idle", "display_mode": frame_display_mode})
            ui_queue.put({"type": "profiles_changed"})

        while True:
            worker_logger.debug("等待下一条指令...")
            command = command_queue.get()
            worker_logger.info(f"收到指令: {command}")

            # [注意] 外层循环现在只处理“主要指令”

            if command["type"] in ["prepare_calibration", "start_calibration", "delete_profile"]:
                worker_logger.debug("因校准或删除操作，重置所有计时器状态。")
                timer_offset_frames, cycle_base_frames, cycle_counter = 0, 0, 0
                previous_logical_frame, last_known_total_frames = -1, 0
                lap_timer_active = False

            if command["type"] == "prepare_calibration":
                current_profile_filename, calibration_data = None, None
                ui_queue.put({"type": "state_change", "state": "pre_calibration", "display_mode": frame_display_mode})
                continue

            elif command["type"] == "delete_profile":
                filename_to_delete = command["filename"]
                if remove_calibration_file(filename_to_delete):
                    if filename_to_delete == current_profile_filename:
                        worker_logger.info("删除了当前正在使用的配置文件，重置状态。")
                        current_profile_filename, calibration_data = None, None
                        config["active_calibration_profile"] = None
                        save_config(config)
                        ui_queue.put({"type": "state_change", "state": "idle", "display_mode": frame_display_mode})
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
                ui_queue.put({"type": "state_change", "state": "calibrating", "display_mode": frame_display_mode})
                try:
                    new_cal_data = calibrate(cap, progress_callback=lambda p: ui_queue.put(
                        {"type": "calibration_progress", "progress": p}))
                    new_basename = f"profile_{int(time.time())}"
                    new_filename = save_calibration_data(new_cal_data, width, height, basename=new_basename)
                    ui_queue.put({"type": "profiles_changed"})
                    command_queue.put({"type": "use_profile", "filename": new_filename})
                except RuntimeError as e:
                    worker_logger.error(f"校准失败: {e}")
                    if current_profile_filename:
                        command_queue.put({"type": "use_profile", "filename": current_profile_filename})
                    else:
                        ui_queue.put({"type": "state_change", "state": "idle", "display_mode": frame_display_mode})
                continue

            elif command["type"] == "use_profile":
                if calibration_data:
                    offset = cycle_base_frames
                    timer_offset_frames += offset
                    worker_logger.info(f"切换配置: 保存了 {offset} 帧的偏移量。当前总偏移: {timer_offset_frames}")

                filename = command["filename"]
                new_data = load_calibration_by_filename(filename)

                if new_data and new_data.get('profiles'):
                    calibration_data = new_data
                    current_profile_filename = filename
                    config["active_calibration_profile"] = filename
                    save_config(config)

                    cycle_counter, cycle_base_frames = 0, 0
                    previous_logical_frame = -1
                    last_detection_time = time.time()
                    last_known_total_frames = timer_offset_frames
                    lap_timer_active = False

                    initial_profile = calibration_data['profiles'][0]
                    total_f_initial = initial_profile.get('total_frames', 30)
                    display_total_initial = f"/{total_f_initial - 1}" if frame_display_mode == '0_to_n-1' else f"/{total_f_initial}"
                    ui_queue.put({"type": "state_change", "state": "running", "display_total": display_total_initial,
                                  "active_profile": current_profile_filename, "display_mode": frame_display_mode})

                    worker_logger.info(
                        f"已切换到配置: {filename} (含 {len(calibration_data['profiles'])} 个模型), 开始持续分析...")

                    frame_counter = 0
                    while True:
                        # --- [核心修复] ---
                        try:
                            cmd = command_queue.get_nowait()
                            worker_logger.info(f"分析循环中收到新指令: {cmd}")

                            cmd_type = cmd.get("type")
                            # 定义不需要中断循环的“次要指令”
                            MINOR_COMMANDS = ["toggle_lap_timer", "set_display_mode"]

                            if cmd_type in MINOR_COMMANDS:
                                # 在循环内部处理次要指令
                                if cmd_type == "toggle_lap_timer":
                                    lap_timer_active = not lap_timer_active
                                    if lap_timer_active:
                                        lap_start_frame = last_known_total_frames
                                        worker_logger.info(f"单圈计时器启动，起始帧: {lap_start_frame}")
                                    else:
                                        worker_logger.info("单圈计时器停止。")

                                elif cmd_type == "set_display_mode":
                                    new_mode = cmd["mode"]
                                    if new_mode != frame_display_mode:
                                        frame_display_mode = new_mode
                                        config['frame_display_mode'] = new_mode
                                        save_config(config)
                                        ui_queue.put({"type": "mode_changed", "mode": new_mode})

                                continue  # <-- 处理完次要指令后，继续内循环

                            # 如果是其他“主要指令”，则放回队列并中断
                            worker_logger.info(f"指令 '{cmd_type}' 需要中断分析，正在退出循环。")
                            command_queue.put(cmd)
                            break
                        except queue.Empty:
                            pass
                        # --- [结束修复] ---

                        frame = cap.capture_frame()
                        frame_counter += 1
                        roi = find_cost_bar_roi(width, height)

                        num_profiles = len(calibration_data['profiles'])
                        current_profile_index = cycle_counter % num_profiles
                        active_profile = calibration_data['profiles'][current_profile_index]

                        logical_frame = get_logical_frame_from_calibration(
                            frame, roi, active_profile,
                            dump_prefix=f"run_frame_{frame_counter}"
                        )

                        if logical_frame is not None:
                            last_detection_time = time.time()
                            total_frames_this_cycle = active_profile.get('total_frames', 30)

                            if previous_logical_frame > total_frames_this_cycle * 0.9 and logical_frame < total_frames_this_cycle * 0.1:
                                worker_logger.info(
                                    f"费用条循环 {cycle_counter} 完成! (周期长度: {total_frames_this_cycle} 帧)")
                                cycle_base_frames += total_frames_this_cycle
                                cycle_counter += 1

                            current_total_frames = timer_offset_frames + cycle_base_frames + logical_frame
                            last_known_total_frames = current_total_frames
                            previous_logical_frame = logical_frame
                        else:
                            previous_logical_frame = -1
                            if time.time() - last_detection_time > RESET_TIMEOUT:
                                if cycle_counter or cycle_base_frames or timer_offset_frames:
                                    worker_logger.warning("长时间未检测到费用条，重置所有计时器。")
                                    cycle_counter, cycle_base_frames, timer_offset_frames = 0, 0, 0
                                    last_known_total_frames = 0
                                    lap_timer_active = False

                        time_str = format_time_from_frames(last_known_total_frames)
                        lap_frames_to_display = last_known_total_frames - lap_start_frame if lap_timer_active else None

                        total_f_active = active_profile.get('total_frames', 30)
                        display_total_text = f"/{total_f_active - 1}" if frame_display_mode == '0_to_n-1' else f"/{total_f_active}"
                        display_frame_text = logical_frame + 1 if frame_display_mode == '1_to_n' and logical_frame is not None else (
                            logical_frame if logical_frame is not None else "--")

                        ui_update_data = {"type": "update", "display_frame": display_frame_text,
                                          "display_total": display_total_text, "time_str": time_str,
                                          "lap_frames": lap_frames_to_display}
                        try:
                            ui_queue.put_nowait(ui_update_data)
                        except queue.Full:
                            pass

                        api_update_data = {"isRunning": logical_frame is not None, "currentFrame": logical_frame,
                                           "totalFramesInCycle": total_f_active if logical_frame is not None else 0,
                                           "totalElapsedFrames": last_known_total_frames,
                                           "activeProfile": get_calibration_basename(current_profile_filename)}
                        try:
                            api_queue.put_nowait(api_update_data)
                        except queue.Full:
                            pass

                else:
                    worker_logger.error(f"无法加载配置文件 {filename}")
                    current_profile_filename, calibration_data = None, None
                    config["active_calibration_profile"] = None
                    save_config(config)
                    ui_queue.put({"type": "state_change", "state": "idle", "display_mode": frame_display_mode})
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
        '--debug-img', action='store_true',
        help='启用详细的图像转储日志功能，用于调试。图像将保存到 logs/img_dumps/ 目录。'
    )
    args = parser.parse_args()

    setup_logging(debug_image_mode=args.debug_img)
    logger.info("程序启动...")

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
            # 检查当前是否为管理员
            is_admin = ctypes.windll.shell32.IsUserAnAdmin() != 0
        except Exception:
            is_admin = False  # 如果检查失败，保守地认为不是管理员

        if not is_admin:
            print("权限不足，正在尝试以管理员身份重新启动...")
            try:
                # 使用 ShellExecuteW 提权并重新运行脚本
                ctypes.windll.shell32.ShellExecuteW(
                    None,  # hwnd
                    "runas",  # lpOperation
                    sys.executable,  # lpFile
                    " ".join(sys.argv),  # lpParameters
                    None,  # lpDirectory
                    1  # nShowCmd
                )
            except Exception as e:
                print(f"自动提权失败: {e}")
                print("请手动右键，选择“以管理员身份运行”本程序。")
                input("按回车键退出...")
                sys.exit(0)  # 退出当前的非管理员进程


    root = ttk.Window(themename="litera")
    root.withdraw()

    config = load_config()
    if not config:
        logger.info("未找到配置文件，启动首次设置向导...")
        config = create_config_with_gui(root)
        if not config:
            logger.info("配置未完成，程序退出。");
            root.destroy();
            return

    if 'frame_display_mode' not in config: config['frame_display_mode'] = '0_to_n-1'

    ui_queue, command_queue, api_data_queue = queue.Queue(maxsize=1), queue.Queue(), queue.Queue(maxsize=1)
    overlay = OverlayWindow(master_callback=command_queue.put, ui_queue=ui_queue, parent_root=root)

    api_port = config.get("api_port", 2606)
    start_server_in_thread(api_data_queue, port=api_port)

    worker = threading.Thread(target=analysis_worker, args=(config, ui_queue, command_queue, api_data_queue),
                              daemon=True, name="AnalysisWorkerThread")
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