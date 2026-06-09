//! 费用条位置数据收集工具(helper)。
//!
//! 在最高抓帧率下连续截图,记录费用条「已填充像素宽度」随时间的变化,按费用循环切分后
//! 导出 CSV + 元数据。用于在不同 (分辨率, 回费效率) 条件下采集数据,以便验证
//! 「pixel_map 能否由 (分辨率, 回费效率) 直接算出」这一假设。
//!
//! 复用 ruler-core 现有公共接口,零新依赖。
//!
//! 用法:
//!   cargo run --release -p ruler-core --example collect_cost_data -- --label "rate1.0"
//!
//! 参数:
//!   --label <str>        必填。标注本次回费效率/环境条件,如 rate1.0、三减费。分辨率会自动从截图读出。
//!   --config <path>      config.json 路径,默认工作区根目录(复用 app 已配置好的捕获后端)。
//!   --out <dir>          输出目录,默认 ./cost_data。
//!   --cycles <n>         采满多少个完整费用循环后停止,默认 8;设为 0 表示只受 --max-seconds 限制。
//!   --max-seconds <sec>  安全运行上限,默认 180。
//!
//! 建议:进关卡后选中一个地图干员进入慢速模式,让数据采集尽量覆盖每个逻辑帧。

use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ruler_core::analysis::roi::find_cost_bar_roi;
use ruler_core::analysis::scanner::get_raw_filled_pixel_width;
use ruler_core::capture::create_backend;
use ruler_core::config::RulerConfig;

/// 一个采集完成的完整费用循环。
struct CompletedCycle {
    index: i32,
    start_ms: f64,
    end_ms: f64,
    /// 该循环内的有效样本 (t_ms, pixel_width),不含 None。
    samples: Vec<(f64, i32)>,
}

struct Options {
    label: String,
    config_path: PathBuf,
    out_dir: PathBuf,
    cycles_target: usize,
    max_seconds: f64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("错误: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = match parse_args()? {
        Some(options) => options,
        None => return Ok(()), // --help
    };

    let started_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // 1) 读取配置并连接捕获后端(复用 app 的 config.json)。
    let config = RulerConfig::load_from_path(&options.config_path).map_err(|e| e.to_string())?;
    let capture_type = config.capture_type.clone();
    let capture_config = config.to_capture_config().map_err(|e| e.to_string())?;
    let mut backend = create_backend(capture_config)?;
    backend.connect()?;

    // 2) 抓一帧确定分辨率与 ROI / 条宽(与 worker::collect_calibration_samples 一致,用首帧尺寸)。
    let probe = backend.capture_frame()?;
    let screen_width = probe.width;
    let screen_height = probe.height;
    let roi = find_cost_bar_roi(screen_width as i32, screen_height as i32);
    let total_bar_width = roi.1 - roi.0;
    if total_bar_width <= 0 {
        return Err(format!(
            "费用条 ROI 宽度无效:{total_bar_width}(分辨率 {screen_width}x{screen_height})"
        ));
    }

    println!("== 费用条数据收集 ==");
    println!("label           : {}", options.label);
    println!("capture_type    : {capture_type}");
    println!("分辨率          : {screen_width}x{screen_height}");
    println!("ROI (x1,x2,y)   : ({}, {}, {})", roi.0, roi.1, roi.2);
    println!("条宽 W          : {total_bar_width}px");
    if options.cycles_target > 0 {
        println!("目标循环数      : {}", options.cycles_target);
    } else {
        println!("目标循环数      : 不限(仅受 max-seconds 约束)");
    }
    println!("max-seconds     : {}", options.max_seconds);
    println!("提示:请让游戏处于慢速模式(选中一个地图干员),开始采集...\n");

    // 3) 紧循环抓帧,记录 (cycle_index, t_ms, pixel_width)。
    let high_threshold = total_bar_width as f64 * 0.9;
    let low_threshold = total_bar_width as f64 * 0.1;

    let mut raw_samples: Vec<(i32, f64, Option<i32>)> = Vec::new();
    let mut completed_cycles: Vec<CompletedCycle> = Vec::new();
    let mut current: Vec<(f64, i32)> = Vec::new();
    let mut current_start_ms = 0.0_f64;
    let mut cycle_index: i32 = -1;
    let mut prev: Option<i32> = None;

    let start = Instant::now();
    let mut frame = probe;
    let mut status_timer = Instant::now();
    let mut ever_read = false;
    let mut warned_no_read = false;

