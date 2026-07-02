//! Desktop GUI (eframe/egui), queue-centric layout:
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ Header (app name, theme, settings, about)    │
//! ├──────────────┬───────────────────────────────┤
//! │ Queue        │ Selected job settings         │
//! ├──────────────┴───────────────────────────────┤
//! │ Convert / Progress / collapsible Log         │
//! └──────────────────────────────────────────────┘
//! ```
//!
//! Jobs are BLF files with per-job settings (DBC, output, format, options).
//! Convert processes the whole queue sequentially on a worker thread.

use crate::config::{AppConfig, ThemeChoice};
use blf_decoder::convert::{
    ConvertOptions, OutputLayout, Progress, ProgressUpdate, Summary, TimestampMode, convert,
};
use blf_decoder::dbc::Dbc;
use blf_decoder::decode::{ColumnSpec, list_columns};
use blf_decoder::export::OutputFormat;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO_URL: &str = "https://github.com/sdrdx4100/canbus-dbc";

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "BLF Decoder",
        options,
        Box::new(|cc| {
            let japanese = install_cjk_font(&cc.egui_ctx);
            cc.egui_ctx.all_styles_mut(|style| {
                style.spacing.item_spacing = egui::vec2(8.0, 6.0);
                style.spacing.button_padding = egui::vec2(10.0, 5.0);
            });
            let app = App::new(japanese);
            app.apply_theme(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icon_64.rgba").to_vec(),
        width: 64,
        height: 64,
    }
}

/// Try to load a system font with Japanese glyph coverage. egui's built-in
/// fonts have no CJK glyphs, so without this the UI falls back to English.
fn install_cjk_font(ctx: &egui::Context) -> bool {
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\YuGothM.ttc",
        "C:\\Windows\\Fonts\\msgothic.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".to_owned(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            if let Some(list) = fonts.families.get_mut(&family) {
                list.push("cjk".to_owned());
            }
        }
        ctx.set_fonts(fonts);
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Localised labels
// ---------------------------------------------------------------------------

struct Labels {
    queue: &'static str,
    add_files: &'static str,
    clear_queue: &'static str,
    retry_failed: &'static str,
    move_up: &'static str,
    move_down: &'static str,
    delete: &'static str,
    empty_queue_hint: &'static str,
    no_job_selected: &'static str,
    blf_file: &'static str,
    dbc_file: &'static str,
    dbc_auto: &'static str,
    dbc_missing: &'static str,
    out_dir: &'static str,
    browse: &'static str,
    format: &'static str,
    options: &'static str,
    relative_ts: &'static str,
    keep_can_id: &'static str,
    only_selected: &'static str,
    skip_unknown: &'static str,
    overwrite: &'static str,
    advanced: &'static str,
    layout_resample: &'static str,
    layout_raw: &'static str,
    apply_to_all: &'static str,
    signals: &'static str,
    search: &'static str,
    select_all: &'static str,
    select_none: &'static str,
    selected_count: fn(usize, usize) -> String,
    convert: &'static str,
    state_ready: &'static str,
    state_running: &'static str,
    state_done: &'static str,
    state_error: &'static str,
    open_folder: &'static str,
    open_file: &'static str,
    copy_path: &'static str,
    log: &'static str,
    copy_log: &'static str,
    settings: &'static str,
    about: &'static str,
    theme_system: &'static str,
    theme_dark: &'static str,
    theme_light: &'static str,
    defaults_note: &'static str,
    progress_of: fn(usize, usize, &str) -> String,
    frames: &'static str,
    current_message: &'static str,
    elapsed: &'static str,
    remaining: &'static str,
    done_summary: fn(&Summary) -> String,
    log_loading: fn(&str) -> String,
    log_done: fn(&str, &Summary) -> String,
    log_error: fn(&str, &str) -> String,
    log_queue_done: &'static str,
    drop_hint: &'static str,
}

const JA: Labels = Labels {
    queue: "キュー",
    add_files: "＋ ファイルを追加",
    clear_queue: "全て削除",
    retry_failed: "失敗を再試行",
    move_up: "上へ",
    move_down: "下へ",
    delete: "削除",
    empty_queue_hint: "BLF ファイルをここへ\nドラッグ＆ドロップ",
    no_job_selected: "左のキューからジョブを選択してください",
    blf_file: "BLF",
    dbc_file: "DBC",
    dbc_auto: "(自動検出)",
    dbc_missing: "DBC ファイルを選択してください",
    out_dir: "出力先",
    browse: "参照...",
    format: "出力形式",
    options: "オプション",
    relative_ts: "相対時刻 (先頭からの経過秒)",
    keep_can_id: "CanId 列を出力 (生データ形式のみ)",
    only_selected: "選択した信号のみ出力",
    skip_unknown: "DBC にない CAN ID をスキップ",
    overwrite: "既存ファイルを上書き",
    advanced: "詳細設定",
    layout_resample: "等間隔サンプリング",
    layout_raw: "フレーム単位 (生データ)",
    apply_to_all: "この設定を全ジョブに適用",
    signals: "信号選択",
    search: "検索",
    select_all: "全選択",
    select_none: "全解除",
    selected_count: |sel, total| format!("{sel} / {total} 信号を選択中"),
    convert: "変換",
    state_ready: "待機",
    state_running: "変換中",
    state_done: "完了",
    state_error: "エラー",
    open_folder: "出力フォルダを開く",
    open_file: "出力ファイルを開く",
    copy_path: "出力パスをコピー",
    log: "ログ",
    copy_log: "コピー",
    settings: "設定",
    about: "情報",
    theme_system: "システム",
    theme_dark: "ダーク",
    theme_light: "ライト",
    defaults_note: "新しく追加するジョブの既定値",
    progress_of: |i, n, file| format!("({i}/{n}) {file} を変換中..."),
    frames: "フレーム",
    current_message: "メッセージ",
    elapsed: "経過",
    remaining: "残り",
    done_summary: |s| {
        format!(
            "{} 行を書き出しました (デコード {} / {} フレーム、信号 {} 列)",
            s.rows_written, s.frames_decoded, s.frames_read, s.signal_columns
        )
    },
    log_loading: |f| format!("{f} を読み込み中..."),
    log_done: |f, s| format!("{f} → {} ({} 行)", s.output_path.display(), s.rows_written),
    log_error: |f, e| format!("{f} でエラー: {e}"),
    log_queue_done: "キューの処理が完了しました。",
    drop_hint: "BLF / DBC / フォルダをドロップで追加",
};

const EN: Labels = Labels {
    queue: "Queue",
    add_files: "+ Add Files",
    clear_queue: "Clear",
    retry_failed: "Retry Failed",
    move_up: "Move Up",
    move_down: "Move Down",
    delete: "Delete",
    empty_queue_hint: "Drag & drop BLF files here",
    no_job_selected: "Select a job from the queue",
    blf_file: "BLF",
    dbc_file: "DBC",
    dbc_auto: "(auto-detected)",
    dbc_missing: "Select a DBC file",
    out_dir: "Output",
    browse: "Browse...",
    format: "Output Format",
    options: "Options",
    relative_ts: "Relative timestamp (seconds from start)",
    keep_can_id: "Keep raw CAN ID column (raw layout only)",
    only_selected: "Export only selected signals",
    skip_unknown: "Skip unknown CAN IDs",
    overwrite: "Overwrite existing files",
    advanced: "Advanced",
    layout_resample: "Fixed-interval sampling",
    layout_raw: "Per frame (raw)",
    apply_to_all: "Apply settings to all jobs",
    signals: "Signal Selection",
    search: "Search",
    select_all: "All",
    select_none: "None",
    selected_count: |sel, total| format!("{sel} / {total} signals selected"),
    convert: "Convert",
    state_ready: "Ready",
    state_running: "Running",
    state_done: "Completed",
    state_error: "Error",
    open_folder: "Open output folder",
    open_file: "Open output file",
    copy_path: "Copy output path",
    log: "Log",
    copy_log: "Copy",
    settings: "Settings",
    about: "About",
    theme_system: "System",
    theme_dark: "Dark",
    theme_light: "Light",
    defaults_note: "Defaults for newly added jobs",
    progress_of: |i, n, file| format!("({i}/{n}) Converting {file}..."),
    frames: "frames",
    current_message: "Message",
    elapsed: "Elapsed",
    remaining: "Remaining",
    done_summary: |s| {
        format!(
            "{} rows written (decoded {} / {} frames, {} signal columns)",
            s.rows_written, s.frames_decoded, s.frames_read, s.signal_columns
        )
    },
    log_loading: |f| format!("Reading {f}..."),
    log_done: |f, s| {
        format!(
            "{f} -> {} ({} rows)",
            s.output_path.display(),
            s.rows_written
        )
    },
    log_error: |f, e| format!("Error in {f}: {e}"),
    log_queue_done: "Queue finished.",
    drop_hint: "Drop BLF / DBC files or folders to add",
};

// ---------------------------------------------------------------------------
// Jobs and the worker
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum JobState {
    Ready,
    Running,
    Done(Summary),
    Failed(String),
}

struct Job {
    id: u64,
    blf: PathBuf,
    dbc: Option<PathBuf>,
    dbc_auto: bool,
    dbc_candidates: Vec<PathBuf>,
    out_dir: PathBuf,
    format_parquet: bool,
    layout_raw: bool,
    interval_ms: f64,
    relative_timestamp: bool,
    keep_can_id: bool,
    skip_unknown: bool,
    overwrite: bool,
    only_selected: bool,
    selected_signals: HashSet<String>,
    state: JobState,
}

impl Job {
    fn file_name(&self) -> String {
        self.blf
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.blf.display().to_string())
    }

    fn options(&self) -> ConvertOptions {
        ConvertOptions {
            format: if self.format_parquet {
                OutputFormat::Parquet
            } else {
                OutputFormat::Csv
            },
            layout: if self.layout_raw {
                OutputLayout::PerFrame
            } else {
                OutputLayout::Resampled
            },
            resample_ms: self.interval_ms,
            timestamp: if self.relative_timestamp {
                TimestampMode::RelativeSeconds
            } else {
                TimestampMode::EpochSeconds
            },
            keep_can_id: self.keep_can_id,
            skip_unknown_ids: self.skip_unknown,
            overwrite: self.overwrite,
            signal_filter: if self.only_selected {
                Some(self.selected_signals.clone())
            } else {
                None
            },
        }
    }
}

enum WorkerEvent {
    Started {
        job_id: u64,
        index: usize,
        total: usize,
    },
    Progress {
        job_id: u64,
        progress: Progress,
        message: Option<String>,
    },
    Finished {
        job_id: u64,
        result: Result<Summary, String>,
    },
    AllDone,
}

struct RunSpec {
    job_id: u64,
    blf: PathBuf,
    dbc: PathBuf,
    out_dir: PathBuf,
    options: ConvertOptions,
}

struct RunState {
    rx: Receiver<WorkerEvent>,
    current_job: Option<u64>,
    queue_index: usize,
    queue_total: usize,
    progress: Progress,
    current_message: Option<String>,
    job_started: Instant,
}

fn spawn_worker(specs: Vec<RunSpec>, tx: Sender<WorkerEvent>) {
    std::thread::spawn(move || {
        let total = specs.len();
        for (index, spec) in specs.into_iter().enumerate() {
            let _ = tx.send(WorkerEvent::Started {
                job_id: spec.job_id,
                index,
                total,
            });
            let mut on_progress = |u: ProgressUpdate<'_>| {
                let _ = tx.send(WorkerEvent::Progress {
                    job_id: spec.job_id,
                    progress: u.progress,
                    message: u.current_message.map(|s| s.to_string()),
                });
            };
            let result = convert(
                &spec.blf,
                &spec.dbc,
                &spec.out_dir,
                spec.options,
                &mut on_progress,
            )
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(WorkerEvent::Finished {
                job_id: spec.job_id,
                result,
            });
        }
        let _ = tx.send(WorkerEvent::AllDone);
    });
}

