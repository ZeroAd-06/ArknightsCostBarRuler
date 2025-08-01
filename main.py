import ctypes
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
        controller = create_capture_controller(config)
        cap = controller.connect()
        temp_frame = cap.capture_frame()
        width, height = temp_frame.size
        # --- [核心修改] 这里的 geometry 消息现在只传递模拟器分辨率 ---
        ui_queue.put({"type": "geometry", "width": width, "height": height})

        initial_profile = config.get("active_calibration_profile")
        if initial_profile and os.path.exists(os.path.join(CALIBRATION_DIR, initial_profile)):
            command_queue.put({"type": "use_profile", "filename": initial_profile})
        else:
            ui_queue.put({"type": "state_change", "state": "idle"})
            ui_queue.put({"type": "profiles_changed"})

        while True:
            command = command_queue.get()
            if command["type"] in ["prepare_calibration", "start_calibration"]:
                timer_offset_frames = 0; cycle_counter = 0; previous_logical_frame = -1; last_known_total_frames = 0; lap_timer_active = False

            if command["type"] == "prepare_calibration":
                current_profile_filename = None; calibration_data = None
                ui_queue.put({"type": "state_change", "state": "pre_calibration"})
                continue
            elif command["type"] == "delete_profile":
                filename_to_delete = command["filename"]
                if remove_calibration_file(filename_to_delete):
                    if filename_to_delete == current_profile_filename:
                        current_profile_filename = None; calibration_data = None; config["active_calibration_profile"] = None
                        save_config(config); timer_offset_frames = 0; lap_timer_active = False
                        ui_queue.put({"type": "state_change", "state": "idle"})
                    ui_queue.put({"type": "profiles_changed"})
                continue
            elif command["type"] == "rename_profile":
                old_filename, new_basename = command["old"], command["new_base"]
                try:
                    loaded_data = load_calibration_by_filename(old_filename)
                    if loaded_data:
                        new_filename = save_calibration_data(loaded_data, loaded_data['screen_width'], loaded_data['screen_height'], basename=new_basename)
                        remove_calibration_file(old_filename)
                        if old_filename == current_profile_filename: command_queue.put({"type": "use_profile", "filename": new_filename})
                        ui_queue.put({"type": "profiles_changed"})
                except Exception as e: print(f"重命名失败: {e}")
                continue
            elif command["type"] == "start_calibration":
                ui_queue.put({"type": "state_change", "state": "calibrating"})
                try:
                    def progress_callback_for_ui(progress): ui_queue.put({"type": "calibration_progress", "progress": progress})
                    new_cal_data = calibrate(cap, progress_callback=progress_callback_for_ui)
                    should_replace_old = False
                    if calibration_data and current_profile_filename:
                        if time.time() - calibration_data.get('calibration_time', 0) < 60: should_replace_old = True
                    if should_replace_old:
                        old_basename = get_calibration_basename(current_profile_filename)
                        remove_calibration_file(current_profile_filename)
                        new_filename = save_calibration_data(new_cal_data, width, height, basename=old_basename)
                    else:
                        new_basename = f"profile_{int(time.time())}"; new_filename = save_calibration_data(new_cal_data, width, height, basename=new_basename)
                    ui_queue.put({"type": "profiles_changed"})
                    command_queue.put({"type": "use_profile", "filename": new_filename})
                except RuntimeError as e:
                    print(f"校准失败: {e}")
                    if current_profile_filename: command_queue.put({"type": "use_profile", "filename": current_profile_filename})
                    else: ui_queue.put({"type": "state_change", "state": "idle"})
                continue
            elif command["type"] == "use_profile":
                if calibration_data:
                    old_total_frames = calibration_data.get('total_frames', 30)
                    offset = cycle_counter * old_total_frames
                    timer_offset_frames += offset
                    print(f"切换配置: 保存了 {offset} 帧的偏移量。当前总偏移: {timer_offset_frames}")
                filename = command["filename"]
                new_data = load_calibration_by_filename(filename)
                if new_data:
                    calibration_data = new_data; current_profile_filename = filename; config["active_calibration_profile"] = filename; save_config(config)
                    ui_queue.put({"type": "state_change", "state": "running", "total_frames": calibration_data.get('total_frames', 30), "active_profile": current_profile_filename})
                    print(f"已切换到配置: {filename}, 开始持续分析...")
                    cycle_counter = 0; previous_logical_frame = -1; last_detection_time = time.time(); last_known_total_frames = timer_offset_frames; lap_timer_active = False
                    while True:
                        try:
                            cmd = command_queue.get_nowait()
                            if cmd.get("type") == "toggle_lap_timer":
                                if not lap_timer_active:
                                    lap_timer_active = True; lap_start_frame = last_known_total_frames
                                    print(f"单圈计时器启动，起始帧: {lap_start_frame}")
                                else:
                                    lap_timer_active = False; print("单圈计时器停止。")
                            else:
                                command_queue.put(cmd); print("分析被中断，处理新指令..."); break
                        except queue.Empty: pass
                        frame = cap.capture_frame()
                        roi = find_cost_bar_roi(width, height)
                        logical_frame = get_logical_frame_from_calibration(frame, roi, calibration_data)
                        if logical_frame is not None:
                            last_detection_time = time.time()
                            total_frames_per_cycle = calibration_data.get('total_frames', 30)
                            HIGH_ZONE_THRESHOLD, LOW_ZONE_THRESHOLD = 0.90, 0.10
                            if previous_logical_frame > total_frames_per_cycle * HIGH_ZONE_THRESHOLD and logical_frame < total_frames_per_cycle * LOW_ZONE_THRESHOLD:
                                cycle_counter += 1; print(f"费用条循环完成! 新计数值: {cycle_counter}")
                            current_total_frames = timer_offset_frames + (cycle_counter * total_frames_per_cycle) + logical_frame
                            last_known_total_frames = current_total_frames
                            previous_logical_frame = logical_frame
                        else:
                            previous_logical_frame = -1
                            if time.time() - last_detection_time > RESET_TIMEOUT:
                                if cycle_counter != 0 or last_known_total_frames != 0 or timer_offset_frames != 0:
                                    print("长时间未检测到费用条，重置循环计数器和计时器。")
                                    cycle_counter = 0; last_known_total_frames = 0; timer_offset_frames = 0; lap_timer_active = False
                        time_str = format_time_from_frames(last_known_total_frames)
                        lap_frames_to_display = None
                        if lap_timer_active: lap_frames_to_display = last_known_total_frames - lap_start_frame
                        is_running = logical_frame is not None
                        ui_update_data = {
                            "type": "update",
                            "frame": logical_frame,
                            "time_str": time_str,
                            "lap_frames": lap_frames_to_display
                        }
                        ui_queue.put(ui_update_data)

                        api_update_data = {
                            "isRunning": is_running,
                            "currentFrame": logical_frame,
                            "totalFramesInCycle": calibration_data.get('total_frames', 0) if is_running else 0,
                            "totalElapsedFrames": last_known_total_frames,
                            "activeProfile": get_calibration_basename(current_profile_filename) if current_profile_filename else None
                        }
                        # 使用 put_nowait 避免在分析线程中阻塞
                        try:
                            api_queue.put_nowait(api_update_data)
                        except queue.Full:
                            pass # 如果API服务器处理不过来，就丢弃一些帧

                        ui_queue.put({"type": "update", "frame": logical_frame, "time_str": time_str, "lap_frames": lap_frames_to_display})

                else:
                    print(f"错误: 无法加载配置 {filename}")
                    current_profile_filename = None; calibration_data = None; config["active_calibration_profile"] = None; save_config(config)
                    ui_queue.put({"type": "state_change", "state": "idle"}); ui_queue.put({"type": "profiles_changed"})
    except (ValueError, FileNotFoundError, ConnectionError, RuntimeError) as e:
        print(f"\n!!! 工作线程出错: {e}"); ui_queue.put({"type": "error", "message": str(e)})
    except KeyboardInterrupt: pass
    finally:
        if controller: controller.disconnect()

def main():
    """程序主入口。"""
    if sys.platform == "win32":
        try: ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except (AttributeError, OSError):
            try: ctypes.windll.user32.SetProcessDPIAware()
            except (AttributeError, OSError): pass
    root = ttk.Window(themename="litera")
    root.withdraw()

    config = load_config()
    if not config:
        print("未找到配置文件，启动首次设置向导...")
        config = create_config_with_gui(root)
        if not config: print("配置未完成，程序退出。"); root.destroy(); return

    ui_queue = queue.Queue(maxsize=1)
    command_queue = queue.Queue()
    api_data_queue = queue.Queue(maxsize=1)
    overlay = OverlayWindow(master_callback=command_queue.put, ui_queue=ui_queue, parent_root=root)
    start_server_in_thread(api_data_queue, port=2606)
    worker = threading.Thread(
        target=analysis_worker,
        args=(config, ui_queue, command_queue, api_data_queue),
        daemon=True
    )
    worker.start()
    print("分析工作线程已启动...")
    try: overlay.run()
    except KeyboardInterrupt: print("\n\n程序已退出。")

if __name__ == "__main__": main()