    loop {
        let t_ms = start.elapsed().as_secs_f64() * 1000.0;
        let width_opt =
            get_raw_filled_pixel_width(&frame.data, frame.width, frame.height, frame.format, roi);

        // 循环边界:上一帧接近满、当前帧接近空 → 新循环开始。
        if let (Some(p), Some(c)) = (prev, width_opt) {
            if (p as f64) > high_threshold && (c as f64) < low_threshold {
                if cycle_index >= 0 && !current.is_empty() {
                    completed_cycles.push(CompletedCycle {
                        index: cycle_index,
                        start_ms: current_start_ms,
                        end_ms: t_ms,
                        samples: std::mem::take(&mut current),
                    });
                    eprintln!(
                        "\n[+] 已采集完整循环 {} 个{}",
                        completed_cycles.len(),
                        if options.cycles_target > 0 {
                            format!(" / {}", options.cycles_target)
                        } else {
                            String::new()
                        }
                    );
                }
                cycle_index += 1;
                current.clear();
                current_start_ms = t_ms;
            }
        }

        raw_samples.push((cycle_index, t_ms, width_opt));
        if cycle_index >= 0 {
            if let Some(c) = width_opt {
                current.push((t_ms, c));
            }
        }
        prev = width_opt;

        if width_opt.is_some() {
            ever_read = true;
        }

        // 实时状态:让你能看到读数在变、采了几个循环(也用于诊断捕获是否正常)。
        if status_timer.elapsed() >= Duration::from_millis(300) {
            let now_elapsed = start.elapsed().as_secs_f64();
            let fps_now = raw_samples.len() as f64 / now_elapsed.max(1e-6);
            let cur = match width_opt {
                Some(value) => format!("{value}px"),
                None => "None".to_string(),
            };
            let target = if options.cycles_target > 0 {
                options.cycles_target.to_string()
            } else {
                "∞".to_string()
            };
            eprint!(
                "\r采集中  t={now_elapsed:5.1}s  fps={fps_now:4.0}  当前={cur:>6}  完整循环={}/{target}    ",
                completed_cycles.len()
            );
            let _ = std::io::stderr().flush();
            status_timer = Instant::now();

            if !ever_read && !warned_no_read && now_elapsed > 5.0 {
                eprintln!(
                    "\n警告:已 5 秒读不到费用条(当前=None)。请检查:游戏窗口是否被遮挡/最小化、\
                     是否已进入关卡、费用条是否在画面右下角可见。继续尝试中(Ctrl-C 可随时中止)..."
                );
                warned_no_read = true;
            }
        }

        let elapsed = start.elapsed().as_secs_f64();
        let reached_cycles =
            options.cycles_target > 0 && completed_cycles.len() >= options.cycles_target;
        if reached_cycles || elapsed >= options.max_seconds {
            break;
        }

        match backend.capture_frame() {
            Ok(next) => frame = next,
            Err(error) => {
                eprintln!("捕获出错,提前结束并保存已采数据: {error}");
                break;
            }
        }
    }
    eprintln!(); // 收尾,结束 \r 实时状态行

    let total_elapsed = start.elapsed().as_secs_f64();
    backend.disconnect();

    let raw_count = raw_samples.len() as u64;
    let measured_capture_fps = if total_elapsed > 0.0 {
        raw_count as f64 / total_elapsed
    } else {
        0.0
    };

    if completed_cycles.is_empty() {
        eprintln!("警告:未采集到任何完整费用循环。请确认费用条可见、正在自然回复,且处于慢速模式。");
    }

    // 4) 写出文件。
    fs::create_dir_all(&options.out_dir)
        .map_err(|e| format!("创建输出目录失败 {}: {e}", options.out_dir.display()))?;
    let stem = format!("{}_{}", sanitize(&options.label), started_unix);

    let raw_path = options.out_dir.join(format!("{stem}_raw.csv"));
    write_file(&raw_path, &build_raw_csv(&raw_samples))?;

    let cycles_path = options.out_dir.join(format!("{stem}_cycles.csv"));
    write_file(&cycles_path, &build_cycles_csv(&completed_cycles))?;

    let meta = serde_json::json!({
        "label": options.label,
        "config_path": options.config_path.display().to_string(),
        "capture_type": capture_type,
        "screen_width": screen_width,
        "screen_height": screen_height,
        "roi": [roi.0, roi.1, roi.2],
        "total_bar_width": total_bar_width,
        "measured_capture_fps": measured_capture_fps,
        "cycles_collected": completed_cycles.len(),
        "raw_samples": raw_count,
        "elapsed_seconds": total_elapsed,
        "started_unix": started_unix,
    });
    let meta_path = options.out_dir.join(format!("{stem}_meta.json"));
    let meta_str = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    write_file(&meta_path, &meta_str)?;

    // 5) 打印摘要(便于当场看 / 直接发出来分析)。
    print_summary(&completed_cycles, total_bar_width, measured_capture_fps);
    println!("\n已写出:");
    println!("  {}", raw_path.display());
    println!("  {}", cycles_path.display());
    println!("  {}", meta_path.display());

    Ok(())
}

fn build_raw_csv(raw_samples: &[(i32, f64, Option<i32>)]) -> String {
    let mut buffer = String::new();
    let _ = writeln!(buffer, "cycle_index,t_ms,pixel_width");
    for (cycle_index, t_ms, width_opt) in raw_samples {
        match width_opt {
            Some(width) => {
                let _ = writeln!(buffer, "{cycle_index},{t_ms:.3},{width}");
            }
            None => {
                let _ = writeln!(buffer, "{cycle_index},{t_ms:.3},");
            }
        }
    }
    buffer
}