/// Scan a directory for .dbc files (used for auto-detection next to a BLF).
fn detect_dbcs(blf: &Path) -> Vec<PathBuf> {
    let Some(dir) = blf.parent() else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| has_extension(p, "dbc"))
        .collect();
    found.sort();
    found
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Collect .blf files from a dropped folder (up to 3 levels deep).
fn collect_blfs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .collect();
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_blfs(&path, depth - 1, out);
        } else if has_extension(&path, "blf") {
            out.push(path);
        }
    }
}

fn open_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    let command = "explorer";
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let command = "xdg-open";
    let _ = std::process::Command::new(command).arg(path).spawn();
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct App {
    labels: &'static Labels,
    config: AppConfig,
    jobs: Vec<Job>,
    selected: Option<u64>,
    next_id: u64,
    run: Option<RunState>,
    log: Vec<String>,
    signal_cache: HashMap<PathBuf, Result<Vec<ColumnSpec>, String>>,
    signal_search: String,
    show_settings: bool,
    show_about: bool,
}

impl App {
    fn new(japanese: bool) -> Self {
        Self {
            labels: if japanese { &JA } else { &EN },
            config: AppConfig::load(),
            jobs: Vec::new(),
            selected: None,
            next_id: 1,
            run: None,
            log: Vec::new(),
            signal_cache: HashMap::new(),
            signal_search: String::new(),
            show_settings: false,
            show_about: false,
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.config.theme {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
        });
    }

