import os
import queue
import sys
import tkinter as tk
import ttkbootstrap as ttk
from ttkbootstrap.dialogs import Querybox, Messagebox
from tkinter import font as tkFont
import webbrowser
import threading
from pystray import MenuItem as item, Menu, Icon
from PIL import Image, ImageTk
from typing import Optional, Callable

from utils import find_cost_bar_roi, resource_path
from calibration_manager import get_calibration_profiles, get_calibration_basename


class OverlayWindow:
    def __init__(self, master_callback: Callable, ui_queue: queue.Queue, parent_root: ttk.Window,
                 scaling_factor: float = 1.0):
        self.parent_root = parent_root
        self.root: Optional[ttk.Toplevel] = None
        self.master_callback = master_callback
        self.ui_queue = ui_queue

        self.scaling_factor = scaling_factor

        # 定义100%缩放下的基准尺寸
        self.FONT_PIXEL_LARGE = -60
        self.FONT_PIXEL_MEDIUM = -20
        self.FONT_PIXEL_SMALL = -15
        self.OFFSET_PIXEL_X = -40
        self.PADDING_PIXEL = 4

        self.icons = {}
        self._drag_data = {"x": 0, "y": 0}
        self.tray_icon: Optional[Icon] = None
        self.active_profile_filename: Optional[str] = None

    def run(self):
        self.root = ttk.Toplevel(self.parent_root)
        self.root.overrideredirect(True)
        self.root.wm_attributes("-topmost", True)
        self.root.wm_attributes("-alpha", 0.75)
        self.root.config(bg='white')
        self.root.withdraw()
        self._load_icons()
        self._create_widgets()
        self._setup_tray_icon()
        self._process_ui_queue()
        self.parent_root.mainloop()

    def _create_widgets(self):
        overlay_bg = '#3a3a3a'
        style = ttk.Style()
        style.configure('Overlay.TFrame', background=overlay_bg)
        style.configure('Overlay.TLabel', background=overlay_bg, foreground='white')
        style.configure('Overlay.Total.TLabel', background=overlay_bg, foreground='gray60')
        style.configure('Overlay.Timer.TLabel', background=overlay_bg, foreground='gray60')
        style.configure('Overlay.TButton', background=overlay_bg, borderwidth=0, highlightthickness=0, padding=0)
        style.map('Overlay.TButton', background=[('active', 'gray40')])

        container = ttk.Frame(self.root, style='Overlay.TFrame')
        container.pack(expand=True, fill='both')

        self.left_frame = ttk.Frame(container, style='Overlay.TFrame')
        self.left_frame.place(relx=0, rely=0, relwidth=0.33, relheight=1.0)
        self.icon_button = ttk.Button(self.left_frame, style='Overlay.TButton')
        self.icon_button.pack(expand=True, fill="both")

        self.right_frame = ttk.Frame(container, style='Overlay.TFrame')
        self.right_frame.place(relx=0.33, rely=0, relwidth=0.67, relheight=1.0)
        self.right_frame.bind("<ButtonPress-1>", self._on_drag_start)
        self.right_frame.bind("<ButtonRelease-1>", self._on_drag_stop)
        self.right_frame.bind("<B1-Motion>", self._on_drag_motion)

        self.pre_cal_label = ttk.Label(self.right_frame, text="", style='Overlay.TLabel',
                                       font=("Segoe UI", self.FONT_PIXEL_MEDIUM), justify='center')
        self.cal_progress_label = ttk.Label(self.right_frame, text="0%", style='Overlay.TLabel',
                                            font=("Segoe UI", self.FONT_PIXEL_LARGE))
        self.running_frame_label = ttk.Label(self.right_frame, text="--", style='Overlay.TLabel',
                                             font=("Segoe UI", self.FONT_PIXEL_LARGE, "bold"))
        self.running_total_label = ttk.Label(container, text="/--", style='Overlay.Total.TLabel',
                                             font=("Segoe UI", self.FONT_PIXEL_MEDIUM))

        self.timer_container = ttk.Frame(container, style='Overlay.TFrame')
        self.timer_icon_label = ttk.Label(self.timer_container, style='Overlay.TLabel')
        self.timer_icon_label.pack(side=tk.LEFT)
        self.timer_label = ttk.Label(self.timer_container, text="00:00:00", style='Overlay.Timer.TLabel',
                                     font=("Segoe UI", self.FONT_PIXEL_SMALL), cursor="hand2")
        self.timer_label.pack(side=tk.LEFT)
        self.timer_label.bind("<Button-1>", self._on_timer_click)

        self.lap_container = ttk.Frame(container, style='Overlay.TFrame')
        self.lap_icon_label = ttk.Label(self.lap_container, style='Overlay.TLabel')
        self.lap_icon_label.pack(side=tk.LEFT)
        self.lap_frame_label = ttk.Label(self.lap_container, text="0", style='Overlay.Timer.TLabel',
                                         font=("Segoe UI", self.FONT_PIXEL_SMALL))
        self.lap_frame_label.pack(side=tk.LEFT)

    def _on_timer_click(self, event=None):
        self.master_callback({"type": "toggle_lap_timer"})

    def _hide_all_dynamic_labels(self):
        """一个辅助函数，用于隐藏所有根据状态动态显示的标签。"""
        self.pre_cal_label.place_forget()
        self.cal_progress_label.place_forget()
        self.running_frame_label.place_forget()
        self.running_total_label.place_forget()
        self.timer_container.place_forget()
        self.lap_container.place_forget()


    def set_state_running(self, total_frames: int, active_profile: str):
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('deco'), command=None)

        self.running_frame_label.place(relx=1.0, rely=0.4, anchor='e', x=self.OFFSET_PIXEL_X)
        self.running_total_label.config(text=f"/{total_frames - 1}")
        self.running_total_label.place(relx=1.0, rely=1.0, anchor='se', x=-self.PADDING_PIXEL, y=-self.PADDING_PIXEL)
        self.timer_container.place(relx=0.0, rely=1.0, anchor='sw', x=self.PADDING_PIXEL, y=-self.PADDING_PIXEL)

        self.active_profile_filename = active_profile
        self._update_tray_menu()

    def update_lap_timer(self, lap_frames: Optional[int]):
        if lap_frames is not None:
            self.lap_frame_label.config(text=f"{lap_frames}")
            self.lap_container.place(relx=0.0, rely=0.0, anchor='nw', x=self.PADDING_PIXEL, y=self.PADDING_PIXEL)
        else:
            self.lap_container.place_forget()

    def update_running_display(self, current_frame: Optional[int]):
        if current_frame is not None:
            self.running_frame_label.config(text=f"{current_frame}")
        else:
            self.running_frame_label.config(text="--")

    def update_timer(self, time_str: str):
        self.timer_label.config(text=time_str)

    def _process_ui_queue(self):
        try:
            message = self.ui_queue.get_nowait()
            msg_type = message.get("type")
            if msg_type == "update":
                self.update_running_display(message["frame"])
                if "time_str" in message: self.update_timer(message["time_str"])
                if "lap_frames" in message: self.update_lap_timer(message["lap_frames"])
            elif msg_type == "geometry":
                self.setup_geometry(message["width"], message["height"])
            elif msg_type == "state_change":
                state = message["state"]
                if state == "running":
                    self.set_state_running(message["total_frames"], message["active_profile"])
                elif state == "idle":
                    self.set_state_idle()
                elif state == "pre_calibration":
                    self.set_state_pre_calibration()
                elif state == "calibrating":
                    self.set_state_calibrating()
            elif msg_type == "calibration_progress":
                self.update_calibration_progress(message["progress"])
            elif msg_type == "profiles_changed":
                self._update_tray_menu()
            elif msg_type == "error":
                self._hide_all_dynamic_labels()
                self.pre_cal_label.config(text=f"错误:\n{message['message'][:50]}...")
                self.pre_cal_label.place(relx=0.5, rely=0.5, anchor="center")
        except queue.Empty:
            pass
        finally:
            if self.root and self.root.winfo_exists(): self.root.after(50, self._process_ui_queue)

    def _load_icons(self):
        try:
            icon_names = ["start", "wait", "deco", "timer"]
            for name in icon_names:
                path = resource_path(os.path.join("icons", f"{name}.png"))
                img = Image.open(path).convert("RGBA")
                self.icons[name] = ImageTk.PhotoImage(image=img)
        except FileNotFoundError as e:
            print(f"错误: 缺少图标文件 {e.filename}。请先运行 setup_assets.py")
            sys.exit(1)

    def _resize_icons(self, size: int):
        try:
            timer_font = tkFont.Font(font=("Segoe UI", self.FONT_PIXEL_SMALL))
            timer_height = timer_font.metrics('linespace')
            for name in ["start", "deco"]:
                path = resource_path(os.path.join("icons", f"{name}.png"))
                img = Image.open(path).resize((size, size), Image.Resampling.LANCZOS)
                self.icons[name] = ImageTk.PhotoImage(image=img)
            wait_path = resource_path(os.path.join("icons", "wait.png"))
            wait_img_large = Image.open(wait_path).resize((size, size), Image.Resampling.LANCZOS)
            self.icons["wait"] = ImageTk.PhotoImage(image=wait_img_large)
            timer_icon_path = resource_path(os.path.join("icons", "timer.png"))
            timer_img = Image.open(timer_icon_path).resize((timer_height, timer_height), Image.Resampling.LANCZOS)
            self.icons["timer_sized"] = ImageTk.PhotoImage(image=timer_img)
            self.timer_icon_label.config(image=self.icons["timer_sized"])
            lap_icon_path = resource_path(os.path.join("icons", "wait.png"))
            lap_img = Image.open(lap_icon_path).resize((timer_height, timer_height), Image.Resampling.LANCZOS)
            self.icons["lap_sized"] = ImageTk.PhotoImage(image=lap_img)
            self.lap_icon_label.config(image=self.icons["lap_sized"])
        except Exception as e:
            print(f"调整图标大小时出错: {e}")

    def setup_geometry(self, screen_width, screen_height):
        roi_x1, roi_x2, _ = find_cost_bar_roi(screen_width, screen_height)
        cost_bar_pixel_length = roi_x2 - roi_x1
        win_width = int(cost_bar_pixel_length * 5 / 6)
        win_height = int(win_width * 27 / 50)
        actual_screen_width = self.root.winfo_screenwidth()
        actual_screen_height = self.root.winfo_screenheight()
        pos_x = actual_screen_width - win_width - 50
        pos_y = actual_screen_height - win_height - 100
        self.root.geometry(f"{win_width}x{win_height}+{pos_x}+{pos_y}")
        button_width = int(win_width * 0.33)
        button_height = win_height
        icon_size = min(button_width, button_height)
        self._resize_icons(icon_size)
        self.root.deiconify()

    def _open_about_page(self, *args):
        webbrowser.open("https://github.com/ZeroAd-06/ArknightsCostBarRuler")

    def _quit_application(self, *args):
        if self.tray_icon: self.tray_icon.stop()
        if self.root:
            self.root.quit()
            self.root.destroy()
            self.parent_root.destroy()

    def _create_tray_menu(self) -> Menu:
        profiles = get_calibration_profiles()
        calib_menu_items = [item('-- 新建 --', lambda *args: self.master_callback({"type": "prepare_calibration"}))]
        if profiles: calib_menu_items.append(Menu.SEPARATOR)
        for p in profiles:
            is_active = p["filename"] == self.active_profile_filename
            display_name = f"{'● ' if is_active else ''}{p['basename']} ({p['total_frames']}f)"
            profile_submenu = Menu(
                item('选用',
                     lambda *args, f=p["filename"]: self.master_callback({"type": "use_profile", "filename": f}),
                     enabled=not is_active),
                item('重命名', lambda *args, f=p["filename"]: self._rename_profile(f)),
                item('删除', lambda *args, f=p["filename"]: self._delete_profile(f))
            )
            calib_menu_items.append(item(display_name, profile_submenu))
        return Menu(
            item('校准配置', Menu(*calib_menu_items)), Menu.SEPARATOR,
            item('关于', self._open_about_page), item('退出', self._quit_application)
        )

    def _update_tray_menu(self):
        if self.tray_icon:
            self.tray_icon.menu = self._create_tray_menu()
            self.tray_icon.update_menu()

    def _rename_profile(self, filename: str):
        self.root.after(0, self._show_rename_dialog, filename)

    def _show_rename_dialog(self, filename: str):
        old_basename = get_calibration_basename(filename)
        new_basename = Querybox.get_string(prompt=f"为 '{old_basename}' 输入新名称:", title="重命名",
                                           initialvalue=old_basename, parent=self.root)
        if new_basename and new_basename.strip():
            self.master_callback({"type": "rename_profile", "old": filename, "new_base": new_basename.strip()})
        elif new_basename is not None:
            Messagebox.show_warning("名称不能为空。", title="无效名称", parent=self.root)

    def _delete_profile(self, filename: str):
        self.root.after(0, self._show_delete_dialog, filename)

    def _show_delete_dialog(self, filename: str):
        basename = get_calibration_basename(filename)
        result = Messagebox.yesno(message=f"确实要删除校准配置 '{basename}' 吗？", title="确认删除", parent=self.root)
        if result == "Yes": self.master_callback({"type": "delete_profile", "filename": filename})

    def _setup_tray_icon(self):
        try:
            icon_path = resource_path(os.path.join("icons", "deco.png"))
            icon_image = Image.open(icon_path)
            self.tray_icon = Icon("ArknightsCostBarRuler", icon_image, "明日方舟费用条尺子",
                                  menu=self._create_tray_menu())
            tray_thread = threading.Thread(target=self.tray_icon.run, daemon=True)
            tray_thread.start()
            print("托盘图标已启动。")
        except Exception as e:
            print(f"创建托盘图标失败: {e}")

    def set_state_idle(self):
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('deco'), command=None)
        self.pre_cal_label.config(text="右键托盘\n选择一个配置")
        self.pre_cal_label.place(relx=0.5, rely=0.5, anchor="center")
        self.active_profile_filename = None
        self._update_tray_menu()

    def set_state_pre_calibration(self):
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('start'),
                                command=lambda: self.master_callback({"type": "start_calibration"}))
        self.pre_cal_label.config(text="选中干员后\n点击左侧校准")
        self.pre_cal_label.place(relx=0.5, rely=0.5, anchor="center")
        self.active_profile_filename = None
        self._update_tray_menu()

    def set_state_calibrating(self):
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('wait'), command=None)
        self.cal_progress_label.place(relx=0.5, rely=0.5, anchor="center")

    def update_calibration_progress(self, percentage: float):
        self.cal_progress_label.config(text=f"{int(percentage)}%")

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