fn build_cycles_csv(cycles: &[CompletedCycle]) -> String {
    let mut buffer = String::new();
    let _ = writeln!(
        buffer,
        "cycle_index,duration_ms,n_samples,n_unique_widths,min_width,max_width,unique_widths"
    );
    for cycle in cycles {
        let unique = unique_widths(cycle);
        let duration = cycle.end_ms - cycle.start_ms;
        let min = unique.first().copied().unwrap_or(0);
        let max = unique.last().copied().unwrap_or(0);
        let widths = unique
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(
            buffer,
            "{},{:.3},{},{},{},{},{}",
            cycle.index,
            duration,
            cycle.samples.len(),
            unique.len(),
            min,
            max,
            widths
        );
    }
    buffer
}

/// 该循环内出现过的、排序去重后的像素宽度。
fn unique_widths(cycle: &CompletedCycle) -> Vec<i32> {
    let mut widths: Vec<i32> = cycle.samples.iter().map(|(_, w)| *w).collect();
    widths.sort_unstable();
    widths.dedup();
    widths
}

fn print_summary(cycles: &[CompletedCycle], total_bar_width: i32, fps: f64) {
    println!("\n== 摘要 ==");
    println!("实测抓帧率      : {fps:.1} fps");
    println!("完整循环数      : {}", cycles.len());
    if cycles.is_empty() {
        return;
    }
    println!(
        "\n{:>6}  {:>11}  {:>9}  {:>9}  {:>5}  {:>5}",
        "cycle", "duration_ms", "n_sample", "n_unique", "min", "max"
    );
    for cycle in cycles {
        let unique = unique_widths(cycle);
        println!(
            "{:>6}  {:>11.1}  {:>9}  {:>9}  {:>5}  {:>5}",
            cycle.index,
            cycle.end_ms - cycle.start_ms,
            cycle.samples.len(),
            unique.len(),
            unique.first().copied().unwrap_or(0),
            unique.last().copied().unwrap_or(0),
        );
    }

    // 给一个快速参考:若线性填充且每循环 N 个离散帧,则步长 ≈ W / N。
    let n_values: Vec<usize> = cycles.iter().map(|c| unique_widths(c).len()).collect();
    if let Some(max_n) = n_values.iter().copied().max() {
        if max_n > 0 {
            println!(
                "\n参考:条宽 W={total_bar_width}px;若 total_frames={max_n},线性步长 ≈ {:.2}px/帧",
                total_bar_width as f64 / max_n as f64
            );
        }
    }
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|e| format!("写入失败 {}: {e}", path.display()))
}

/// 把 label 清理成文件名安全的形式(保留中文,替换空白与非法字符)。
fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_whitespace() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut config_path: Option<PathBuf> = None;
    let mut label: Option<String> = None;
    let mut out_dir = PathBuf::from("cost_data");
    let mut cycles_target: usize = 8;
    let mut max_seconds: f64 = 180.0;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = Some(PathBuf::from(require_value(&mut args, "--config")?)),
            "--label" => label = Some(require_value(&mut args, "--label")?),
            "--out" => out_dir = PathBuf::from(require_value(&mut args, "--out")?),
            "--cycles" => {
                cycles_target = require_value(&mut args, "--cycles")?
                    .parse()
                    .map_err(|_| "--cycles 需要一个非负整数".to_string())?;
            }
            "--max-seconds" => {
                max_seconds = require_value(&mut args, "--max-seconds")?
                    .parse()
                    .map_err(|_| "--max-seconds 需要一个数字".to_string())?;
            }
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            other => return Err(format!("未知参数: {other}(用 --help 查看用法)")),
        }
    }

    let label = label.ok_or_else(|| "缺少必填参数 --label(标注本次回费效率条件)".to_string())?;
    let config_path = config_path.unwrap_or_else(default_config_path);

    Ok(Some(Options {
        label,
        config_path,
        out_dir,
        cycles_target,
        max_seconds,
    }))
}

fn require_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} 需要一个值"))
}

/// 默认 config.json:工作区根目录(与 ruler-app 的 ResourceLocator::config_path 同路径)。
fn default_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("config.json")
}

fn print_help() {
    println!(
        "费用条位置数据收集工具\n\n\
用法:\n  \
cargo run --release -p ruler-core --example collect_cost_data -- --label <条件> [选项]\n\n\
参数:\n  \
--label <str>        必填。标注本次回费效率/环境条件(如 rate1.0、三减费)。分辨率自动读出。\n  \
--config <path>      config.json 路径,默认工作区根目录的 config.json。\n  \
--out <dir>          输出目录,默认 ./cost_data。\n  \
--cycles <n>         采满多少个完整费用循环后停止,默认 8;0 表示仅受 --max-seconds 限制。\n  \
--max-seconds <sec>  安全运行上限,默认 180。\n  \
-h, --help           显示本帮助。\n\n\
提示:进关卡后选中一个地图干员进入慢速模式,再开始采集,可采到尽量多的离散像素宽度。"
    );
}
