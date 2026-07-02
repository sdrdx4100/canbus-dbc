//! Desktop GUI (eframe/egui): pick a BLF, a DBC and an output folder, choose
//! the format, convert with live progress. The conversion runs on a worker
//! thread and reports back over a channel so the UI stays responsive.

use blf_decoder::convert::{Progress, Summary, convert};
use blf_decoder::export::OutputFormat;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 340.0])
            .with_min_inner_size([480.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "BLF Decoder",
        options,
        Box::new(|cc| {
            let japanese = install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(App::new(japanese)))
        }),
    )
}

/// Try to load a system font with Japanese glyph coverage. egui's built-in
/// fonts have no CJK glyphs, so without this the UI falls back to English.
fn install_cjk_font(ctx: &egui::Context) -> bool {
    const CANDIDATES: &[&str] = &[
        // Windows (primary target)
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\YuGothM.ttc",
        "C:\\Windows\\Fonts\\msgothic.ttc",
        // macOS
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        // Linux
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

/// UI strings, Japanese when a CJK-capable font is available.
struct Labels {
    blf_file: &'static str,
    dbc_file: &'static str,
    out_dir: &'static str,
    format: &'static str,
    browse: &'static str,
    convert: &'static str,
    converting: &'static str,
    done: fn(&Summary) -> String,
    error_prefix: &'static str,
    not_selected: &'static str,
}

const JA: Labels = Labels {
    blf_file: "BLFファイル",
    dbc_file: "DBCファイル",
    out_dir: "出力フォルダ",
    format: "出力形式",
    browse: "選択...",
    convert: "変換",
    converting: "変換中...",
    done: |s| {
        format!(
            "完了: {}\nデコード {} / {} フレーム, 信号列 {}",
            s.output_path.display(),
            s.frames_decoded,
            s.frames_read,
            s.signal_columns
        )
    },
    error_prefix: "エラー: ",
    not_selected: "(未選択)",
};

const EN: Labels = Labels {
    blf_file: "BLF file",
    dbc_file: "DBC file",
    out_dir: "Output folder",
    format: "Output format",
    browse: "Browse...",
    convert: "Convert",
    converting: "Converting...",
    done: |s| {
        format!(
            "Done: {}\nDecoded {} of {} frames, {} signal columns",
            s.output_path.display(),
            s.frames_decoded,
            s.frames_read,
            s.signal_columns
        )
    },
    error_prefix: "Error: ",
    not_selected: "(not selected)",
};

enum WorkerMsg {
    Progress(Progress),
    Finished(Result<Summary, String>),
}

enum State {
    Idle,
    Running {
        progress: Progress,
        rx: Receiver<WorkerMsg>,
    },
    Done(Summary),
    Failed(String),
}

struct App {
    labels: &'static Labels,
    blf_path: Option<PathBuf>,
    dbc_path: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    format: OutputFormat,
    state: State,
}

impl App {
    fn new(japanese: bool) -> Self {
        Self {
            labels: if japanese { &JA } else { &EN },
            blf_path: None,
            dbc_path: None,
            out_dir: None,
            format: OutputFormat::Csv,
            state: State::Idle,
        }
    }

    fn start_conversion(&mut self) {
        let (Some(blf), Some(dbc), Some(out)) = (
            self.blf_path.clone(),
            self.dbc_path.clone(),
            self.out_dir.clone(),
        ) else {
            return;
        };
        let format = self.format;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut on_progress = |p: Progress| {
                let _ = tx.send(WorkerMsg::Progress(p));
            };
            let result =
                convert(&blf, &dbc, &out, format, &mut on_progress).map_err(|e| format!("{e:#}"));
            let _ = tx.send(WorkerMsg::Finished(result));
        });
        self.state = State::Running {
            progress: Progress::default(),
            rx,
        };
    }

    fn poll_worker(&mut self) {
        if let State::Running { progress, rx } = &mut self.state {
            let mut finished = None;
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerMsg::Progress(p) => *progress = p,
                    WorkerMsg::Finished(result) => finished = Some(result),
                }
            }
            if let Some(result) = finished {
                self.state = match result {
                    Ok(summary) => State::Done(summary),
                    Err(message) => State::Failed(message),
                };
            }
        }
    }

    fn path_row(
        ui: &mut egui::Ui,
        label: &str,
        browse: &str,
        not_selected: &str,
        path: &mut Option<PathBuf>,
        enabled: bool,
        pick: impl Fn() -> Option<PathBuf>,
    ) {
        ui.label(label);
        let text = path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| not_selected.to_string());
        ui.add(egui::Label::new(egui::RichText::new(text).monospace()).truncate());
        if ui.add_enabled(enabled, egui::Button::new(browse)).clicked()
            && let Some(picked) = pick()
        {
            *path = Some(picked);
        }
        ui.end_row();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_worker();
        let labels = self.labels;
        let running = matches!(self.state, State::Running { .. });
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;

            egui::Grid::new("inputs")
                .num_columns(3)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    Self::path_row(
                        ui,
                        labels.blf_file,
                        labels.browse,
                        labels.not_selected,
                        &mut self.blf_path,
                        !running,
                        || {
                            rfd::FileDialog::new()
                                .add_filter("BLF", &["blf"])
                                .pick_file()
                        },
                    );
                    Self::path_row(
                        ui,
                        labels.dbc_file,
                        labels.browse,
                        labels.not_selected,
                        &mut self.dbc_path,
                        !running,
                        || {
                            rfd::FileDialog::new()
                                .add_filter("DBC", &["dbc"])
                                .pick_file()
                        },
                    );
                    Self::path_row(
                        ui,
                        labels.out_dir,
                        labels.browse,
                        labels.not_selected,
                        &mut self.out_dir,
                        !running,
                        || rfd::FileDialog::new().pick_folder(),
                    );
                });

            ui.horizontal(|ui| {
                ui.label(labels.format);
                ui.add_enabled_ui(!running, |ui| {
                    ui.radio_value(&mut self.format, OutputFormat::Csv, "CSV");
                    ui.radio_value(&mut self.format, OutputFormat::Parquet, "Parquet");
                });
            });

            ui.separator();

            let ready = self.blf_path.is_some()
                && self.dbc_path.is_some()
                && self.out_dir.is_some()
                && !running;
            if ui
                .add_enabled(
                    ready,
                    egui::Button::new(labels.convert).min_size([120.0, 32.0].into()),
                )
                .clicked()
            {
                self.start_conversion();
            }

            match &self.state {
                State::Idle => {}
                State::Running { progress, .. } => {
                    ui.add(
                        egui::ProgressBar::new(progress.fraction())
                            .show_percentage()
                            .animate(true),
                    );
                    ui.label(format!(
                        "{} {} frames",
                        labels.converting, progress.frames_read
                    ));
                }
                State::Done(summary) => {
                    ui.colored_label(egui::Color32::DARK_GREEN, (labels.done)(summary));
                }
                State::Failed(message) => {
                    ui.colored_label(
                        egui::Color32::RED,
                        format!("{}{}", labels.error_prefix, message),
                    );
                }
            }
        });
    }
}
