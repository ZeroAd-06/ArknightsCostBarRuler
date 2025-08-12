# ark_timeline_tool/config.py

# --- 项目信息 ---
VERSION = "v0.5"

# --- 应用设置 ---
FPS = 30
NODE_COLORS = ["#ff6347", "#4682b4", "#32cd32", "#ffd700", "#9370db", "#ffa500"]
WINDOW_WIDTH = 1000
WINDOW_HEIGHT = WINDOW_WIDTH // 5
DEFAULT_ALPHA = 0.85
ICON_SIZE = (30, 30)

# --- 时间轴与节点（美化后配置） ---
PIXELS_PER_FRAME = 2            # 每个逻辑帧在时间轴上占用的像素

NODE_FIND_TOLERANCE = 3         # 光标附近查找节点的容差（帧）
NODE_CLICK_TOLERANCE = 15       # 点击节点的容差范围（帧），比查找范围大更易于点击
NODE_DIAMOND_SIZE = {"h": 10, "w": 7} # 节点菱形的大小
NODE_OUTLINE_COLOR = "#FFFFFF"  # 节点默认轮廓颜色
NODE_SELECTED_OUTLINE_COLOR = "#00FFFF" # 选中时节点的高亮轮廓颜色
NODE_SELECTED_SCALE = 1.5       # 选中时节点的放大倍数

# --- 交互手感 (美化后配置) ---
TIMELINE_DRAG_SENSITIVITY = 2   # 时间轴拖动灵敏度，分母越小越灵敏
MAGNET_BREAK_THRESHOLD = 30     # 在磁铁模式下拖动超过多少像素时自动关闭磁铁模式
INERTIA_FRICTION = 0.92         # 惯性滚动的摩擦力，越接近1滚动越远

# --- 时间轴视觉 (美化后新增) ---
TIMELINE_TRACK_COLOR = "#2c313a" # 时间轴轨道的背景色
TIMELINE_TRACK_HEIGHT = 40      # 时间轴轨道的高度（像素）
TIMELINE_TICK_COLOR = "#abb2bf"  # 时间刻度线（秒）的主颜色
TIMELINE_SUBTICK_COLOR = "#5c6370" # 时间刻度线（帧）的次颜色
TIMELINE_MAJOR_TICK_H = 10      # 秒级刻度线半高
TIMELINE_MINOR_TICK_H = 5       # 帧级刻度线半高
TIMELINE_SUBTICK_INTERVAL = 10  # 每隔多少帧画一个子刻度线

# --- 网络设置 ---
WEBSOCKET_URI = "ws://localhost:2606"
WEBSOCKET_RECONNECT_DELAY = 5
QUEUE_POLL_INTERVAL = 16 # ~60 FPS

# --- 日志和资源目录 ---
LOG_DIR = "logs"
ICON_DIR = "icons"
