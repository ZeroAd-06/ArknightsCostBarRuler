# overlay_window.py (最终版)

import os
import queue
import tkinter as tk
from typing import Optional

from PIL import Image, ImageTk

from utils import find_cost_bar_roi, resource_path

class OverlayWindow:
    """
    一个悬浮窗，用于显示费用条信息。
    通过队列与工作线程安全通信。
    """

    def __init__(self, master_callback, ui_queue: queue.Queue):
        self.root = None
        self.master_callback = master_callback
        self.ui_queue = ui_queue
        self.icons = {}
        self._drag_data = {"x": 0, "y": 0}

    def _load_icons(self):
        """加载所有需要的图标资源"""
        try:
            icon_names = ["start", "wait", "deco"]
            for name in icon_names:
                path = resource_path(os.path.join("icons", f"{name}.png"))
                img = Image.open(path).convert("RGBA")
                self.icons[name] = ImageTk.PhotoImage(image=img)
        except FileNotFoundError as e:
            print(f"错误: 缺少图标文件 {e.filename}。")
            img = Image.new("RGBA", (64, 64), (255, 0, 0, 128))
            self.icons["error"] = ImageTk.PhotoImage(image=img)

    def _create_widgets(self):
        """创建所有UI组件"""
        self.root.config(bg='#3a3a3a')
        self.left_frame = tk.Frame(self.root, bg=self.root.cget('bg'))
        self.left_frame.place(relx=0, rely=0, relwidth=0.33, relheight=1.0)
        self.icon_button = tk.Button(self.left_frame, borderwidth=0, highlightthickness=0,
                                     bg=self.root.cget('bg'), activebackground='gray30')
        self.icon_button.pack(expand=True, fill="both")
        self.right_frame = tk.Frame(self.root, bg=self.root.cget('bg'))
        self.right_frame.place(relx=0.33, rely=0, relwidth=0.67, relheight=1.0)
        self.right_frame.bind("<ButtonPress-1>", self._on_drag_start)
        self.right_frame.bind("<ButtonRelease-1>", self._on_drag_stop)
        self.right_frame.bind("<B1-Motion>", self._on_drag_motion)
        self.pre_cal_label = tk.Label(self.right_frame, text="选中干员\n点击左侧按钮", fg="white",
                                      bg=self.root.cget('bg'), font=("Segoe UI", 11))
        self.cal_progress_label = tk.Label(self.right_frame, text="0%", fg="white", bg=self.root.cget('bg'),
                                           font=("Segoe UI", 34))
        self.running_frame_label = tk.Label(self.right_frame, text="--", fg="white", bg=self.root.cget('bg'),
                                            font=("Segoe UI", 34))
        self.running_total_label = tk.Label(self.root, text="/--", fg="gray60", bg=self.root.cget('bg'),
                                            font=("Segoe UI", 12))

    def setup_geometry(self, screen_width, screen_height):
        """根据屏幕分辨率计算并设置窗口大小和位置"""
        # 使用从截图模块获取的尺寸来计算ROI和窗口大小，这保证了与游戏内容的比例正确
        roi_x1, roi_x2, _ = find_cost_bar_roi(screen_width, screen_height)
        cost_bar_pixel_length = roi_x2 - roi_x1
        win_width = int(cost_bar_pixel_length * 5 / 6)
        win_height = int(win_width * 27 / 50)

        # --- [核心修正] ---
        # 使用Tkinter自己获取的、经操作系统缩放调整后的实际屏幕尺寸来定位窗口
        # 这确保窗口永远不会被放置到屏幕以外
        actual_screen_width = self.root.winfo_screenwidth()
        actual_screen_height = self.root.winfo_screenheight()
        pos_x = actual_screen_width - win_width - 50
        pos_y = actual_screen_height - win_height - 100
        # --- [修正结束] ---

        self.root.geometry(f"{win_width}x{win_height}+{pos_x}+{pos_y}")
        self._resize_icons(win_height)
        self.root.deiconify()

    def _resize_icons(self, height):
        """根据窗口高度调整图标大小"""
        size = int(height * 0.8)
        try:
            for name in ["start", "wait", "deco"]:
                path = resource_path(os.path.join("icons", f"{name}.png"))
                img = Image.open(path).resize((size, size), Image.Resampling.LANCZOS)
                self.icons[name] = ImageTk.PhotoImage(image=img)
        except Exception as e:
            print(f"调整图标大小时出错: {e}")

    def _hide_all_dynamic_labels(self):
        self.pre_cal_label.place_forget()
        self.cal_progress_label.place_forget()
        self.running_frame_label.place_forget()
        self.running_total_label.place_forget()

    def set_state_pre_calibration(self):
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('start'),
                                command=lambda: self.master_callback("start_calibration"))
        self.pre_cal_label.place(relx=0.5, rely=0.5, anchor="center")

    def set_state_calibrating(self):
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('wait'), command=None)
        self.cal_progress_label.place(relx=0.5, rely=0.5, anchor="center")

    def update_calibration_progress(self, percentage: float):
        self.cal_progress_label.config(text=f"{int(percentage)}%")

    def set_state_running(self, total_frames: int):
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('deco'),
                                command=lambda: self.master_callback("delete_calibration"))
        self.running_total_label.config(text=f"/{total_frames - 1}")
        self.running_frame_label.place(relx=1.0, rely=0.5, anchor='e', x=-40)
        self.running_total_label.place(relx=1.0, rely=1.0, anchor='se', x=-5, y=-5)

    def update_running_display(self, current_frame: Optional[int]):
        if current_frame is not None:
            self.running_frame_label.config(text=f"{current_frame}")
        else:
            self.running_frame_label.config(text="--")

    def _process_ui_queue(self):
        try:
            message = self.ui_queue.get_nowait()
            msg_type = message.get("type")
            if msg_type == "geometry":
                self.setup_geometry(message["width"], message["height"])
            elif msg_type == "state_change":
                state = message["state"]
                if state == "running":
                    self.set_state_running(message["total_frames"])
                elif state == "pre_calibration":
                    self.set_state_pre_calibration()
                elif state == "calibrating":
                    self.set_state_calibrating()
            elif msg_type == "update":
                self.update_running_display(message["frame"])
            elif msg_type == "calibration_progress":
                self.update_calibration_progress(message["progress"])
            elif msg_type == "error":
                self.pre_cal_label.config(text=f"错误:\n{message['message']}")
        except queue.Empty:
            pass
        finally:
            self.root.after(50, self._process_ui_queue)

    def _on_drag_start(self, event):
        self._drag_data["x"] = event.x
        self._drag_data["y"] = event.y

    def _on_drag_stop(self, event):
        self._drag_data["x"] = 0
        self._drag_data["y"] = 0

    def _on_drag_motion(self, event):
        dx = event.x - self._drag_data["x"]
        dy = event.y - self._drag_data["y"]
        x = self.root.winfo_x() + dx
        y = self.root.winfo_y() + dy
        self.root.geometry(f"+{x}+{y}")

    def run(self):
        self.root = tk.Tk()
        self.root.overrideredirect(True)
        self.root.wm_attributes("-topmost", True)
        self.root.wm_attributes("-alpha", 0.75)
        self.root.withdraw()
        self._load_icons()
        self._create_widgets()
        self._process_ui_queue()
        self.root.mainloop()