# ark_timeline_tool/app.py

import tkinter as tk
from tkinter import ttk, simpledialog, TclError
import ttkbootstrap as ttkb
from ttkbootstrap.constants import *
from ttkbootstrap.tooltip import ToolTip
import queue
import logging
import os
from PIL import Image, ImageTk

try:
    import winsound

    HAS_WINSOUND = True
except ImportError:
    HAS_WINSOUND = False
    logging.warning("'winsound' 模块未找到，声音提醒功能将不可用。")

import config
from utils import resource_path, format_frame_time
from file_io import load_timeline_from_file, save_timeline_to_file
from websocket_client import WebsocketClient

logger = logging.getLogger(__name__)


class TimelineApp:
    def __init__(self, root):
        self.root = root
        self._configure_root_window()

        # --- 核心状态 ---
        self.mode = tk.StringVar(value="打轴模式")
        self.timeline_data = []
        self.current_game_frame = 0
        self.timeline_offset = 0
        self.magnet_mode = tk.BooleanVar(value=True)
        self.current_next_node = None

        # --- 提醒功能配置 ---
        self.sound_alert_enabled = tk.BooleanVar(value=True)
        self.visual_alert_enabled = tk.BooleanVar(value=True)
        self.alert_lead_frames = {"sound": 60, "visual": 60}

        self.last_sound_alert_frame = -1
        # is_flashing 现在表示是否正处于持续闪烁的状态
        self.is_flashing = False

        self.alert_lead_var = tk.StringVar()
        self.alert_lead_var.set(str(self.alert_lead_frames["visual"]))
        self.alert_lead_var.trace_add("write", self._update_alert_lead)

        # --- 拖动数据 ---
        self._window_drag_data = {"x": 0, "y": 0}
        self._timeline_drag_data = {"x": 0}

        # --- 通信队列与图标 ---
        self.ws_queue = queue.Queue()
        self.icons = {}
        self._load_icons()

        # --- UI设置与启动 ---
        self._setup_styles()
        self._setup_ui()
        self.mode.trace_add("write", self._update_ui_for_mode)
        self._update_ui_for_mode()

        # --- 启动后台服务与UI更新循环 ---
        self.ws_client = WebsocketClient(config.WEBSOCKET_URI)
        self.ws_client.start(self.ws_queue)
        self.root.after(config.QUEUE_POLL_INTERVAL, self._process_ws_queue)
        logger.info(f"TimelineApp {config.VERSION} 初始化完成。")

    def _configure_root_window(self):
        self.root.title(f"明日方舟打轴/对轴器 {config.VERSION}")
        self.root.geometry(f"{config.WINDOW_WIDTH}x{config.WINDOW_HEIGHT}+100+100")
        self.root.overrideredirect(True)
        self.root.wm_attributes("-topmost", True)
        self.root.wm_attributes("-alpha", config.DEFAULT_ALPHA)

    def _load_icons(self):
        icon_files = {"open": "open.png", "save": "save.png", "magnet_on": "magnet_on.png",
                      "magnet_off": "magnet_off.png", "add": "add.png", "remove": "remove.png", "color": "color.png",
                      "sound_on": "sound_on.png", "sound_off": "sound_off.png", "visual_on": "visual_on.png",
                      "visual_off": "visual_off.png", "rename": "rename.png"}
        for name, filename in icon_files.items():
            path = resource_path(os.path.join(config.ICON_DIR, filename))
            try:
                img = Image.open(path).resize(config.ICON_SIZE, Image.Resampling.LANCZOS)
                self.icons[name] = ImageTk.PhotoImage(img)
            except FileNotFoundError:
                self.icons[name] = None
                logger.error(f"图标文件未找到: {path}")

    def _setup_styles(self):
        style = ttkb.Style.get_instance()
        style.configure("TFrame", background="#282c34")
        style.configure("TLabel", background="#282c34", foreground="#abb2bf")
        style.configure("Tool.TButton", background="#282c34", borderwidth=0, focuscolor="#282c34", padding=0)
        style.map("Tool.TButton", background=[("active", "#3e4451")])
        style.configure("Info.TLabel", font=("Segoe UI", 12))

    def _setup_ui(self):
        main_frame = ttk.Frame(self.root, style="TFrame")
        main_frame.pack(expand=True, fill=BOTH)
        self.ops_frame = ttk.Frame(main_frame, width=config.WINDOW_WIDTH // 4, style="TFrame")
        self.ops_frame.pack(side=LEFT, fill=Y, padx=5, pady=5)
        self.ops_frame.pack_propagate(False)
        self.dynamic_ops_frame = ttk.Frame(self.ops_frame, style="TFrame")
        self.dynamic_ops_frame.pack(side=TOP, fill=BOTH, expand=True)
        self.dynamic_ops_frame.columnconfigure((0, 1, 2), weight=1)
        display_frame = ttk.Frame(main_frame, style="TFrame")
        display_frame.pack(side=LEFT, expand=True, fill=BOTH)
        self.timeline_canvas = tk.Canvas(display_frame, bg="#21252b", highlightthickness=0)
        self.timeline_canvas.place(relx=0, rely=0, relwidth=1.0, relheight=2 / 3)
        self.timeline_canvas.bind("<ButtonPress-1>", self._on_timeline_drag_start)
        self.timeline_canvas.bind("<B1-Motion>", self._on_timeline_drag_motion)
        info_frame = ttk.Frame(display_frame, style="TFrame")
        info_frame.place(relx=0, rely=2 / 3, relwidth=1.0, relheight=1 / 3)
        self.info_time_label = ttk.Label(info_frame, text="00:00:00", style="Info.TLabel",
                                         font=("Segoe UI", 12, "bold"))
        self.info_time_label.pack(side=LEFT, padx=(10, 0))
        self.info_diamond_label = ttk.Label(info_frame, text="", style="Info.TLabel")
        self.info_diamond_label.pack(side=LEFT, padx=(5, 0))
        self.info_name_label = ttk.Label(info_frame, text="", style="Info.TLabel", cursor="hand2")
        self.info_name_label.pack(side=LEFT, padx=(0, 5))
        self.info_name_label.bind("<Button-1>", self._on_next_node_name_click)
        self.info_remaining_label = ttk.Label(info_frame, text="", style="Info.TLabel", foreground="gray")
        self.info_remaining_label.pack(side=LEFT, padx=5)
        for widget in (info_frame, self.info_time_label, self.info_diamond_label, self.info_name_label,
                       self.info_remaining_label):
            widget.bind("<ButtonPress-1>", self._on_window_drag_start)
            widget.bind("<B1-Motion>", self._on_window_drag_motion)
        quit_button = ttk.Button(self.ops_frame, text="退出", command=self.root.quit, style="Danger.TButton")
        quit_button.pack(side=BOTTOM, fill=X, pady=(0, 5))
        switch_frame = ttk.Frame(self.ops_frame, style="TFrame")
        switch_frame.pack(side=BOTTOM, fill=X, pady=5)
        switch_frame.columnconfigure((0, 1), weight=1)
        ttk.Radiobutton(switch_frame, text="打轴", variable=self.mode, value="打轴模式",
                        style="Outline.Toolbutton").grid(row=0, column=0, sticky="ew", padx=1)
        ttk.Radiobutton(switch_frame, text="对轴", variable=self.mode, value="对轴模式",
                        style="Outline.Toolbutton").grid(row=0, column=1, sticky="ew", padx=1)

    def _process_ws_queue(self):
        try:
            while not self.ws_queue.empty():
                data = self.ws_queue.get_nowait()
                if data.get("isRunning"):
                    self.current_game_frame = data.get("totalElapsedFrames", self.current_game_frame)
            self._update_display()
        except queue.Empty:
            pass
        finally:
            self.root.after(config.QUEUE_POLL_INTERVAL, self._process_ws_queue)

    def _update_ui_for_mode(self, *args):
        for widget in self.dynamic_ops_frame.winfo_children():
            widget.destroy()
        for i in range(4): self.dynamic_ops_frame.rowconfigure(i, weight=1)
        if self.mode.get() == "打轴模式":
            self.magnet_mode.set(False)
            self._create_editing_buttons()
        else:
            self.magnet_mode.set(True)
            self._create_following_buttons()

    def _create_grid_button(self, parent, r, c, text, icon_name, command):
        icon = self.icons.get(icon_name)
        btn = ttk.Button(parent, command=command, style="Tool.TButton")
        if icon:
            btn.config(image=icon)
            ToolTip(btn, text=text)
        else:
            btn.config(text=text)
        btn.grid(row=r, column=c, padx=2, pady=2, sticky="nsew")
        return btn

    def _create_grid_toggle_button(self, parent, r, c, text_on, text_off, var, on_icon, off_icon, command=None):
        btn = ttk.Button(parent, style="Tool.TButton")
        tooltip = ToolTip(btn)

        def update_display():
            is_on = var.get()
            icon_name = on_icon if is_on else off_icon
            current_text = text_on if is_on else text_off
            icon = self.icons.get(icon_name)
            tooltip.text = current_text
            if icon:
                btn.config(image=icon, text="")
            else:
                btn.config(text=current_text, image="")

        def toggler():
            var.set(not var.get())
            update_display()
            if command: command()

        btn.config(command=toggler)
        update_display()
        btn.grid(row=r, column=c, padx=2, pady=2, sticky="nsew")
        return btn

    def _create_editing_buttons(self):
        frame = self.dynamic_ops_frame
        self._create_grid_button(frame, 0, 0, "打开", "open", self._load_timeline)
        self._create_grid_button(frame, 0, 1, "保存", "save", self._save_timeline)
        self.add_remove_btn = self._create_grid_button(frame, 0, 2, "添加/移除", "add",
                                                       self._add_or_remove_node_at_cursor)
        self._create_grid_button(frame, 1, 0, "切换颜色", "color", self._change_node_color_at_cursor)
        self._create_grid_button(frame, 1, 1, "重命名", "rename", self._rename_node_at_cursor)

        def on_magnet_toggle():
            if not self.magnet_mode.get():
                self.timeline_offset = self.current_game_frame
                logger.debug(f"手动关闭磁铁模式，时间轴位置同步到: {self.timeline_offset}")

        self._create_grid_toggle_button(frame, 1, 2, "磁铁: 开", "磁铁: 关", self.magnet_mode, "magnet_on",
                                        "magnet_off", command=on_magnet_toggle)

    def _create_following_buttons(self):
        frame = self.dynamic_ops_frame
        self._create_grid_button(frame, 0, 0, "打开", "open", self._load_timeline)
        self._create_grid_toggle_button(frame, 0, 1, "声音提醒: 开", "声音提醒: 关", self.sound_alert_enabled,
                                        "sound_on", "sound_off")
        self._create_grid_toggle_button(frame, 0, 2, "视觉提醒: 开", "视觉提醒: 关", self.visual_alert_enabled,
                                        "visual_on", "visual_off")
        lead_frame = ttk.Frame(frame, style="TFrame")
        lead_frame.grid(row=1, column=0, columnspan=3, sticky="ew", pady=(15, 0))
        ttk.Label(lead_frame, text="提醒提前(帧):", font=("Segoe UI", 8)).pack(side=LEFT, padx=5)
        spinbox = ttk.Spinbox(
            lead_frame,
            from_=0,
            to_=300,
            textvariable=self.alert_lead_var,
            width=5
        )
        spinbox.pack(side=LEFT, padx=5)
        spinbox.bind('<Return>', self._update_alert_lead)

    def _update_alert_lead(self, *args):
        try:
            frames_str = self.alert_lead_var.get()
            if frames_str:
                frames = int(frames_str)
                frames = max(0, min(frames, 300))
                self.alert_lead_frames["sound"] = frames
                self.alert_lead_frames["visual"] = frames
                logger.debug(f"提醒提前时间已更新为: {frames} 帧")
        except (ValueError, TclError):
            logger.warning("输入的提醒提前帧数无效。")
            pass

    def _update_display(self):
        canvas = self.timeline_canvas
        canvas.delete("all")
        width, height = canvas.winfo_width(), canvas.winfo_height()
        if width <= 1 or height <= 1: return
        center_frame = self.get_current_display_frame()
        pixels_per_frame = 2
        canvas.create_line(0, height / 2, width, height / 2, fill="#abb2bf")
        canvas.create_line(width / 2, height / 2 - 15, width / 2, height / 2 + 15, fill="#00FFFF", width=2)
        for node in self.timeline_data:
            frame_diff = node["frame"] - center_frame
            x_pos = width / 2 + frame_diff * pixels_per_frame
            if 0 < x_pos < width:
                h = config.NODE_DIAMOND_SIZE["h"]
                w = config.NODE_DIAMOND_SIZE["w"]
                points = [x_pos, height / 2 - h, x_pos + w, height / 2, x_pos, height / 2 + h, x_pos - w, height / 2]
                canvas.create_polygon(points, fill=node["color"], outline=node["color"], tags=f"node_{node['frame']}")
                canvas.create_text(x_pos, height / 2 + (h + 10), text=node["name"], fill="white", font=("Segoe UI", 9))
        playhead_x = width / 2 + (self.current_game_frame - center_frame) * pixels_per_frame
        canvas.create_line(playhead_x, 0, playhead_x, height, fill="#ff6347", width=2, dash=(4, 2))
        current_display_frame = self.get_current_display_frame()
        self.info_time_label.config(text=format_frame_time(current_display_frame))
        self.current_next_node = self._find_next_node(current_display_frame)
        if self.current_next_node:
            node = self.current_next_node
            time_to_next = node['frame'] - current_display_frame
            self.info_diamond_label.config(text=" ♦", foreground=node['color'])
            self.info_name_label.config(text=f" {node['name']}({format_frame_time(node['frame'])})")
            self.info_remaining_label.config(text=f" {time_to_next}帧后")
            if self.mode.get() == "对轴模式": self._handle_alerts(time_to_next, node['frame'])
        else:
            # 如果没有下一个节点，确保停止所有提醒
            if self.mode.get() == "对轴模式": self._handle_alerts(-1, -1)
            self.info_diamond_label.config(text="")
            self.info_name_label.config(text="")
            self.info_remaining_label.config(text="")
        if self.mode.get() == "打轴模式" and hasattr(self, 'add_remove_btn'):
            node_at_cursor = self._find_node_at(current_display_frame, tolerance=config.NODE_FIND_TOLERANCE)
            icon_name = "remove" if node_at_cursor else "add"
            text = "移除节点" if node_at_cursor else "添加节点"
            icon = self.icons.get(icon_name)
            if icon:
                self.add_remove_btn.config(image=icon)
                ToolTip(self.add_remove_btn, text=text)

    def _on_timeline_drag_start(self, event):
        if not self.magnet_mode.get():
            width = self.timeline_canvas.winfo_width()
            pixels_per_frame = 2
            clicked_frame = self.timeline_offset + int((event.x - width / 2) / pixels_per_frame)
            node_to_snap = self._find_node_at(clicked_frame, tolerance=config.NODE_CLICK_TOLERANCE)
            if node_to_snap:
                logger.info(f"吸附到节点: {node_to_snap['name']} ({node_to_snap['frame']})")
                self.timeline_offset = node_to_snap['frame']
                return
        self._timeline_drag_data["x"] = event.x

    def _on_timeline_drag_motion(self, event):
        dx = event.x - self._timeline_drag_data["x"]
        if self.magnet_mode.get():
            if abs(dx) > config.MAGNET_BREAK_THRESHOLD:
                logger.info("通过大幅度拖拽已脱离磁铁模式。")
                self.magnet_mode.set(False)
                self.timeline_offset = self.current_game_frame
                self.timeline_offset -= int(dx / config.TIMELINE_DRAG_SENSITIVITY)
        else:
            self.timeline_offset -= int(dx / config.TIMELINE_DRAG_SENSITIVITY)
        self._timeline_drag_data["x"] = event.x

    def _find_node_at(self, frame, tolerance=config.NODE_FIND_TOLERANCE):
        for node in self.timeline_data:
            if abs(node["frame"] - frame) <= tolerance:
                return node
        return None

    def get_current_display_frame(self):
        return self.current_game_frame if self.magnet_mode.get() else self.timeline_offset

    def _on_window_drag_start(self, event):
        if event.widget == self.info_name_label and self.mode.get() == "打轴模式": return
        self._window_drag_data = {"x": event.x, "y": event.y}

    def _on_window_drag_motion(self, event):
        if event.widget == self.info_name_label and self.mode.get() == "打轴模式": return
        dx = event.x - self._window_drag_data["x"]
        dy = event.y - self._window_drag_data["y"]
        x = self.root.winfo_x() + dx
        y = self.root.winfo_y() + dy
        self.root.geometry(f"+{x}+{y}")

    def _load_timeline(self):
        loaded_data = load_timeline_from_file(self.root)
        if loaded_data is not None:
            self.timeline_data = loaded_data

    def _save_timeline(self):
        save_timeline_to_file(self.timeline_data, self.root)

    def _find_next_node(self, from_frame):
        return next(
            (node for node in sorted(self.timeline_data, key=lambda x: x['frame']) if node['frame'] > from_frame), None)

    def _add_or_remove_node_at_cursor(self):
        current_frame = self.get_current_display_frame()
        node_to_remove = self._find_node_at(current_frame, tolerance=config.NODE_FIND_TOLERANCE)
        if node_to_remove:
            self.timeline_data.remove(node_to_remove)
            logger.info(f"移除了节点: {node_to_remove['name']}")
        else:
            new_node = {"frame": current_frame, "name": f"操作@{format_frame_time(current_frame)}",
                        "color": config.NODE_COLORS[0]}
            self.timeline_data.append(new_node)
            logger.info(f"添加了新节点在 {current_frame} 帧")

    def _rename_node_logic(self, node_to_rename):
        if not node_to_rename: return
        new_name = simpledialog.askstring("重命名节点", "输入新名称:", initialvalue=node_to_rename.get('name', ''),
                                          parent=self.root)
        if new_name and new_name.strip():
            logger.info(f"节点 '{node_to_rename['name']}' 重命名为 '{new_name.strip()}'")
            node_to_rename['name'] = new_name.strip()

    def _rename_node_at_cursor(self):
        self._rename_node_logic(
            self._find_node_at(self.get_current_display_frame(), tolerance=config.NODE_FIND_TOLERANCE))

    def _on_next_node_name_click(self, event):
        if self.mode.get() == "打轴模式" and self.current_next_node:
            self._rename_node_logic(self.current_next_node)

    def _change_node_color_at_cursor(self):
        node = self._find_node_at(self.get_current_display_frame(), tolerance=config.NODE_FIND_TOLERANCE)
        if node:
            try:
                current_color_index = config.NODE_COLORS.index(node['color'])
                next_color_index = (current_color_index + 1) % len(config.NODE_COLORS)
                node['color'] = config.NODE_COLORS[next_color_index]
            except ValueError:
                node['color'] = config.NODE_COLORS[0]
            logger.debug(f"节点 '{node['name']}' 颜色已更改为 {node['color']}")

    def _handle_alerts(self, time_to_next, node_frame):
        # BUG修复：重写视觉和声音提醒逻辑

        # --- 声音提醒 ---
        if HAS_WINSOUND and self.sound_alert_enabled.get() and \
                0 < time_to_next <= self.alert_lead_frames["sound"] and \
                self.last_sound_alert_frame != node_frame:
            winsound.PlaySound("SystemAsterisk", winsound.SND_ASYNC)
            self.last_sound_alert_frame = node_frame

        # --- 视觉提醒 ---
        # 判断当前是否应该处于闪烁状态
        should_be_flashing = self.visual_alert_enabled.get() and 0 < time_to_next <= self.alert_lead_frames["visual"]

        if should_be_flashing and not self.is_flashing:
            # 如果应该闪烁，但闪烁循环还未开始，则启动它
            self.is_flashing = True
            self._flash_loop()
        elif not should_be_flashing and self.is_flashing:
            # 如果不应闪烁，但闪烁循环仍在运行，则将其关闭
            # _flash_loop 会检测到这个变化并自动停止
            self.is_flashing = False

    def _flash_loop(self):
        # BUG修复：新的持续闪烁循环
        if not self.is_flashing:
            # 状态已被关闭，重置背景颜色并终止循环
            try:
                self.root.config(bg="#282c34")
            except tk.TclError:  # 窗口关闭时可能出错，安全退出
                pass
            return

        try:
            # 切换背景颜色
            current_bg = self.root.cget("bg")
            next_bg = "#ff6347" if current_bg == "#282c34" else "#282c34"
            self.root.config(bg=next_bg)

            # 预约下一次闪烁
            self.root.after(250, self._flash_loop)
        except tk.TclError:  # 窗口关闭时可能出错，安全退出
            pass
