import os
import queue
import tkinter as tk
from typing import Optional

from PIL import Image, ImageTk

from utils import find_cost_bar_roi


class OverlayWindow:
    """
    一个悬浮窗，用于显示费用条信息。
    通过队列与工作线程安全通信。
    """

    def __init__(self, master_callback, ui_queue: queue.Queue):
        self.root = None
        self.master_callback = master_callback  # 用于向工作线程发送命令
        self.ui_queue = ui_queue  # 用于从工作线程接收更新
        self.icons = {}
        self._drag_data = {"x": 0, "y": 0}

    def _load_icons(self):
        """加载所有需要的图标资源"""
        try:
            icon_names = ["start", "wait", "deco"]
            for name in icon_names:
                path = os.path.join("icons", f"{name}.png")
                img = Image.open(path).convert("RGBA")
                self.icons[name] = ImageTk.PhotoImage(image=img)
        except FileNotFoundError as e:
            print(f"错误: 缺少图标文件 {e.filename}。请先运行 setup_assets.py 生成图标。")
            img = Image.new("RGBA", (64, 64), (255, 0, 0, 128))
            self.icons["error"] = ImageTk.PhotoImage(image=img)

    def _create_widgets(self):
        """创建所有UI组件"""
        self.root.config(bg='#3a3a3a')

        # --- 左侧 1/3: 图标按钮 ---
        self.left_frame = tk.Frame(self.root, bg=self.root.cget('bg'))
        self.left_frame.place(relx=0, rely=0, relwidth=0.33, relheight=1.0)

        self.icon_button = tk.Button(self.left_frame, borderwidth=0, highlightthickness=0,
                                     bg=self.root.cget('bg'), activebackground='gray30')
        self.icon_button.pack(expand=True, fill="both")

        # --- 右侧 2/3: 信息显示与拖动区 ---
        self.right_frame = tk.Frame(self.root, bg=self.root.cget('bg'))
        self.right_frame.place(relx=0.33, rely=0, relwidth=0.67, relheight=1.0)

        self.right_frame.bind("<ButtonPress-1>", self._on_drag_start)
        self.right_frame.bind("<ButtonRelease-1>", self._on_drag_stop)
        self.right_frame.bind("<B1-Motion>", self._on_drag_motion)

        # --- 预创建所有状态下可能用到的Label ---

        # 状态1:
        self.pre_cal_label = tk.Label(self.right_frame, text="选中干员\n点击左侧按钮", fg="white",
                                      bg=self.root.cget('bg'), font=("Segoe UI", 11))

        # 状态2:
        self.cal_progress_label = tk.Label(self.right_frame, text="0%", fg="white", bg=self.root.cget('bg'),
                                           font=("Segoe UI", 34))

        # 状态3:
        self.running_frame_label = tk.Label(self.right_frame, text="--", fg="white", bg=self.root.cget('bg'),
                                            font=("Segoe UI", 34))
        self.running_total_label = tk.Label(self.root, text="/--", fg="gray60", bg=self.root.cget('bg'),
                                            font=("Segoe UI", 12))

    def setup_geometry(self, screen_width, screen_height):
        """根据屏幕分辨率计算并设置窗口大小和位置"""
        roi_x1, roi_x2, _ = find_cost_bar_roi(screen_width, screen_height)
        cost_bar_pixel_length = roi_x2 - roi_x1

        # 计算窗口尺寸
        win_width = int(cost_bar_pixel_length * 5 / 6)
        win_height = int(win_width * 27 / 50)

        pos_x = screen_width - win_width - 50
        pos_y = screen_height - win_height - 100

        self.root.geometry(f"{win_width}x{win_height}+{pos_x}+{pos_y}")
        self._resize_icons(win_height)
        self.root.deiconify()  # 显示窗口

    def _resize_icons(self, height):
        """根据窗口高度调整图标大小"""
        size = int(height * 0.8)
        try:
            for name in ["start", "wait", "deco"]:
                path = os.path.join("icons", f"{name}.png")
                img = Image.open(path).resize((size, size), Image.Resampling.LANCZOS)
                self.icons[name] = ImageTk.PhotoImage(image=img)
        except Exception as e:
            print(f"调整图标大小时出错: {e}")

    def _hide_all_dynamic_labels(self):
        """隐藏所有动态标签"""
        self.pre_cal_label.place_forget()
        self.cal_progress_label.place_forget()
        self.running_frame_label.place_forget()
        self.running_total_label.place_forget()

    def set_state_pre_calibration(self):
        """设置为“未校准”状态"""
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('start'),
                                command=lambda: self.master_callback("start_calibration"))
        self.pre_cal_label.place(relx=0.5, rely=0.5, anchor="center")

    def set_state_calibrating(self):
        """设置为“校准中”状态"""
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('wait'), command=None)
        self.cal_progress_label.place(relx=0.5, rely=0.5, anchor="center")

    def update_calibration_progress(self, percentage: float):
        """更新校准进度显示"""
        self.cal_progress_label.config(text=f"{int(percentage)}%")

    def set_state_running(self, total_frames: int):
        """设置为“运行中”状态"""
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('deco'),
                                command=lambda: self.master_callback("delete_calibration"))

        self.running_total_label.config(text=f"/{total_frames - 1}")

        self.running_frame_label.place(relx=1.0, rely=0.5, anchor='e', x=-40)
        self.running_total_label.place(relx=1.0, rely=1.0, anchor='se', x=-5, y=-5)

    def update_running_display(self, current_frame: Optional[int]):
        """更新当前逻辑帧的显示"""
        if current_frame is not None:
            self.running_frame_label.config(text=f"{current_frame}")
        else:
            self.running_frame_label.config(text="--")

    def _process_ui_queue(self):
        """
        轮询UI队列，并根据从工作线程收到的消息更新UI。
        这是线程安全的核心。
        """
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
                # 简单地在标签上显示错误，或者可以创建一个错误对话框
                self.pre_cal_label.config(text=f"错误:\n{message['message']}")

        except queue.Empty:
            pass  # 队列为空是正常情况
        finally:
            # 安排下一次轮询
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
        """启动Tkinter主循环"""
        self.root = tk.Tk()
        self.root.overrideredirect(True)
        self.root.wm_attributes("-topmost", True)
        self.root.wm_attributes("-alpha", 0.75)
        self.root.withdraw()  # 初始隐藏，等待工作线程提供尺寸信息

        self._load_icons()
        self._create_widgets()

        # 启动UI队列轮询
        self._process_ui_queue()

        self.root.mainloop()