    fn job(&self, id: u64) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn job_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    fn add_blf(&mut self, blf: PathBuf) {
        if self.jobs.iter().any(|j| j.blf == blf) {
            return;
        }
        let candidates = detect_dbcs(&blf);
        let auto = candidates.len() == 1;
        let dbc = if auto {
            Some(candidates[0].clone())
        } else {
            // fall back to the most recent DBC that still exists
            self.config.recent_dbcs.iter().find(|p| p.exists()).cloned()
        };
        let out_dir = self
            .config
            .recent_out_dirs
            .first()
            .filter(|p| p.exists())
            .cloned()
            .or_else(|| blf.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        self.config.last_blf_dir = blf.parent().map(Path::to_path_buf);
        let d = &self.config.defaults;
        let job = Job {
            id: self.next_id,
            blf,
            dbc,
            dbc_auto: auto,
            dbc_candidates: candidates,
            out_dir,
            format_parquet: d.format_parquet,
            layout_raw: d.layout_raw,
            interval_ms: d.interval_ms,
            relative_timestamp: d.relative_timestamp,
            keep_can_id: d.keep_can_id,
            skip_unknown: d.skip_unknown,
            overwrite: d.overwrite,
            only_selected: false,
            selected_signals: HashSet::new(),
            state: JobState::Ready,
        };
        self.selected = Some(job.id);
        self.next_id += 1;
        self.jobs.push(job);
        self.config.save();
    }

    fn assign_dbc(&mut self, dbc: PathBuf) {
        self.config.remember_dbc(&dbc);
        let target = self.selected;
        let mut assigned = false;
        for job in &mut self.jobs {
            let is_target = target == Some(job.id);
            if is_target || (target.is_none() && job.dbc.is_none()) {
                job.dbc = Some(dbc.clone());
                job.dbc_auto = false;
                assigned = true;
            }
        }
        if !assigned {
            for job in &mut self.jobs {
                if job.dbc.is_none() {
                    job.dbc = Some(dbc.clone());
                    job.dbc_auto = false;
                }
            }
        }
        self.config.save();
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        for path in dropped {
            if path.is_dir() {
                let mut blfs = Vec::new();
                collect_blfs(&path, 3, &mut blfs);
                blfs.sort();
                for blf in blfs {
                    self.add_blf(blf);
                }
            } else if has_extension(&path, "blf") {
                self.add_blf(path);
            } else if has_extension(&path, "dbc") {
                self.assign_dbc(path);
            }
        }
    }

    fn start_queue(&mut self) {
        let specs: Vec<RunSpec> = self
            .jobs
            .iter()
            .filter(|j| matches!(j.state, JobState::Ready | JobState::Failed(_)))
            .filter_map(|j| {
                let dbc = j.dbc.clone()?;
                Some(RunSpec {
                    job_id: j.id,
                    blf: j.blf.clone(),
                    dbc,
                    out_dir: j.out_dir.clone(),
                    options: j.options(),
                })
            })
            .collect();
        if specs.is_empty() {
            return;
        }
        for job in &mut self.jobs {
            if specs.iter().any(|s| s.job_id == job.id) {
                job.state = JobState::Running;
            }
        }
        let total = specs.len();
        let (tx, rx) = channel();
        spawn_worker(specs, tx);
        self.run = Some(RunState {
            rx,
            current_job: None,
            queue_index: 0,
            queue_total: total,
            progress: Progress::default(),
            current_message: None,
            job_started: Instant::now(),
        });
    }

    fn poll_worker(&mut self) {
        let labels = self.labels;
        let Some(run) = &mut self.run else { return };
        let mut finished_events: Vec<(u64, Result<Summary, String>)> = Vec::new();
        let mut all_done = false;
        while let Ok(event) = run.rx.try_recv() {
            match event {
                WorkerEvent::Started {
                    job_id,
                    index,
                    total,
                } => {
                    run.current_job = Some(job_id);
                    run.queue_index = index;
                    run.queue_total = total;
                    run.progress = Progress::default();
                    run.current_message = None;
                    run.job_started = Instant::now();
                    if let Some(job) = self.jobs.iter().find(|j| j.id == job_id) {
                        self.log.push((labels.log_loading)(&job.file_name()));
                    }
                }
                WorkerEvent::Progress {
                    job_id,
                    progress,
                    message,
                } => {
                    if run.current_job == Some(job_id) {
                        run.progress = progress;
                        if message.is_some() {
                            run.current_message = message;
                        }
                    }
                }
                WorkerEvent::Finished { job_id, result } => {
                    finished_events.push((job_id, result));
                }
                WorkerEvent::AllDone => all_done = true,
            }
        }
        for (job_id, result) in finished_events {
            let name = self.job(job_id).map(|j| j.file_name()).unwrap_or_default();
            match &result {
                Ok(summary) => self.log.push((labels.log_done)(&name, summary)),
                Err(error) => self.log.push((labels.log_error)(&name, error)),
            }
            if let Some(job) = self.job_mut(job_id) {
                job.state = match result {
                    Ok(summary) => JobState::Done(summary),
                    Err(error) => JobState::Failed(error),
                };
            }
        }
        if all_done {
            self.log.push(labels.log_queue_done.to_string());
            self.run = None;
        }
    }

    // -- panels -------------------------------------------------------------

    fn header_ui(&mut self, ui: &mut egui::Ui) {
        let labels = self.labels;
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(egui::RichText::new("BLF Decoder").strong().size(17.0));
            ui.label(egui::RichText::new(format!("v{APP_VERSION}")).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(labels.about).clicked() {
                    self.show_about = !self.show_about;
                }
                if ui.button(labels.settings).clicked() {
                    self.show_settings = !self.show_settings;
                }
                let mut theme = self.config.theme;
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(match theme {
                        ThemeChoice::System => labels.theme_system,
                        ThemeChoice::Dark => labels.theme_dark,
                        ThemeChoice::Light => labels.theme_light,
                    })
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut theme, ThemeChoice::System, labels.theme_system);
                        ui.selectable_value(&mut theme, ThemeChoice::Dark, labels.theme_dark);
                        ui.selectable_value(&mut theme, ThemeChoice::Light, labels.theme_light);
                    });
                if theme != self.config.theme {
                    self.config.theme = theme;
                    self.apply_theme(ui.ctx());
                    self.config.save();
                }
                ui.label(egui::RichText::new(labels.drop_hint).weak().small());
            });
        });
    }

    fn queue_ui(&mut self, ui: &mut egui::Ui) {
        let labels = self.labels;
        let running = self.run.is_some();
        ui.label(egui::RichText::new(labels.queue).strong());
        if ui
            .add_enabled(!running, egui::Button::new(labels.add_files))
            .clicked()
        {
            let mut dialog = rfd::FileDialog::new().add_filter("BLF", &["blf"]);
            if let Some(dir) = &self.config.last_blf_dir {
                dialog = dialog.set_directory(dir);
            }
            if let Some(files) = dialog.pick_files() {
                for file in files {
                    self.add_blf(file);
                }
            }
        }
        ui.separator();

        if self.jobs.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(labels.empty_queue_hint).weak());
            });
            return;
        }

        let mut delete_job: Option<u64> = None;
        let mut move_job: Option<(u64, isize)> = None;
        let mut retry_job: Option<u64> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .max_height(ui.available_height() - 40.0)
            .show(ui, |ui| {
                for job in &self.jobs {
                    let (dot, state_text) = match &job.state {
                        JobState::Ready => (egui::Color32::GRAY, labels.state_ready),
                        JobState::Running => (
                            egui::Color32::from_rgb(0xff, 0xb0, 0x2e),
                            labels.state_running,
                        ),
                        JobState::Done(_) => {
                            (egui::Color32::from_rgb(0x3d, 0xb0, 0x62), labels.state_done)
                        }
                        JobState::Failed(_) => (
                            egui::Color32::from_rgb(0xd6, 0x45, 0x45),
                            labels.state_error,
                        ),
                    };
                    let selected = self.selected == Some(job.id);
                    let text = egui::RichText::new(format!("● {}", job.file_name())).color(dot);
                    let response = ui.selectable_label(selected, text).on_hover_text(format!(
                        "{}\n{}",
                        job.blf.display(),
                        state_text
                    ));
                    if response.clicked() {
                        self.selected = Some(job.id);
                    }
                    response.context_menu(|ui| {
                        if let JobState::Done(summary) = &job.state {
                            if ui.button(labels.open_folder).clicked() {
                                if let Some(dir) = summary.output_path.parent() {
                                    open_in_file_manager(dir);
                                }
                                ui.close();
                            }
                            if ui.button(labels.open_file).clicked() {
                                open_in_file_manager(&summary.output_path);
                                ui.close();
                            }
                            if ui.button(labels.copy_path).clicked() {
                                ui.ctx()
                                    .copy_text(summary.output_path.display().to_string());
                                ui.close();
                            }
                            ui.separator();
                        }
                        if matches!(job.state, JobState::Failed(_))
                            && ui.button(labels.retry_failed).clicked()
                        {
                            retry_job = Some(job.id);
                            ui.close();
                        }
                        if ui
                            .add_enabled(!running, egui::Button::new(labels.move_up))
                            .clicked()
                        {
                            move_job = Some((job.id, -1));
                            ui.close();
                        }
                        if ui
                            .add_enabled(!running, egui::Button::new(labels.move_down))
                            .clicked()
                        {
                            move_job = Some((job.id, 1));
                            ui.close();
                        }
                        if ui
                            .add_enabled(!running, egui::Button::new(labels.delete))
                            .clicked()
                        {
                            delete_job = Some(job.id);
                            ui.close();
                        }
                    });
                }
            });

        ui.separator();
        ui.horizontal(|ui| {
            let has_failed = self
                .jobs
                .iter()
                .any(|j| matches!(j.state, JobState::Failed(_)));
            if ui
                .add_enabled(
                    has_failed && !running,
                    egui::Button::new(labels.retry_failed).small(),
                )
                .clicked()
            {
                for job in &mut self.jobs {
                    if matches!(job.state, JobState::Failed(_)) {
                        job.state = JobState::Ready;
                    }
                }
            }
            if ui
                .add_enabled(!running, egui::Button::new(labels.clear_queue).small())
                .clicked()
            {
                self.jobs.clear();
                self.selected = None;
            }
        });

        if let Some(id) = retry_job
            && let Some(job) = self.job_mut(id)
        {
            job.state = JobState::Ready;
        }
        if let Some(id) = delete_job {
            self.jobs.retain(|j| j.id != id);
            if self.selected == Some(id) {
                self.selected = self.jobs.first().map(|j| j.id);
            }
        }
        if let Some((id, delta)) = move_job
            && let Some(index) = self.jobs.iter().position(|j| j.id == id)
        {
            let new_index = index as isize + delta;
            if new_index >= 0 && (new_index as usize) < self.jobs.len() {
                self.jobs.swap(index, new_index as usize);
            }
        }
    }

    fn job_editor_ui(&mut self, ui: &mut egui::Ui) {
        let labels = self.labels;
        let running = self.run.is_some();
        let Some(id) = self.selected else {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new(labels.no_job_selected).weak());
            });
            return;
        };

        // Job state banner (done / error) first.
        let state = self.job(id).map(|j| j.state.clone());
        match state {
            Some(JobState::Done(summary)) => {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x3d, 0xb0, 0x62),
                        format!("✔ {}", labels.state_done),
                    );
                    ui.label((labels.done_summary)(&summary));
                });
                ui.horizontal(|ui| {
                    if ui.button(labels.open_folder).clicked()
                        && let Some(dir) = summary.output_path.parent()
                    {
                        open_in_file_manager(dir);
                    }
                    if ui.button(labels.open_file).clicked() {
                        open_in_file_manager(&summary.output_path);
                    }
                    if ui.button(labels.copy_path).clicked() {
                        ui.ctx()
                            .copy_text(summary.output_path.display().to_string());
                    }
                });
                ui.separator();
            }
            Some(JobState::Failed(error)) => {
                ui.colored_label(
                    egui::Color32::from_rgb(0xd6, 0x45, 0x45),
                    format!("{}: {error}", labels.state_error),
                );
                ui.separator();
            }
            _ => {}
        }

        let mut dbc_to_remember: Option<PathBuf> = None;
        let mut out_to_remember: Option<PathBuf> = None;
        let mut apply_to_all = false;

        {
            let recent_dbcs = self.config.recent_dbcs.clone();
            let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) else {
                return;
            };
            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("job_paths")
                    .num_columns(3)
                    .spacing([10.0, 8.0])
                    .min_col_width(56.0)
                    .show(ui, |ui| {
                        // BLF row
                        ui.label(labels.blf_file);
                        ui.add(
                            egui::Label::new(egui::RichText::new(job.file_name()).strong())
                                .truncate(),
                        )
                        .on_hover_text(job.blf.display().to_string());
                        ui.label("");
                        ui.end_row();

                        // DBC row: candidates + recents in a combo, plus browse
                        ui.label(labels.dbc_file);
                        ui.horizontal(|ui| {
                            let mut choices: Vec<PathBuf> = job.dbc_candidates.clone();
                            for recent in &recent_dbcs {
                                if !choices.contains(recent) && recent.exists() {
                                    choices.push(recent.clone());
                                }
                            }
                            let current_text = job
                                .dbc
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| labels.dbc_missing.to_string());
                            egui::ComboBox::from_id_salt("dbc_combo")
                                .selected_text(current_text)
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    for choice in &choices {
                                        let name = choice
                                            .file_name()
                                            .map(|n| n.to_string_lossy().into_owned())
                                            .unwrap_or_default();
                                        let is_current = job.dbc.as_ref() == Some(choice);
                                        if ui
                                            .selectable_label(is_current, name)
                                            .on_hover_text(choice.display().to_string())
                                            .clicked()
                                        {
                                            job.dbc = Some(choice.clone());
                                            job.dbc_auto = false;
                                            dbc_to_remember = Some(choice.clone());
                                        }
                                    }
                                });
                            if ui.button(labels.browse).clicked() {
                                let mut dialog = rfd::FileDialog::new().add_filter("DBC", &["dbc"]);
                                if let Some(dir) = job
                                    .dbc
                                    .as_ref()
                                    .and_then(|p| p.parent())
                                    .or(job.blf.parent())
                                {
                                    dialog = dialog.set_directory(dir);
                                }
                                if let Some(file) = dialog.pick_file() {
                                    job.dbc = Some(file.clone());
                                    job.dbc_auto = false;
                                    dbc_to_remember = Some(file);
                                }
                            }
                            if job.dbc_auto {
                                ui.label(egui::RichText::new(labels.dbc_auto).weak().small());
                            }
                        });
                        ui.label("");
                        ui.end_row();

                        // Output dir row
                        ui.label(labels.out_dir);
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(job.out_dir.display().to_string()).truncate());
                            if ui.button(labels.browse).clicked()
                                && let Some(dir) = rfd::FileDialog::new()
                                    .set_directory(&job.out_dir)
                                    .pick_folder()
                            {
                                job.out_dir = dir.clone();
                                out_to_remember = Some(dir);
                            }
                        });
                        ui.label("");
                        ui.end_row();

                        // Format row
                        ui.label(labels.format);
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut job.format_parquet, false, "CSV");
                            ui.radio_value(&mut job.format_parquet, true, "Parquet");
                        });
                        ui.label("");
                        ui.end_row();
                    });

                ui.add_space(4.0);
                ui.label(egui::RichText::new(labels.options).strong());
                ui.checkbox(&mut job.relative_timestamp, labels.relative_ts);
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        job.layout_raw,
                        egui::Checkbox::new(&mut job.keep_can_id, labels.keep_can_id),
                    );
                });
                ui.checkbox(&mut job.skip_unknown, labels.skip_unknown);
                ui.checkbox(&mut job.overwrite, labels.overwrite);
                ui.checkbox(&mut job.only_selected, labels.only_selected);

                egui::CollapsingHeader::new(labels.advanced)
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut job.layout_raw, false, labels.layout_resample);
                            ui.add_enabled(
                                !job.layout_raw,
                                egui::DragValue::new(&mut job.interval_ms)
                                    .range(1.0..=60_000.0)
                                    .speed(10)
                                    .suffix(" ms"),
                            );
                        });
                        ui.radio_value(&mut job.layout_raw, true, labels.layout_raw);
                        ui.add_space(4.0);
                        if ui.button(labels.apply_to_all).clicked() {
                            apply_to_all = true;
                        }
                    });
            });
        }

        if let Some(dbc) = dbc_to_remember {
            self.config.remember_dbc(&dbc);
            self.config.save();
        }
        if let Some(dir) = out_to_remember {
            self.config.remember_out_dir(&dir);
            self.config.save();
        }
        if apply_to_all {
            self.apply_settings_to_all(id);
        }

        // Signal selection
        let (only_selected, dbc_path) = self
            .job(id)
            .map(|j| (j.only_selected, j.dbc.clone()))
            .unwrap_or((false, None));
        if only_selected && let Some(dbc_path) = dbc_path {
            self.signal_selection_ui(ui, id, &dbc_path, running);
        }
    }

    fn apply_settings_to_all(&mut self, source_id: u64) {
        let Some(source) = self.job(source_id) else {
            return;
        };
        let (fp, lr, im, rt, kc, su, ow, os, sel, dbc) = (
            source.format_parquet,
            source.layout_raw,
            source.interval_ms,
            source.relative_timestamp,
            source.keep_can_id,
            source.skip_unknown,
            source.overwrite,
            source.only_selected,
            source.selected_signals.clone(),
            source.dbc.clone(),
        );
        let out_dir = source.out_dir.clone();
        for job in &mut self.jobs {
            if job.id == source_id {
                continue;
            }
            job.format_parquet = fp;
            job.layout_raw = lr;
            job.interval_ms = im;
            job.relative_timestamp = rt;
            job.keep_can_id = kc;
            job.skip_unknown = su;
            job.overwrite = ow;
            job.only_selected = os;
            job.selected_signals = sel.clone();
            job.out_dir = out_dir.clone();
            if job.dbc.is_none() {
                job.dbc = dbc.clone();
                job.dbc_auto = false;
            }
        }
    }

    fn signal_selection_ui(
        &mut self,
        ui: &mut egui::Ui,
        job_id: u64,
        dbc_path: &Path,
        running: bool,
    ) {
        let labels = self.labels;
        let specs = self
            .signal_cache
            .entry(dbc_path.to_path_buf())
            .or_insert_with(|| {
                Dbc::from_file(dbc_path)
                    .map(|dbc| list_columns(&dbc))
                    .map_err(|e| format!("{e:#}"))
            })
            .clone();

        ui.add_space(4.0);
        ui.label(egui::RichText::new(labels.signals).strong());
        match specs {
            Err(error) => {
                ui.colored_label(egui::Color32::from_rgb(0xd6, 0x45, 0x45), error);
            }
            Ok(specs) => {
                let Some(job) = self.jobs.iter_mut().find(|j| j.id == job_id) else {
                    return;
                };
                // First time: everything selected.
                if job.selected_signals.is_empty() {
                    job.selected_signals = specs.iter().map(|s| s.name.clone()).collect();
                }
                ui.add_enabled_ui(!running, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(labels.search);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.signal_search)
                                .desired_width(200.0),
                        );
                        if ui.small_button(labels.select_all).clicked() {
                            job.selected_signals = specs.iter().map(|s| s.name.clone()).collect();
                        }
                        if ui.small_button(labels.select_none).clicked() {
                            job.selected_signals.clear();
                        }
                        ui.label(
                            egui::RichText::new((labels.selected_count)(
                                job.selected_signals.len(),
                                specs.len(),
                            ))
                            .weak(),
                        );
                    });
                    let needle = self.signal_search.to_lowercase();
                    egui::ScrollArea::vertical()
                        .id_salt("signal_list")
                        .max_height(200.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for spec in &specs {
                                if !needle.is_empty()
                                    && !spec.name.to_lowercase().contains(&needle)
                                    && !spec.message.to_lowercase().contains(&needle)
                                {
                                    continue;
                                }
                                let mut checked = job.selected_signals.contains(&spec.name);
                                let label = format!("{}    ({})", spec.name, spec.message);
                                if ui.checkbox(&mut checked, label).changed() {
                                    if checked {
                                        job.selected_signals.insert(spec.name.clone());
                                    } else {
                                        job.selected_signals.remove(&spec.name);
                                    }
                                }
                            }
                        });
                });
            }
        }
    }

    fn footer_ui(&mut self, ui: &mut egui::Ui) {
        let labels = self.labels;
        let running = self.run.is_some();

        ui.add_space(4.0);
        // Convert button, centred and prominent.
        let can_convert = !running
            && self.jobs.iter().any(|j| {
                matches!(j.state, JobState::Ready | JobState::Failed(_)) && j.dbc.is_some()
            });
        ui.vertical_centered_justified(|ui| {
            if ui
                .add_enabled(
                    can_convert,
                    egui::Button::new(egui::RichText::new(labels.convert).size(16.0))
                        .min_size([0.0, 34.0].into()),
                )
                .clicked()
            {
                self.start_queue();
            }
        });

        // Progress block.
        if let Some(run) = &self.run {
            let file = run
                .current_job
                .and_then(|id| self.job(id))
                .map(|j| j.file_name())
                .unwrap_or_default();
            ui.label((labels.progress_of)(
                run.queue_index + 1,
                run.queue_total,
                &file,
            ));
            ui.add(
                egui::ProgressBar::new(run.progress.fraction())
                    .show_percentage()
                    .animate(true),
            );
            ui.horizontal(|ui| {
                ui.label(format!("{}: {}", labels.frames, run.progress.frames_read));
                if let Some(message) = &run.current_message {
                    ui.separator();
                    ui.label(format!("{}: {}", labels.current_message, message));
                }
                let elapsed = run.job_started.elapsed().as_secs_f32();
                ui.separator();
                ui.label(format!("{}: {}", labels.elapsed, fmt_duration(elapsed)));
                let fraction = run.progress.fraction();
                if fraction > 0.01 {
                    let remaining = elapsed / fraction - elapsed;
                    ui.separator();
                    ui.label(format!("{}: {}", labels.remaining, fmt_duration(remaining)));
                }
            });
        }

        // Collapsible log.
        egui::CollapsingHeader::new(labels.log)
            .default_open(false)
            .show(ui, |ui| {
                if ui.small_button(labels.copy_log).clicked() {
                    ui.ctx().copy_text(self.log.join("\n"));
                }
                egui::ScrollArea::vertical()
                    .id_salt("log_scroll")
                    .max_height(120.0)
                    .stick_to_bottom(true)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.add(
                                egui::Label::new(egui::RichText::new(line).monospace().small())
                                    .truncate(),
                            )
                            .on_hover_text(line);
                        }
                    });
            });
        ui.add_space(2.0);
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let labels = self.labels;
        let mut open = self.show_settings;
        let mut changed = false;
        egui::Window::new(labels.settings)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(labels.defaults_note).weak());
                ui.separator();
                let d = &mut self.config.defaults;
                ui.horizontal(|ui| {
                    ui.label(labels.format);
                    changed |= ui
                        .radio_value(&mut d.format_parquet, false, "CSV")
                        .changed();
                    changed |= ui
                        .radio_value(&mut d.format_parquet, true, "Parquet")
                        .changed();
                });
                ui.horizontal(|ui| {
                    changed |= ui
                        .radio_value(&mut d.layout_raw, false, labels.layout_resample)
                        .changed();
                    changed |= ui
                        .add_enabled(
                            !d.layout_raw,
                            egui::DragValue::new(&mut d.interval_ms)
                                .range(1.0..=60_000.0)
                                .speed(10)
                                .suffix(" ms"),
                        )
                        .changed();
                });
                changed |= ui
                    .radio_value(&mut d.layout_raw, true, labels.layout_raw)
                    .changed();
                changed |= ui
                    .checkbox(&mut d.relative_timestamp, labels.relative_ts)
                    .changed();
                changed |= ui
                    .checkbox(&mut d.keep_can_id, labels.keep_can_id)
                    .changed();
                changed |= ui
                    .checkbox(&mut d.skip_unknown, labels.skip_unknown)
                    .changed();
                changed |= ui.checkbox(&mut d.overwrite, labels.overwrite).changed();
            });
        if changed {
            self.config.save();
        }
        self.show_settings = open;
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        let labels = self.labels;
        let mut open = self.show_about;
        egui::Window::new(labels.about)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("BLF Decoder").strong().size(16.0));
                ui.label(format!("Version {APP_VERSION}"));
                ui.label("BLF + DBC → CSV / Parquet");
                ui.hyperlink(REPO_URL);
            });
        self.show_about = open;
    }
}

fn fmt_duration(seconds: f32) -> String {
    let total = seconds.max(0.0) as u64;
    if total >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            total / 3600,
            (total % 3600) / 60,
            total % 60
        )
    } else {
        format!("{}:{:02}", total / 60, total % 60)
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_worker();
        let ctx = ui.ctx().clone();
        self.handle_drops(&ctx);
        if self.run.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(4.0);
            self.header_ui(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("footer").show(ui, |ui| {
            self.footer_ui(ui);
        });
        egui::Panel::left("queue")
            .resizable(true)
            .default_size(230.0)
            .size_range(170.0..=400.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                self.queue_ui(ui);
            });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            self.job_editor_ui(ui);
        });

        if self.show_settings {
            self.settings_window(&ctx);
        }
        if self.show_about {
            self.about_window(&ctx);
        }
    }
}
