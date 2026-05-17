import logging
import os
import queue
import sys
import tkinter as tk
from tkinter import Menu as tkMenu
import ttkbootstrap as ttk
from ttkbootstrap.dialogs import Querybox, Messagebox
from tkinter import font as tkFont
import webbrowser
from PIL import Image, ImageTk
from typing import Optional, Callable

from utils import find_cost_bar_roi, resource_path
from calibration_manager import get_calibration_profiles, get_calibration_basename
from i18n import i18n

logger = logging.getLogger(__name__)

VERSION = "v1.2.1"
FRAMES_PER_SECOND = 30
TRAY_SUPPORTED = False
try:
    from pystray import MenuItem as item, Menu, Icon

    TRAY_SUPPORTED = True
    logger.info("pystray 模块已加载，系统托盘功能已启用。")
except (ImportError, ModuleNotFoundError):
    logger.warning("pystray 模块未安装或不受支持，系统托盘功能将被禁用。将使用右键菜单作为替代。")

    class Icon:
        pass
    class Menu:
        pass
    class item:
        pass




class OverlayWindow:
    def __init__(self, master_callback: Callable, ui_queue: queue.Queue, parent_root: ttk.Window):
        logger.info(i18n.get("overlay.init", "Initializing OverlayWindow..."))
        self.parent_root = parent_root
        self.root: Optional[ttk.Toplevel] = None
        self.master_callback = master_callback
        self.ui_queue = ui_queue

        self.current_display_mode = '0_to_n-1'
        self.current_cycle_total_frames = 0

        self.screen_width = self.parent_root.winfo_screenwidth()
        self.screen_height = self.parent_root.winfo_screenheight()
        logger.info(i18n.get("overlay.screen_res", "Detected screen resolution: {width}x{height}", width=self.screen_width, height=self.screen_height))

        self.fonts = {}
        self.sizes = {}
        self.icons = {}
        self._drag_data = {"x": 0, "y": 0}
        self.tray_icon: Optional[Icon] = None
        self.active_profile_filename: Optional[str] = None

    def run(self):
        logger.info("OverlayWindow.run() - 开始创建和运行窗口。")
        self.root = ttk.Toplevel(self.parent_root)
        self.root.overrideredirect(True)
        self.root.wm_attributes("-topmost", True)
        self.root.wm_attributes("-alpha", 0.75)
        self.root.config(bg='white')
        self.root.withdraw()

        self._load_icons()
        self._create_widgets()

        if TRAY_SUPPORTED:
            self._setup_tray_icon()

        self._process_ui_queue()

        logger.info("进入Tkinter主循环 (mainloop)...")
        self.parent_root.mainloop()

    def _create_widgets(self):
        logger.debug("正在创建悬浮窗控件...")
        overlay_bg = '#3a3a3a'
        style = ttk.Style()
        style.configure('Overlay.TFrame', background=overlay_bg)
        style.configure('Overlay.TLabel', background=overlay_bg, foreground='white')
        style.configure('Overlay.Total.TLabel', background=overlay_bg, foreground='gray60')
        style.configure('Overlay.Timer.TLabel', background=overlay_bg, foreground='gray60')
        style.configure('Overlay.TButton', background=overlay_bg, borderwidth=0, highlightthickness=0, padding=0)
        style.map('Overlay.TButton', background=[('active', 'gray40')])

        self.container = ttk.Frame(self.root, style='Overlay.TFrame')
        self.container.pack(expand=True, fill='both')

        self.left_frame = ttk.Frame(self.container, style='Overlay.TFrame')
        self.left_frame.place(relx=0, rely=0, relwidth=0.33, relheight=1.0)

        self.icon_button = ttk.Button(self.left_frame, style='Overlay.TButton')
        self.icon_button.pack(expand=True, fill="both")

        self.right_frame = ttk.Frame(self.container, style='Overlay.TFrame')
        self.right_frame.place(relx=0.33, rely=0, relwidth=0.67, relheight=1.0)

        for widget in [self.container, self.left_frame, self.right_frame, self.icon_button]:
            widget.bind("<ButtonPress-1>", self._on_drag_start)
            widget.bind("<ButtonRelease-1>", self._on_drag_stop)
            widget.bind("<B1-Motion>", self._on_drag_motion)
            widget.bind("<Button-3>", self._show_context_menu)

        self.pre_cal_label = ttk.Label(self.right_frame, text="", style='Overlay.TLabel', justify='center')
        self.cal_progress_label = ttk.Label(self.right_frame, text="0%", style='Overlay.TLabel')
        self.running_frame_label = ttk.Label(self.right_frame, text="--", style='Overlay.TLabel')
        self.running_total_label = ttk.Label(self.container, text="/--", style='Overlay.Total.TLabel')

        self.timer_container = ttk.Frame(self.container, style='Overlay.TFrame')
        self.timer_icon_label = ttk.Label(self.timer_container, style='Overlay.TLabel')
        self.timer_icon_label.pack(side=tk.LEFT)
        self.timer_label = ttk.Label(self.timer_container, text="00:00:00", style='Overlay.Timer.TLabel',
                                     cursor="hand2")
        self.timer_label.pack(side=tk.LEFT)
        self.timer_label.bind("<Button-1>", self._on_timer_click)
        self.timer_container.bind("<Button-3>", self._show_context_menu)
        self.timer_label.bind("<Button-3>", self._show_context_menu)
        self.timer_icon_label.bind("<Button-3>", self._show_context_menu)

        self.lap_container = ttk.Frame(self.container, style='Overlay.TFrame')
        self.lap_icon_label = ttk.Label(self.lap_container, style='Overlay.TLabel')
        self.lap_icon_label.pack(side=tk.LEFT)
        self.lap_frame_label = ttk.Label(self.lap_container, text="0", style='Overlay.Timer.TLabel')
        self.lap_frame_label.pack(side=tk.LEFT)

        logger.debug("悬浮窗控件创建完成。")

    def _show_context_menu(self, event):
        """创建并显示Tkinter上下文菜单。"""
        logger.debug("显示右键上下文菜单...")
        context_menu = tkMenu(self.root, tearoff=0)

        context_menu.add_cascade(label=i18n.get("overlay.menu.calibration"), menu=self._create_tkinter_profile_submenu(context_menu))
        context_menu.add_cascade(label=i18n.get("overlay.menu.display"), menu=self._create_tkinter_display_mode_submenu(context_menu))
        context_menu.add_cascade(label=i18n.get("overlay.menu.timer"), menu=self._create_tkinter_timer_adjust_submenu(context_menu))

        context_menu.add_separator()
        context_menu.add_separator()
        context_menu.add_command(label=i18n.get("overlay.menu.about", version=VERSION), command=self._open_about_page)
        context_menu.add_command(label=i18n.get("overlay.menu.exit"), command=self._schedule_quit)

        try:
            context_menu.tk_popup(event.x_root, event.y_root)
        finally:
            context_menu.grab_release()

    def _create_tkinter_profile_submenu(self, parent_menu):
        profile_submenu = tkMenu(parent_menu, tearoff=0)
        profiles = get_calibration_profiles()
        profile_submenu = tkMenu(parent_menu, tearoff=0)
        profiles = get_calibration_profiles()
        profile_submenu.add_command(label=i18n.get("overlay.menu.new_profile"),
                                    command=lambda: self.master_callback({"type": "prepare_calibration"}))
        if profiles:
            profile_submenu.add_separator()
        for p in profiles:
            is_active = p["filename"] == self.active_profile_filename
            display_name = f"{p['basename']} ({p['total_frames_str']})"

            actions_submenu = tkMenu(profile_submenu, tearoff=0)
            actions_submenu.add_command(label=i18n.get("overlay.menu.select"), command=lambda f=p["filename"]: self.master_callback(
                {"type": "use_profile", "filename": f}), state="disabled" if is_active else "normal")
            actions_submenu.add_command(label=i18n.get("overlay.menu.rename"), command=lambda f=p["filename"]: self._rename_profile(f))
            actions_submenu.add_command(label=i18n.get("overlay.menu.delete"), command=lambda f=p["filename"]: self._delete_profile(f))

            profile_submenu.add_cascade(label=display_name, menu=actions_submenu,
                                        foreground="blue" if is_active else "black")
        return profile_submenu

    def _create_tkinter_display_mode_submenu(self, parent_menu):
        display_mode_submenu = tkMenu(parent_menu, tearoff=0)
        modes = {"0_to_n-1": "0 / n-1", "0_to_n": "0 / n", "1_to_n": "1 / n"}
        tk_display_mode = tk.StringVar(value=self.current_display_mode)
        for key, text in modes.items():
            display_mode_submenu.add_radiobutton(label=text, variable=tk_display_mode, value=key,
                                                 command=lambda m=key: self.master_callback(
                                                     {"type": "set_display_mode", "mode": m}))
        return display_mode_submenu

    def _create_tkinter_timer_adjust_submenu(self, parent_menu):
        timer_adjust_submenu = tkMenu(parent_menu, tearoff=0)
        is_running = self.active_profile_filename is not None
        menu_state = "normal" if is_running else "disabled"
        cycle_frames = self.current_cycle_total_frames

        def adjust_cb(frames): self.master_callback({"type": "adjust_timer", "frames": frames})

        def reset_cb(): self.master_callback({"type": "reset_timer"})

        timer_adjust_submenu.add_command(label=i18n.get("overlay.timer.back_frames", frames=cycle_frames), state=menu_state,
                                         command=lambda: adjust_cb(-cycle_frames))
        timer_adjust_submenu.add_command(label=i18n.get("overlay.timer.back_1s"), state=menu_state,
                                         command=lambda: adjust_cb(-FRAMES_PER_SECOND))
        timer_adjust_submenu.add_separator()
        timer_adjust_submenu.add_command(label=i18n.get("overlay.timer.reset"), state=menu_state, command=reset_cb)
        timer_adjust_submenu.add_separator()
        timer_adjust_submenu.add_command(label=i18n.get("overlay.timer.fwd_1s"), state=menu_state,
                                         command=lambda: adjust_cb(FRAMES_PER_SECOND))
        timer_adjust_submenu.add_command(label=i18n.get("overlay.timer.fwd_frames", frames=cycle_frames), state=menu_state,
                                         command=lambda: adjust_cb(cycle_frames))

        return timer_adjust_submenu

    def _on_timer_click(self, event=None):
        logger.info(i18n.get("overlay.timer.click_log", "Timer label clicked, sending toggle_lap_timer command."))
        self.master_callback({"type": "toggle_lap_timer"})

    def _hide_all_dynamic_labels(self):
        logger.debug("隐藏所有动态标签以切换状态。")
        self.pre_cal_label.place_forget()
        self.cal_progress_label.place_forget()
        self.running_frame_label.place_forget()
        self.running_total_label.place_forget()
        self.timer_container.place_forget()
        self.lap_container.place_forget()

    def setup_geometry(self, emulator_width: int, emulator_height: int):
        logger.info(f"根据模拟器分辨率 {emulator_width}x{emulator_height} 设置悬浮窗几何尺寸。")
        roi_x1, roi_x2, _ = find_cost_bar_roi(self.screen_width, self.screen_height)
        cost_bar_pixel_length = roi_x2 - roi_x1
        logger.debug(f"计算出的屏幕费用条像素长度: {cost_bar_pixel_length}")

        win_width = int(cost_bar_pixel_length * 5 / 6)
        win_height = int(win_width * 27 / 50)
        logger.debug(f"悬浮窗尺寸设置为: {win_width}x{win_height}")

        # Adjust font size for English to prevent truncation
        medium_scale = 0.22
        if i18n.current_locale == "en_US":
            medium_scale = 0.18

        self.fonts['large_bold'] = tkFont.Font(family="Segoe UI", size=-int(win_height * 0.55), weight="bold")
        self.fonts['large_normal'] = tkFont.Font(family="Segoe UI", size=-int(win_height * 0.55))
        self.fonts['medium'] = tkFont.Font(family="Segoe UI", size=-int(win_height * medium_scale))
        self.fonts['small'] = tkFont.Font(family="Segoe UI", size=-int(win_height * 0.18))

        self.sizes['offset_x'] = -int(win_width * 0.2)
        self.sizes['padding'] = int(win_height * 0.01)

        self.pre_cal_label.config(font=self.fonts['medium'])
        self.cal_progress_label.config(font=self.fonts['large_normal'])
        self.running_frame_label.config(font=self.fonts['large_bold'])
        self.running_total_label.config(font=self.fonts['medium'])
        self.timer_label.config(font=self.fonts['small'])
        self.lap_frame_label.config(font=self.fonts['small'])

        pos_x = self.screen_width - win_width - 50
        pos_y = self.screen_height - win_height - 100
        self.root.geometry(f"{win_width}x{win_height}+{pos_x}+{pos_y}")
        logger.debug(f"悬浮窗初始位置: {pos_x}, {pos_y}")

        button_width = int(win_width * 0.33)
        icon_size = min(button_width, win_height)
        self._resize_icons(icon_size)

        self.root.deiconify()
        logger.info("悬浮窗几何尺寸设置完成并已显示。")

    def set_state_running(self, display_total: str, active_profile: str, display_mode: str):
        logger.info(f"UI状态切换: running (profile='{active_profile}', mode='{display_mode}')")
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('deco'), command=None)

        self.current_display_mode = display_mode

        padding = self.sizes.get('padding', 4)
        offset_x = self.sizes.get('offset_x', -40)

        self.running_frame_label.place(relx=1.0, rely=0.4, anchor='e', x=offset_x)
        self.running_total_label.config(text=display_total)
        self.running_total_label.place(relx=1.0, rely=1.0, anchor='se', x=-padding, y=-padding)
        self.timer_container.place(relx=0.0, rely=1.0, anchor='sw', x=padding, y=-padding)

        self.active_profile_filename = active_profile

        if TRAY_SUPPORTED:
            self._update_tray_menu()

    def update_lap_timer(self, lap_frames: Optional[int]):
        padding = self.sizes.get('padding', 4)
        if lap_frames is not None:
            if not self.lap_container.winfo_ismapped():
                logger.debug("显示计圈器。")
            self.lap_frame_label.config(text=f"{lap_frames}")
            self.lap_container.place(relx=0.0, rely=0.0, anchor='nw', x=padding, y=padding)
        else:
            if self.lap_container.winfo_ismapped():
                logger.debug("隐藏计圈器。")
            self.lap_container.place_forget()

    def _resize_icons(self, size: int):
        logger.debug(f"正在将所有图标调整为尺寸: {size}x{size}")
        try:
            timer_height = self.fonts['small'].metrics('linespace')
            logger.debug(f"计时器图标高度: {timer_height}")

            for name in ["start", "deco"]:
                path = resource_path(os.path.join("./icons", f"{name}.png"))
                img = Image.open(path).resize((size, size), Image.Resampling.LANCZOS)
                self.icons[name] = ImageTk.PhotoImage(image=img)

            wait_path = resource_path(os.path.join("./icons", "wait.png"))
            wait_img_large = Image.open(wait_path).resize((size, size), Image.Resampling.LANCZOS)
            self.icons["wait"] = ImageTk.PhotoImage(image=wait_img_large)

            timer_icon_path = resource_path(os.path.join("./icons", "timer.png"))
            timer_img = Image.open(timer_icon_path).resize((timer_height, timer_height), Image.Resampling.LANCZOS)
            self.icons["timer_sized"] = ImageTk.PhotoImage(image=timer_img)
            self.timer_icon_label.config(image=self.icons["timer_sized"])

            lap_icon_path = resource_path(os.path.join("./icons", "wait.png"))
            lap_img = Image.open(lap_icon_path).resize((timer_height, timer_height), Image.Resampling.LANCZOS)
            self.icons["lap_sized"] = ImageTk.PhotoImage(image=lap_img)
            self.lap_icon_label.config(image=self.icons["lap_sized"])
        except Exception as e:
            logger.exception(f"调整图标大小时出错: {e}")

    def update_running_display(self, display_frame: str, display_total: str):
        self.running_frame_label.config(text=f"{display_frame}")
        self.running_total_label.config(text=display_total)

    def update_timer(self, time_str: str):
        self.timer_label.config(text=time_str)

    def _process_ui_queue(self):
        try:
            message = self.ui_queue.get_nowait()
            msg_type = message.get("type")
            if msg_type != "update":
                logger.debug(f"从UI队列收到消息: {message}")

            if msg_type == "update":
                self.update_running_display(message["display_frame"], message["display_total"])
                if "time_str" in message: self.update_timer(message["time_str"])
                if "lap_frames" in message: self.update_lap_timer(message["lap_frames"])
                self.current_cycle_total_frames = message.get("totalFramesInCycle", 0)
            elif msg_type == "geometry":
                self.setup_geometry(message["width"], message["height"])
            elif msg_type == "state_change":
                self.current_display_mode = message.get('display_mode', '0_to_n-1')
                state = message["state"]
                if state == "running":
                    self.set_state_running(message["display_total"], message["active_profile"],
                                           self.current_display_mode)
                elif state == "idle":
                    self.set_state_idle()
                elif state == "pre_calibration":
                    self.set_state_pre_calibration()
                elif state == "calibrating":
                    self.set_state_calibrating()
            elif msg_type == "calibration_progress":
                self.update_calibration_progress(message["progress"])
            elif msg_type == "profiles_changed":
                logger.info("收到配置文件变更通知，正在更新托盘菜单...")
                if TRAY_SUPPORTED: self._update_tray_menu()
            elif msg_type == "mode_changed":
                logger.info("收到显示模式变更通知，正在更新托盘菜单...")
                self.current_display_mode = message["mode"]
                if TRAY_SUPPORTED: self._update_tray_menu()
            elif msg_type == "error":
                logger.error(f"UI收到错误消息: {message['message']}")
                self._hide_all_dynamic_labels()
                self.pre_cal_label.config(text=f"错误:\n{message['message'][:50]}...")
                self.pre_cal_label.place(relx=0.5, rely=0.5, anchor="center")
        except queue.Empty:
            pass
        except Exception as e:
            logger.exception("处理UI队列消息时发生未预料的错误。")
        finally:
            if self.root and self.root.winfo_exists(): self.root.after(1, self._process_ui_queue)

    def _load_icons(self):
        logger.debug("正在加载所有图标资源...")
        try:
            icon_names = ["start", "wait", "deco", "timer"]
            for name in icon_names:
                path = resource_path(os.path.join("./icons", f"{name}.png"))
                img = Image.open(path).convert("RGBA")
                self.icons[name] = ImageTk.PhotoImage(image=img)
            logger.debug("图标资源加载完成。")
        except FileNotFoundError as e:
            logger.critical(f"错误: 缺少图标文件 {e.filename}。请确保资源文件完整。")
            sys.exit(1)

    def _open_about_page(self, *args):
        logger.info("用户点击 '关于'，打开项目主页。")
        webbrowser.open("https://github.com/ZeroAd-06/ArknightsCostBarRuler")

    def _schedule_quit(self):
        logger.info("接收到退出指令，将在主线程中执行关闭操作。")
        self.parent_root.after(0, self._quit_application)

    def _quit_application(self):
        logger.info("正在主线程中执行关闭操作...")
        if TRAY_SUPPORTED and self.tray_icon:
            logger.debug("停止托盘图标...")
            self.tray_icon.stop()
        logger.debug("销毁Tkinter根窗口...")
        self.parent_root.destroy()
        logger.info("程序已退出。")

    def _create_pystray_display_mode_submenu(self) -> Menu:
        modes = {"0_to_n-1": "0 / n-1", "0_to_n": "0 / n", "1_to_n": "1 / n"}

        def is_checked(mode_key): return self.current_display_mode == mode_key

        menu_items = [
            # 使用 *args 忽略所有传入的位置参数，并安全地使用关键字参数 m
            item(text, lambda *args, m=key: self.master_callback({"type": "set_display_mode", "mode": m}),
                 checked=lambda *args, m=key: is_checked(m), radio=True)
            for key, text in modes.items()
        ]
        return Menu(*menu_items)

    def _create_pystray_profile_submenu(self) -> Menu:
        profiles = get_calibration_profiles()
        calib_menu_items = [item(i18n.get("overlay.menu.new_profile"), lambda: self.master_callback({"type": "prepare_calibration"}))]
        if profiles: calib_menu_items.append(Menu.SEPARATOR)
        for p in profiles:
            is_active = p["filename"] == self.active_profile_filename
            display_name = f"{'● ' if is_active else ''}{p['basename']} ({p['total_frames_str']})"
            profile_actions = Menu(
                item(i18n.get("overlay.menu.select"),
                     lambda *args, f=p["filename"]: self.master_callback({"type": "use_profile", "filename": f}),
                     enabled=not is_active),
                item(i18n.get("overlay.menu.rename"), lambda *args, f=p["filename"]: self._rename_profile(f)),
                item(i18n.get("overlay.menu.delete"), lambda *args, f=p["filename"]: self._delete_profile(f))
            )
            calib_menu_items.append(item(display_name, profile_actions))
        return Menu(*calib_menu_items)

    def _create_pystray_timer_adjust_submenu(self):
        is_running = self.active_profile_filename is not None
        cycle_frames = self.current_cycle_total_frames

        def adjust_cb(frames): self.master_callback({"type": "adjust_timer", "frames": frames})

        def reset_cb(): self.master_callback({"type": "reset_timer"})

        return Menu(
            item(i18n.get("overlay.timer.back_frames", frames=cycle_frames), lambda: adjust_cb(-cycle_frames), enabled=is_running),
            item(i18n.get("overlay.timer.back_1s"), lambda: adjust_cb(-FRAMES_PER_SECOND), enabled=is_running),
            Menu.SEPARATOR,
            item(i18n.get("overlay.timer.reset"), reset_cb, enabled=is_running),
            Menu.SEPARATOR,
            item(i18n.get("overlay.timer.fwd_1s"), lambda: adjust_cb(FRAMES_PER_SECOND), enabled=is_running),
            item(i18n.get("overlay.timer.fwd_frames"), lambda: adjust_cb(cycle_frames), enabled=is_running)
        )

    def _update_tray_menu(self):
        if not TRAY_SUPPORTED or not self.tray_icon: return
        self.tray_icon.menu = Menu(
            item(i18n.get("overlay.menu.calibration"), self._create_pystray_profile_submenu()),
            item(i18n.get("overlay.menu.display"), self._create_pystray_display_mode_submenu()),
            item(i18n.get("overlay.menu.timer"), self._create_pystray_timer_adjust_submenu()),
            Menu.SEPARATOR,
            item(i18n.get("overlay.menu.about", version=VERSION), self._open_about_page),
            item(i18n.get("overlay.menu.exit"), self._schedule_quit)
        )
        logger.debug("托盘菜单已更新。")

    def _rename_profile(self, filename: str):
        logger.info(f"请求重命名配置文件: {filename}")
        self.root.after(0, self._show_rename_dialog, filename)

    def _show_rename_dialog(self, filename: str):
        old_basename = get_calibration_basename(filename)
        new_basename = Querybox.get_string(prompt=i18n.get("overlay.dialog.rename.prompt", old_basename=old_basename), title=i18n.get("overlay.dialog.rename.title"),
                                           initialvalue=old_basename, parent=self.root)
        if new_basename and new_basename.strip():
            logger.info(f"用户为 '{filename}' 输入新名称: '{new_basename.strip()}'，发送指令。")
            self.master_callback({"type": "rename_profile", "old": filename, "new_base": new_basename.strip()})
        elif new_basename is not None:
            logger.warning("用户输入了无效的空名称。")
            Messagebox.show_warning(i18n.get("overlay.error.name_empty"), title=i18n.get("overlay.error.name_empty.title"), parent=self.root)
        else:
            logger.debug("用户取消了重命名操作。")

    def _delete_profile(self, filename: str):
        logger.info(f"请求删除配置文件: {filename}")
        self.root.after(0, self._show_delete_dialog, filename)

    def _show_delete_dialog(self, filename: str):
        basename = get_calibration_basename(filename)
        result = Messagebox.yesno(message=i18n.get("overlay.dialog.delete.msg", basename=basename), title=i18n.get("overlay.dialog.delete.title"), parent=self.root)
        if result == "Yes" or "确认":
            logger.info(f"用户确认删除 '{filename}'，发送指令。")
            self.master_callback({"type": "delete_profile", "filename": filename})
        else:
            logger.debug("用户取消了删除操作。")

    def _setup_tray_icon(self):
        if not TRAY_SUPPORTED: return
        logger.info("正在设置系统托盘图标...")
        try:
            icon_path = resource_path(os.path.join("./icons", "deco.png"))
            icon_image = Image.open(icon_path)
            self.tray_icon = Icon("ArknightsCostBarRuler", icon_image, "明日方舟费用条尺子")
            self._update_tray_menu()
            self.tray_icon.run_detached()
            logger.info("托盘图标已启动。")
        except Exception as e:
            logger.exception(f"创建托盘图标失败: {e}")

    def set_state_idle(self):
        logger.info("UI状态切换: idle")
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('deco'), command=None)
        self.pre_cal_label.config(text=i18n.get("overlay.msg.idle"))
        self.pre_cal_label.place(relx=0.5, rely=0.5, anchor="center")
        self.active_profile_filename = None
        self.current_cycle_total_frames = 0
        if TRAY_SUPPORTED: self._update_tray_menu()

    def set_state_pre_calibration(self):
        logger.info("UI状态切换: pre_calibration")
        self._hide_all_dynamic_labels()
        self.icon_button.config(image=self.icons.get('start'),
                                command=lambda: self.master_callback({"type": "start_calibration"}))
        self.pre_cal_label.config(text=i18n.get("overlay.msg.pre_cal"))
        self.pre_cal_label.place(relx=0.5, rely=0.5, anchor="center")
        self.active_profile_filename = None
        self.current_cycle_total_frames = 0
        if TRAY_SUPPORTED: self._update_tray_menu()

    def set_state_calibrating(self):
        logger.info("UI状态切换: calibrating")
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
        logger.debug(f"窗口拖动结束，当前位置: {self.root.winfo_x()}, {self.root.winfo_y()}")

    def _on_drag_motion(self, event):
        dx = event.x - self._drag_data["x"]
        dy = event.y - self._drag_data["y"]
        x = self.root.winfo_x() + dx
        y = self.root.winfo_y() + dy
        self.root.geometry(f"+{x}+{y}")