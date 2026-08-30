//! Platform-neutral GUI tool state machine and schema-to-control mapping.

use crate::execution::Artifact;
#[cfg(feature = "gui")]
use crate::execution::OutputKind;
#[cfg(feature = "gui")]
use crate::execution::ParameterSpec;
#[cfg(feature = "gui")]
use crate::provenance::Notebook;
use std::collections::BTreeMap;
#[cfg(feature = "gui")]
use std::fs;
#[cfg(feature = "gui")]
use std::path::{Path, PathBuf};
#[cfg(feature = "gui")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "gui")]
use std::sync::mpsc::{self, Receiver};
#[cfg(feature = "gui")]
use std::sync::Arc;

/// GUI 结果页写出的凭证：载荷来自包内源码与字节码，因此摘要在本机即可复核，
/// 权限先锚定到包目录再转成可移植形式，避免凭证带上本机绝对路径。
pub fn gui_evidence(
    source: &str,
    bytecode: &crate::bytecode::BytecodeProgram,
    permissions: &crate::security::PermissionSet,
    base: &std::path::Path,
    parameters: &BTreeMap<String, serde_json::Value>,
    output: &[String],
    artifacts: &[Artifact],
) -> crate::evidence::EvidenceManifest {
    gui_evidence_with_run(
        source,
        bytecode,
        permissions,
        base,
        parameters,
        output,
        artifacts,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        crate::execution::ResourceSummary::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn gui_evidence_with_run(
    source: &str,
    bytecode: &crate::bytecode::BytecodeProgram,
    permissions: &crate::security::PermissionSet,
    base: &std::path::Path,
    parameters: &BTreeMap<String, serde_json::Value>,
    output: &[String],
    artifacts: &[Artifact],
    inputs: Vec<crate::execution::InputObservation>,
    diagnostics: Vec<crate::api::DiagnosticReport>,
    mut external_dependencies: Vec<crate::evidence::ExternalDependency>,
    resources: crate::execution::ResourceSummary,
) -> crate::evidence::EvidenceManifest {
    use crate::execution::hex_digest;
    let program_digest = hex_digest(source.as_bytes());
    if external_dependencies.is_empty() {
        external_dependencies.push(crate::evidence::ExternalDependency {
            kind: "无".into(),
            identity: "本机执行".into(),
            fingerprint: None,
            level: crate::evidence::ExternalDependencyLevel::None,
        });
    }
    crate::evidence::EvidenceManifest {
        version: crate::execution::ContractVersion::default(),
        evidence_version: crate::evidence::EVIDENCE_VERSION,
        run_id: format!("gui-{}", &program_digest[..12]),
        program_digest,
        source_path: None,
        source_text: Some(source.to_string()),
        output: output.to_vec(),
        success: true,
        permissions: Some(permissions.clone().into_portable_roots(base)),
        bytecode_digest: postcard::to_allocvec(bytecode)
            .ok()
            .map(|bytes| hex_digest(&bytes)),
        parameters: parameters.clone(),
        inputs,
        artifacts: artifacts.to_vec(),
        diagnostics,
        external_dependencies,
        resources,
        metadata: crate::execution::Metadata::default(),
    }
}

#[cfg(feature = "gui")]
pub fn run_gui() -> eframe::Result {
    crate::privacy::install_secret_scrubbing_panic_hook();
    let (width, height) = crate::pack::read_embedded()
        .ok()
        .flatten()
        .and_then(|embedded| embedded.descriptor.tool)
        .and_then(|tool| tool.gui)
        .map(|gui| (gui.width.max(520) as f32, gui.height.max(420) as f32))
        .unwrap_or((860.0, 640.0));
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([width, height])
            .with_min_inner_size([520.0, 420.0]),
        run_and_return: false,
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    {
        // winit 0.30 otherwise probes the current bundle during launch. On macOS 26
        // that CoreFoundation path can dereference a null bundle string, especially
        // for an ad-hoc signed executable assembled into a user-created .app bundle.
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_activation_policy(ActivationPolicy::Regular);
        }));
    }
    let title = crate::pack::read_embedded()
        .ok()
        .flatten()
        .and_then(|embedded| embedded.descriptor.tool)
        .and_then(|tool| tool.gui)
        .map(|gui| gui.title)
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "衡枢".into());
    eframe::run_native(
        &title,
        options,
        Box::new(|context| {
            apply_visual_style(&context.egui_ctx);
            install_system_cjk_font(&context.egui_ctx);
            Ok(Box::new(GuiApp::new()))
        }),
    )
}

/// 字号、间距和圆角写进两套主题：`set_style` 只改当前主题，系统切换深浅色后
/// 未写入的那套会退回 egui 默认值。
#[cfg(feature = "gui")]
fn apply_visual_style(context: &eframe::egui::Context) {
    use eframe::egui::{FontId, TextStyle, Theme};
    context.all_styles_mut(|style| {
        style.spacing.item_spacing = eframe::egui::vec2(10.0, 8.0);
        style.spacing.button_padding = eframe::egui::vec2(16.0, 7.0);
        style.spacing.menu_margin = eframe::egui::Margin::same(8);
        style.spacing.indent = 18.0;
        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(15.0, eframe::egui::FontFamily::Proportional),
        );
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(15.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(13.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(19.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(14.0));
        for widget in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = eframe::egui::CornerRadius::same(8);
        }
        style.visuals.window_corner_radius = eframe::egui::CornerRadius::same(10);
    });
    for (theme, accent) in [(Theme::Light, ACCENT_LIGHT), (Theme::Dark, ACCENT_DARK)] {
        context.style_mut_of(theme, |style| {
            style.visuals.selection.bg_fill = accent.gamma_multiply(0.35);
            style.visuals.selection.stroke.color = accent;
            style.visuals.hyperlink_color = accent;
        });
    }
}

#[cfg(feature = "gui")]
const ACCENT_LIGHT: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(23, 105, 170);
#[cfg(feature = "gui")]
const ACCENT_DARK: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(102, 176, 235);

#[cfg(feature = "gui")]
fn system_cjk_font_path() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\simhei.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        ]
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

#[cfg(feature = "gui")]
fn install_system_cjk_font(context: &eframe::egui::Context) {
    let Some(path) = system_cjk_font_path() else {
        return;
    };
    let Ok(bytes) = fs::read(&path) else {
        return;
    };
    let mut definitions = eframe::egui::FontDefinitions::default();
    let name = "hengshu-system-cjk".to_owned();
    definitions.font_data.insert(
        name.clone(),
        std::sync::Arc::new(eframe::egui::FontData::from_owned(bytes)),
    );
    for family in [
        eframe::egui::FontFamily::Proportional,
        eframe::egui::FontFamily::Monospace,
    ] {
        // 字族度量取第一个字体，而每个字形按自己字体的 ascent 落位；把中文字体放到
        // 末尾会让汉字与拉丁字形分属两套度量而错开基线，因此中文界面必须前插。
        definitions
            .families
            .entry(family)
            .or_default()
            .insert(0, name.clone());
    }
    context.set_fonts(definitions);
}

#[cfg(feature = "gui")]
struct GuiApp {
    model: DesktopModel,
    descriptor: Option<crate::pack::PackageDescriptor>,
    values: BTreeMap<String, String>,
    output_root: String,
    source: String,
    show_source: bool,
    message: String,
    program_output: Vec<String>,
    diagnostics: Vec<String>,
    report_warning: Option<String>,
    job: Option<GuiJob>,
    artifacts: Vec<Artifact>,
    notebook: Option<Notebook>,
    last_parameters: BTreeMap<String, serde_json::Value>,
    last_inputs: Vec<crate::execution::InputObservation>,
    last_external_dependencies: Vec<crate::evidence::ExternalDependency>,
    last_diagnostic_reports: Vec<crate::api::DiagnosticReport>,
    last_resources: crate::execution::ResourceSummary,
}

#[cfg(feature = "gui")]
struct GuiJob {
    receiver: Receiver<GuiJobResult>,
    cancellation: Arc<AtomicBool>,
}

#[cfg(feature = "gui")]
enum GuiJobResult {
    Succeeded {
        output: Vec<String>,
        notebook: Option<Notebook>,
        artifacts: Vec<Artifact>,
        output_root: PathBuf,
        report_warning: Option<String>,
        inputs: Vec<crate::execution::InputObservation>,
        external_dependencies: Vec<crate::evidence::ExternalDependency>,
        diagnostic_reports: Vec<crate::api::DiagnosticReport>,
        resources: crate::execution::ResourceSummary,
    },
    Failed {
        message: String,
        diagnostics: Vec<String>,
    },
}

#[cfg(feature = "gui")]
impl GuiApp {
    fn new() -> Self {
        let embedded = crate::pack::read_embedded().ok().flatten();
        let source = embedded
            .as_ref()
            .map(|embedded| embedded.source.clone())
            .unwrap_or_default();
        let descriptor = embedded.map(|embedded| embedded.descriptor);
        let values = descriptor
            .as_ref()
            .and_then(|descriptor| descriptor.tool.as_ref())
            .map(|tool| {
                tool.parameters
                    .keys()
                    .map(|name| (name.clone(), String::new()))
                    .collect()
            })
            .unwrap_or_default();
        let output_root = descriptor
            .as_ref()
            .and_then(|descriptor| descriptor.tool.as_ref())
            .map(|tool| tool.output_root(OutputKind::Gui).display().to_string())
            .unwrap_or_else(|| "结果".into());
        Self {
            model: DesktopModel::default(),
            descriptor,
            values,
            output_root,
            source,
            show_source: false,
            message: String::new(),
            program_output: Vec::new(),
            diagnostics: Vec::new(),
            report_warning: None,
            job: None,
            artifacts: Vec::new(),
            notebook: None,
            last_parameters: BTreeMap::new(),
            last_inputs: Vec::new(),
            last_external_dependencies: Vec::new(),
            last_diagnostic_reports: Vec::new(),
            last_resources: crate::execution::ResourceSummary::default(),
        }
    }

    fn output_path(&self, base: &Path) -> Result<PathBuf, String> {
        let relative = Path::new(&self.output_root);
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err("输出目录必须是包目录内的相对路径".into());
        }
        Ok(base.join(relative))
    }

    fn run_embedded(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(embedded) = (match crate::pack::read_embedded() {
            Ok(value) => value,
            Err(error) => {
                self.message = error;
                return;
            }
        }) else {
            self.message = "当前程序没有内嵌衡枢载荷".into();
            return;
        };
        let Some(tool) = embedded.descriptor.tool.as_ref() else {
            self.message = "GUI 包缺少工具 schema".into();
            return;
        };
        let text_parameters = self
            .values
            .iter()
            .filter(|(name, value)| {
                !value.is_empty()
                    || tool
                        .parameters
                        .get(*name)
                        .is_some_and(|parameter| parameter.required)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        for (name, value) in &text_parameters {
            if tool
                .parameters
                .get(name)
                .is_some_and(|parameter| parameter.is_secret())
            {
                crate::privacy::register_secret(value);
            }
        }
        let parameters = match tool
            .parse_text_parameters(&text_parameters)
            .and_then(|value| tool.validate_parameters(&value))
        {
            Ok(value) => value,
            Err(error) => {
                self.message = error;
                return;
            }
        };
        let args = match tool.encode_program_args(&parameters) {
            Ok(args) => args,
            Err(error) => {
                self.message = error;
                return;
            }
        };
        // 凭证记录本次提交的参数，但秘密参数只保留名字不保留值。
        self.last_parameters = parameters
            .iter()
            .filter(|(name, _)| {
                !tool
                    .parameters
                    .get(*name)
                    .is_some_and(ParameterSpec::is_secret)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let mut permissions = embedded.permissions;
        let base = application_base_dir();
        let output_root = match self.output_path(&base) {
            Ok(path) => path,
            Err(error) => {
                self.message = error;
                return;
            }
        };
        if let Err(error) = fs::create_dir_all(&output_root) {
            self.message = format!("创建输出目录失败：{error}");
            return;
        }
        permissions.anchor_file_roots(&base);
        let host_permissions = permissions.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            crate::evidence::start_observing();
            let started = std::time::Instant::now();
            let (mut interpreter, error) = crate::vm::execute_run_with_observation_and_cancellation(
                &embedded.bytecode,
                embedded.modules,
                base.clone(),
                false,
                args,
                permissions,
                crate::execution::ObservationMode::Snapshots,
                crate::execution::ObservationBudget::default(),
                worker_cancellation,
            );
            let notebook = interpreter.finish_observation();
            let output = interpreter.output;
            crate::evidence::observe_standard_output(&output);
            let observed = crate::evidence::observations();
            let observed_metadata = crate::evidence::metadata_from_observations(&observed);
            let artifacts = observed
                .output_paths
                .iter()
                .filter(|path| path.is_file())
                .filter_map(|path| {
                    let mut artifact = Artifact::observe_file(path, "gui-output").ok()?;
                    artifact.metadata = observed_metadata.clone();
                    artifact.purpose = "运行输出".into();
                    if let Ok(relative) = artifact.path.strip_prefix(&base) {
                        artifact.path = relative.to_path_buf();
                    }
                    Some(artifact)
                })
                .collect::<Vec<_>>();
            let resources = crate::execution::ResourceSummary {
                elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                output_bytes: output.iter().map(String::len).sum(),
                observation_bytes: observed
                    .inputs
                    .iter()
                    .map(|input| usize::try_from(input.bytes).unwrap_or(usize::MAX))
                    .fold(0usize, usize::saturating_add),
                peak_events: notebook
                    .as_ref()
                    .map(|notebook| notebook.events.len())
                    .unwrap_or_default(),
            };
            crate::evidence::stop_observing();
            let report_warning = notebook.as_ref().and_then(|notebook| {
                save_notebook_files(&output_root, notebook, &host_permissions)
                    .err()
                    .map(|error| format!("过程报告保存失败：{error}"))
            });
            let result = match error {
                None => GuiJobResult::Succeeded {
                    output,
                    notebook,
                    artifacts,
                    output_root,
                    report_warning,
                    inputs: observed.inputs,
                    external_dependencies: observed.externals,
                    diagnostic_reports: Vec::new(),
                    resources,
                },
                Some(error) => GuiJobResult::Failed {
                    message: crate::privacy::scrub_secrets(&error.render()),
                    diagnostics: vec![crate::privacy::scrub_secrets(&error.message)],
                },
            };
            let _ = sender.send(result);
        });
        // 秘密参数默认不保存：提交后立刻从表单状态里清除，界面重新显示为空。
        for (name, value) in self.values.iter_mut() {
            if tool
                .parameters
                .get(name)
                .is_some_and(|parameter| parameter.is_secret())
            {
                value.clear();
            }
        }
        self.model.state = DesktopState::Running { progress: None };
        self.message = "正在运行…".into();
        self.job = Some(GuiJob {
            receiver,
            cancellation,
        });
    }

    fn poll_job(&mut self, context: &eframe::egui::Context) {
        let result = self
            .job
            .as_ref()
            .and_then(|job| job.receiver.try_recv().ok());
        if let Some(result) = result {
            self.job = None;
            match result {
                GuiJobResult::Succeeded {
                    output,
                    notebook,
                    artifacts,
                    output_root,
                    report_warning,
                    inputs,
                    external_dependencies,
                    diagnostic_reports,
                    resources,
                } => {
                    self.message = "运行成功".into();
                    self.program_output = output;
                    self.diagnostics.clear();
                    self.report_warning = report_warning;
                    self.artifacts = artifacts;
                    self.notebook = notebook;
                    self.last_inputs = inputs;
                    self.last_external_dependencies = external_dependencies;
                    self.last_diagnostic_reports = diagnostic_reports;
                    self.last_resources = resources;
                    self.output_root = output_root.display().to_string();
                    self.model.state = DesktopState::Succeeded {
                        artifacts: self.artifacts.clone(),
                    };
                }
                GuiJobResult::Failed {
                    message,
                    diagnostics,
                } => {
                    self.message = message.clone();
                    self.program_output.clear();
                    self.report_warning = None;
                    self.diagnostics = diagnostics.clone();
                    self.model.state = DesktopState::Failed {
                        message,
                        diagnostics,
                    };
                }
            }
        }
        if self.job.is_some() {
            context.request_repaint();
        }
    }

    fn save_notebook(&mut self, html: bool) {
        let Some(notebook) = self.notebook.as_ref() else {
            self.message = "当前没有可保存的过程报告".into();
            return;
        };
        let path = PathBuf::from(&self.output_root).join(if html {
            "过程报告.html"
        } else {
            "过程报告.json"
        });
        let content = if html {
            notebook.to_html()
        } else {
            notebook.to_json()
        };
        let permissions = crate::pack::read_embedded()
            .ok()
            .flatten()
            .map(|embedded| {
                let mut permissions = embedded.permissions;
                permissions.anchor_file_roots(&application_base_dir());
                permissions
            })
            .unwrap_or_else(|| crate::security::PermissionSet::for_source(Path::new(".")));
        match content.and_then(|content| {
            permissions.write_file_atomic(
                &path,
                content.as_bytes(),
                crate::privacy::Endpoint::LocalFile,
                &crate::execution::Sensitivity::default(),
            )
        }) {
            Ok(()) => self.message = format!("已保存过程报告：{}", path.display()),
            Err(error) => self.message = error,
        }
    }

    fn save_evidence(&mut self) {
        if !matches!(self.model.state, DesktopState::Succeeded { .. }) {
            self.message = "没有已完成的运行，无法写出凭证".into();
            return;
        }
        let embedded = match crate::pack::read_embedded() {
            Ok(Some(embedded)) => embedded,
            Ok(None) => {
                self.message = "当前程序没有内嵌衡枢载荷".into();
                return;
            }
            Err(error) => {
                self.message = error;
                return;
            }
        };
        let base = application_base_dir();
        let path = PathBuf::from(&self.output_root).join("运行.凭证");
        let manifest = gui_evidence_with_run(
            &self.source,
            &embedded.bytecode,
            &embedded.permissions,
            &base,
            &self.last_parameters,
            &self.program_output,
            &self.artifacts,
            self.last_inputs.clone(),
            self.last_diagnostic_reports.clone(),
            self.last_external_dependencies.clone(),
            self.last_resources.clone(),
        );
        let mut permissions = embedded.permissions;
        permissions.anchor_file_roots(&base);
        match manifest.write_file_authorized(
            &path,
            crate::resource::MAX_METADATA_BYTES,
            &permissions,
        ) {
            Ok(()) => self.message = format!("已保存运行凭证：{}", path.display()),
            Err(error) => self.message = error,
        }
    }

    fn show_parameters(&mut self, ui: &mut eframe::egui::Ui) {
        section(ui, "参数", |ui| {
            let mut rendered = false;
            if let Some(tool) = self
                .descriptor
                .as_ref()
                .and_then(|descriptor| descriptor.tool.as_ref())
            {
                if let Ok(controls) = controls_for_tool(tool) {
                    rendered = !controls.is_empty();
                    eframe::egui::Grid::new("parameter-grid")
                        .num_columns(2)
                        .spacing([10.0, 10.0])
                        .min_col_width(88.0)
                        .show(ui, |ui| {
                            for control in controls {
                                let label = if control.required {
                                    format!("{} *", control.label)
                                } else {
                                    control.label.clone()
                                };
                                ui.add(eframe::egui::Label::new(label).wrap());
                                let entry = self.values.entry(control.name.clone()).or_default();
                                if control.kind == ControlKind::Boolean {
                                    let mut checked = entry.parse::<bool>().unwrap_or(false);
                                    if ui
                                        .add(eframe::egui::Checkbox::new(&mut checked, "启用"))
                                        .changed()
                                    {
                                        *entry = checked.to_string();
                                    }
                                } else {
                                    let mut input = eframe::egui::TextEdit::singleline(entry)
                                        .desired_width(f32::INFINITY)
                                        .hint_text(control.description.clone());
                                    if control.secret {
                                        input = input.password(true);
                                    }
                                    ui.add(input);
                                }
                                ui.end_row();
                            }
                        });
                }
            }
            if !rendered {
                ui.label(eframe::egui::RichText::new("当前工具没有声明参数").weak());
            }
            ui.add_space(10.0);
            ui.label(eframe::egui::RichText::new("输出目录").strong());
            ui.add(
                eframe::egui::TextEdit::singleline(&mut self.output_root)
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                eframe::egui::RichText::new("相对于应用所在目录")
                    .weak()
                    .small(),
            );
        });
    }

    fn show_source_panel(&mut self, ui: &mut eframe::egui::Ui) {
        section(ui, "代码", |ui| {
            if ui
                .add(
                    eframe::egui::Button::new(if self.show_source {
                        "隐藏代码"
                    } else {
                        "查看代码"
                    })
                    .min_size(eframe::egui::vec2(96.0, 30.0)),
                )
                .clicked()
            {
                self.show_source = !self.show_source;
            }
            ui.add_space(6.0);
            if !self.show_source {
                ui.label(eframe::egui::RichText::new("源码已内嵌，点击查看").weak());
                return;
            }
            if self.source.is_empty() {
                ui.label(eframe::egui::RichText::new("当前载荷没有源码快照").weak());
                return;
            }
            let mut view = self.source.as_str();
            eframe::egui::ScrollArea::vertical()
                .id_salt("source-scroll")
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        eframe::egui::TextEdit::multiline(&mut view)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(10),
                    );
                });
        });
    }

    fn show_actions(&mut self, ui: &mut eframe::egui::Ui) {
        let (state_label, state_color) = self.state_badge(ui.visuals().dark_mode);
        section(ui, "运行", |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(state_color, "●");
                ui.label(eframe::egui::RichText::new(state_label).strong());
                if let DesktopState::Running {
                    progress: Some(progress),
                } = &self.model.state
                {
                    ui.add(
                        eframe::egui::ProgressBar::new(f32::from(*progress) / 100.0)
                            .desired_width(110.0),
                    );
                }
            });
            ui.add_space(10.0);
            let accent = ui.visuals().selection.stroke.color;
            let on_accent = if ui.visuals().dark_mode {
                eframe::egui::Color32::from_rgb(16, 22, 28)
            } else {
                eframe::egui::Color32::WHITE
            };
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.job.is_none(),
                        eframe::egui::Button::new(
                            eframe::egui::RichText::new("运行").color(on_accent),
                        )
                        .fill(accent)
                        .min_size(BUTTON_SIZE),
                    )
                    .clicked()
                {
                    self.run_embedded();
                }
                if ui
                    .add_enabled(self.job.is_some(), action_button("取消"))
                    .clicked()
                {
                    if let Some(job) = &self.job {
                        job.cancellation.store(true, Ordering::Relaxed);
                    }
                    self.model.state = DesktopState::Cancelling;
                    self.message = "正在取消…".into();
                }
                if ui
                    .add_enabled(self.job.is_none(), action_button("重置"))
                    .clicked()
                {
                    self.model = DesktopModel::default();
                    self.message.clear();
                    self.program_output.clear();
                    self.diagnostics.clear();
                    self.report_warning = None;
                    self.artifacts.clear();
                    self.notebook = None;
                }
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(self.notebook.is_some(), action_button("保存 JSON"))
                    .clicked()
                {
                    self.save_notebook(false);
                }
                if ui
                    .add_enabled(self.notebook.is_some(), action_button("保存 HTML"))
                    .clicked()
                {
                    self.save_notebook(true);
                }
                if ui
                    .add_enabled(
                        matches!(self.model.state, DesktopState::Succeeded { .. }),
                        action_button("保存凭证"),
                    )
                    .clicked()
                {
                    self.save_evidence();
                }
            });
        });
    }

    fn state_badge(&self, dark: bool) -> (&'static str, eframe::egui::Color32) {
        use eframe::egui::Color32;
        let (label, light_color, dark_color) = match &self.model.state {
            DesktopState::Ready => ("就绪", Color32::from_rgb(120, 128, 136), Color32::GRAY),
            DesktopState::Validating => (
                "校验中",
                Color32::from_rgb(23, 105, 170),
                Color32::from_rgb(102, 176, 235),
            ),
            DesktopState::Running { .. } => (
                "运行中",
                Color32::from_rgb(23, 105, 170),
                Color32::from_rgb(102, 176, 235),
            ),
            DesktopState::Cancelling => (
                "取消中",
                Color32::from_rgb(176, 108, 20),
                Color32::from_rgb(226, 168, 84),
            ),
            DesktopState::Succeeded { .. } => (
                "已完成",
                Color32::from_rgb(28, 128, 74),
                Color32::from_rgb(106, 196, 144),
            ),
            DesktopState::Failed { .. } => (
                "失败",
                Color32::from_rgb(176, 48, 48),
                Color32::from_rgb(232, 122, 122),
            ),
        };
        (label, if dark { dark_color } else { light_color })
    }

    fn show_results(&self, ui: &mut eframe::egui::Ui) {
        let id = eframe::egui::Id::new("hengshu-result-tab");
        let mut tab = ui
            .data_mut(|data| data.get_temp::<ResultTab>(id))
            .unwrap_or_default();
        let events = self.notebook.as_ref().map_or(0, |book| book.events.len());
        let diagnostics = self.diagnostics.len() + usize::from(self.report_warning.is_some());
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut tab,
                ResultTab::Output,
                format!("输出（{}）", self.program_output.len()),
            );
            ui.selectable_value(
                &mut tab,
                ResultTab::Artifacts,
                format!("交付物（{}）", self.artifacts.len()),
            );
            ui.selectable_value(&mut tab, ResultTab::Events, format!("过程（{events}）"));
            ui.selectable_value(
                &mut tab,
                ResultTab::Diagnostics,
                format!("诊断（{diagnostics}）"),
            );
        });
        ui.data_mut(|data| data.insert_temp(id, tab));
        let (hint, body, placeholder, salt) = match tab {
            ResultTab::Output => (
                "程序按行写出，可选中复制",
                self.output_text(),
                "暂无程序输出",
                "result-output",
            ),
            ResultTab::Artifacts => (
                "交付物来自输出目录，附字节数与 SHA-256",
                self.artifacts_text(),
                "暂无交付物",
                "result-artifacts",
            ),
            ResultTab::Events => (
                "按字节码指令记录，一条语句通常对应多条指令",
                self.events_text(),
                "暂无过程事件",
                "result-events",
            ),
            ResultTab::Diagnostics => (
                "诊断与报告警告",
                self.diagnostics_text(),
                "暂无诊断信息",
                "result-diagnostics",
            ),
        };
        ui.add_space(2.0);
        ui.label(eframe::egui::RichText::new(hint).weak().small());
        ui.add_space(4.0);
        read_only_text(ui, salt, &body, placeholder);
    }

    fn output_text(&self) -> String {
        self.program_output.join("\n")
    }

    fn artifacts_text(&self) -> String {
        self.artifacts
            .iter()
            .map(|artifact| {
                format!(
                    "{} · {} 字节\nSHA-256 {}",
                    artifact.display_name, artifact.bytes, artifact.sha256
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn events_text(&self) -> String {
        let Some(notebook) = &self.notebook else {
            return String::new();
        };
        let mut lines = notebook
            .events
            .iter()
            .take(200)
            .map(|event| {
                let position = event
                    .source_span
                    .as_ref()
                    .map(|span| format!("第{}行", span.start.line))
                    .unwrap_or_else(|| "位置未知".into());
                format!("#{} {} · {}", event.step, position, event.description)
            })
            .collect::<Vec<_>>();
        if notebook.events.len() > 200 {
            lines.push("事件过多，仅显示前 200 项".into());
        }
        lines.join("\n")
    }

    fn diagnostics_text(&self) -> String {
        let mut lines = self.diagnostics.clone();
        if let Some(warning) = &self.report_warning {
            lines.push(warning.clone());
        }
        lines.join("\n")
    }
}

#[cfg(feature = "gui")]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ResultTab {
    #[default]
    Output,
    Artifacts,
    Events,
    Diagnostics,
}

#[cfg(feature = "gui")]
const BUTTON_SIZE: eframe::egui::Vec2 = eframe::egui::vec2(78.0, 30.0);

#[cfg(feature = "gui")]
fn action_button(label: &str) -> eframe::egui::Button<'_> {
    eframe::egui::Button::new(label).min_size(BUTTON_SIZE)
}

#[cfg(feature = "gui")]
fn section(ui: &mut eframe::egui::Ui, title: &str, contents: impl FnOnce(&mut eframe::egui::Ui)) {
    eframe::egui::Frame::group(ui.style())
        .inner_margin(eframe::egui::Margin::same(10))
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(eframe::egui::RichText::new(title).strong());
            ui.separator();
            contents(ui);
        });
    ui.add_space(10.0);
}

/// 结果框的行数跟随剩余高度：窗口拉高框就变高，而不是固定高度把内容截断。
#[cfg(feature = "gui")]
fn fill_rows(available_height: f32, row_height: f32) -> usize {
    ((available_height - 16.0) / row_height.max(1.0))
        .floor()
        .clamp(4.0, 400.0) as usize
}

/// 结果文本用不可变 `&str` 缓冲：`TextBuffer for &str` 的 `is_mutable` 为假，
/// 因此文本框只读但仍可选中复制，而 `interactive(false)` 会连选中一起禁掉。
#[cfg(feature = "gui")]
fn read_only_text(ui: &mut eframe::egui::Ui, salt: &str, text: &str, placeholder: &str) {
    if text.is_empty() {
        ui.label(eframe::egui::RichText::new(placeholder).weak());
        return;
    }
    let row = ui
        .text_style_height(&eframe::egui::TextStyle::Monospace)
        .max(1.0);
    let rows = fill_rows(ui.available_height(), row);
    let mut view = text;
    eframe::egui::ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                eframe::egui::TextEdit::multiline(&mut view)
                    .font(eframe::egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(rows),
            );
        });
}

#[cfg(feature = "gui")]
fn application_base_dir() -> PathBuf {
    std::env::current_exe()
        .map(|path| application_base_dir_from(&path))
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(feature = "gui")]
fn application_base_dir_from(executable: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let mut directory = executable.parent();
        while let Some(candidate) = directory {
            if candidate.extension().and_then(|value| value.to_str()) == Some("app") {
                return candidate.parent().unwrap_or(candidate).to_path_buf();
            }
            directory = candidate.parent();
        }
    }
    executable
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(feature = "gui")]
fn save_notebook_files(
    root: &Path,
    notebook: &Notebook,
    permissions: &crate::security::PermissionSet,
) -> Result<(), String> {
    let sensitivity = notebook.events.iter().fold(
        crate::execution::Sensitivity::default(),
        |current, event| {
            event
                .inputs
                .values()
                .chain(event.outputs.values())
                .fold(current, |value, summary| {
                    value.union(&summary.metadata.sensitivity)
                })
        },
    );
    let json = notebook.to_json().map_err(|error| error.to_string())?;
    permissions.write_file_atomic(
        &root.join("过程报告.json"),
        json.as_bytes(),
        crate::privacy::Endpoint::LocalFile,
        &sensitivity,
    )?;
    let html = notebook.to_html().map_err(|error| error.to_string())?;
    permissions.write_file_atomic(
        &root.join("过程报告.html"),
        html.as_bytes(),
        crate::privacy::Endpoint::LocalFile,
        &sensitivity,
    )?;
    Ok(())
}

#[cfg(feature = "gui")]
impl eframe::App for GuiApp {
    fn update(&mut self, context: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        self.poll_job(context);
        eframe::egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                ui.heading(
                    self.descriptor
                        .as_ref()
                        .and_then(|descriptor| descriptor.tool.as_ref())
                        .and_then(|tool| tool.gui.as_ref())
                        .map(|gui| gui.title.as_str())
                        .or_else(|| {
                            self.descriptor
                                .as_ref()
                                .and_then(|descriptor| descriptor.tool.as_ref())
                                .map(|tool| tool.name.as_str())
                        })
                        .unwrap_or("衡枢工具"),
                );
                ui.separator();
                ui.label(eframe::egui::RichText::new("可信重复执行").weak());
            });
        });
        eframe::egui::SidePanel::left("workspace")
            .resizable(true)
            .default_width(320.0)
            .width_range(240.0..=460.0)
            .show(context, |ui| {
                eframe::egui::ScrollArea::vertical()
                    .id_salt("workspace-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.show_parameters(ui);
                        self.show_actions(ui);
                        self.show_source_panel(ui);
                    });
            });
        eframe::egui::TopBottomPanel::bottom("status").show(context, |ui| {
            ui.horizontal_wrapped(|ui| {
                if self.message.is_empty() {
                    ui.label(
                        eframe::egui::RichText::new(format!("输出目录：{}", self.output_root))
                            .weak(),
                    );
                } else {
                    ui.add(eframe::egui::Label::new(&self.message).wrap());
                }
            });
        });
        eframe::egui::CentralPanel::default().show(context, |ui| self.show_results(ui));
    }
}
mod model;
pub use model::{
    controls_for_tool, ControlKind, ControlSpec, DesktopEvent, DesktopModel, DesktopState,
};
#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{ParameterSpec, ToolManifest};

    #[test]
    fn controls_map_schema_without_executing_arbitrary_logic() {
        let tool = ToolManifest {
            version: Default::default(),
            name: "工具".into(),
            tool_version: "1".into(),
            entry: "main".into(),
            parameters: BTreeMap::from([
                (
                    "日期".into(),
                    ParameterSpec {
                        type_name: "日期".into(),
                        required: true,
                        secret: false,
                        description: String::new(),
                    },
                ),
                (
                    "密码".into(),
                    ParameterSpec {
                        type_name: "秘密".into(),
                        required: true,
                        secret: true,
                        description: String::new(),
                    },
                ),
            ]),
            gui: None,
            cli: None,
            permissions: Default::default(),
        };
        let controls = controls_for_tool(&tool).unwrap();
        assert_eq!(
            controls
                .iter()
                .find(|control| control.name == "日期")
                .unwrap()
                .kind,
            ControlKind::Date
        );
        assert_eq!(
            controls
                .iter()
                .find(|control| control.name == "密码")
                .unwrap()
                .kind,
            ControlKind::Secret
        );
    }

    #[test]
    fn desktop_state_machine_rejects_invalid_transitions_and_supports_cancel() {
        let mut model = DesktopModel::default();
        model.dispatch(DesktopEvent::Run).unwrap();
        model.dispatch(DesktopEvent::Run).unwrap();
        model.dispatch(DesktopEvent::Cancel).unwrap();
        assert!(matches!(model.state, DesktopState::Cancelling));
        model.dispatch(DesktopEvent::Completed(Vec::new())).unwrap();
        assert!(matches!(model.state, DesktopState::Ready));
    }

    #[test]
    fn gui_result_page_writes_a_verifiable_evidence_manifest() {
        let source = "输出(1)\n";
        let program = crate::parser::parse(source).unwrap();
        let bytecode = crate::compiler::compile(&program).unwrap();
        let base = std::path::Path::new("/安装目录");
        let permissions = crate::security::PermissionSet::for_source(base);
        let manifest = gui_evidence(
            source,
            &bytecode,
            &permissions,
            base,
            &BTreeMap::from([("月份".into(), serde_json::json!("2026-08"))]),
            &["1".to_string()],
            &[],
        );
        assert_eq!(manifest.evidence_version, crate::evidence::EVIDENCE_VERSION);
        assert_eq!(manifest.source_text.as_deref(), Some(source));
        assert_eq!(manifest.output, ["1"]);
        assert_eq!(manifest.parameters["月份"], serde_json::json!("2026-08"));
        assert_eq!(
            manifest.bytecode_digest,
            postcard::to_allocvec(&bytecode)
                .ok()
                .map(|bytes| crate::execution::hex_digest(&bytes))
        );
        assert!(manifest
            .external_dependencies
            .iter()
            .all(|dependency| dependency.level == crate::evidence::ExternalDependencyLevel::None));
        let permissions = manifest.permissions.unwrap();
        assert!(
            !format!("{permissions:?}").contains("/安装目录"),
            "凭证不应带上本机绝对路径：{permissions:?}"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn macos_app_output_base_is_next_to_bundle() {
        let executable = Path::new("/发布/平台工具.app/Contents/MacOS/主程序");
        let base = application_base_dir_from(executable);
        #[cfg(target_os = "macos")]
        assert_eq!(base, PathBuf::from("/发布"));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(base, PathBuf::from("/发布/平台工具.app/Contents/MacOS"));
    }

    #[cfg(feature = "gui")]
    #[test]
    fn narrow_high_dpi_result_panel_wraps_long_chinese_text() {
        use eframe::egui;

        fn collect_text(shape: &egui::Shape, found: &mut Vec<(usize, f32)>) {
            match shape {
                egui::Shape::Text(text) => {
                    found.push((text.galley.rows.len(), text.galley.rect.width()))
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_text(shape, found);
                    }
                }
                _ => {}
            }
        }

        let long = "衡枢已生成月度成绩分析报表并写入结果目录".repeat(20);
        let mut app = GuiApp::new();
        app.program_output = vec![long.clone()];
        app.diagnostics = vec![long.clone()];
        app.artifacts = vec![Artifact::from_bytes(
            format!("结果/{long}.xlsx"),
            "表格",
            b"payload",
        )];

        let context = egui::Context::default();
        context.set_pixels_per_point(2.0);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 240.0));
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| app.show_results(ui));
                },
            );
            for clipped in &output.shapes {
                collect_text(&clipped.shape, &mut texts);
            }
        }

        assert!(
            texts.iter().any(|(rows, _)| *rows > 1),
            "窄窗口下中文长文本应换行：{texts:?}"
        );
        assert!(
            texts.iter().all(|(_, width)| *width <= screen.width()),
            "文本不应超出窗口宽度：{texts:?}"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn keyboard_focus_and_typing_fill_parameter_controls() {
        use eframe::egui;

        let tool = ToolManifest {
            version: Default::default(),
            name: "月报".into(),
            tool_version: "1".into(),
            entry: "main".into(),
            parameters: BTreeMap::from([(
                "月份".into(),
                ParameterSpec {
                    type_name: "文本".into(),
                    required: true,
                    secret: false,
                    description: String::new(),
                },
            )]),
            gui: None,
            cli: None,
            permissions: Default::default(),
        };
        let mut app = GuiApp::new();
        app.descriptor = Some(crate::pack::PackageDescriptor {
            kind: crate::pack::PackageKind::Gui,
            tool: Some(tool),
            automation: None,
            entry_version: 1,
        });
        app.values.clear();

        let context = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(860.0, 640.0));
        let frame = |events: Vec<egui::Event>, app: &mut GuiApp| {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| app.show_parameters(ui));
                },
            );
        };

        frame(Vec::new(), &mut app);
        frame(
            vec![egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut app,
        );
        assert!(
            context.memory(|memory| memory.focused()).is_some(),
            "Tab 键应把焦点交给参数控件"
        );

        frame(vec![egui::Event::Text("2026-08".into())], &mut app);
        assert_eq!(
            app.values.get("月份").map(String::as_str),
            Some("2026-08"),
            "键盘输入应写入参数值：{:?}",
            app.values
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn system_cjk_font_covers_chinese_glyphs() {
        use eframe::egui;

        if system_cjk_font_path().is_none() {
            return;
        }

        let has_chinese = |context: &egui::Context| {
            let mut covered = false;
            let _ = context.run(egui::RawInput::default(), |context| {
                covered = context
                    .fonts(|fonts| fonts.has_glyphs(&egui::FontId::proportional(15.0), "衡枢报表"));
            });
            covered
        };

        let default_fonts = egui::Context::default();
        let with_cjk = egui::Context::default();
        install_system_cjk_font(&with_cjk);

        assert!(!has_chinese(&default_fonts), "egui 默认字体本不含汉字");
        assert!(has_chinese(&with_cjk), "安装系统字体后应覆盖汉字字形");
    }

    #[cfg(feature = "gui")]
    #[test]
    fn mixed_chinese_and_latin_text_shares_one_baseline() {
        use eframe::egui;

        if system_cjk_font_path().is_none() {
            return;
        }

        let context = egui::Context::default();
        install_system_cjk_font(&context);
        let mut baselines = Vec::new();
        let _ = context.run(egui::RawInput::default(), |context| {
            let galley = context.fonts(|fonts| {
                fonts.layout_no_wrap(
                    "运行 Run 12".to_owned(),
                    egui::FontId::proportional(15.0),
                    egui::Color32::BLACK,
                )
            });
            baselines = galley.rows[0]
                .glyphs
                .iter()
                .map(|glyph| (glyph.chr, glyph.pos.y))
                .collect();
        });

        let first = baselines
            .first()
            .map(|(_, y)| *y)
            .expect("混排文本应产出字形");
        assert!(
            baselines.iter().all(|(_, y)| *y == first),
            "中文与拉丁字形必须共用同一基线：{baselines:?}"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn result_text_is_left_aligned() {
        use eframe::egui;

        fn collect_halign(shape: &egui::Shape, found: &mut Vec<egui::Align>) {
            match shape {
                egui::Shape::Text(text) => found.push(text.galley.job.halign),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_halign(shape, found);
                    }
                }
                _ => {}
            }
        }

        let mut app = GuiApp::new();
        app.program_output = vec!["平台打包成功".into(), "结果已写入 结果/".into()];
        let context = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0));
        let mut aligns = Vec::new();
        for _ in 0..2 {
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        app.show_parameters(ui);
                        app.show_results(ui);
                    });
                },
            );
            aligns.clear();
            for clipped in &output.shapes {
                collect_halign(&clipped.shape, &mut aligns);
            }
        }

        assert!(!aligns.is_empty(), "参数区与结果页应渲染文本");
        assert!(
            aligns.iter().all(|align| *align == egui::Align::Min),
            "参数与结果文字必须左对齐：{aligns:?}"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn result_box_rows_follow_the_available_height() {
        let short = fill_rows(200.0, 16.0);
        let tall = fill_rows(800.0, 16.0);
        assert!(tall > short, "窗口拉高后结果框应变高：{short} -> {tall}");
        assert!(fill_rows(0.0, 16.0) >= 4, "极窄窗口也要保留可用行数");
        assert!(fill_rows(1.0e6, 16.0) <= 400, "行数必须有上界");
    }

    #[cfg(feature = "gui")]
    #[test]
    fn visual_style_applies_to_both_light_and_dark_themes() {
        use eframe::egui::{self, Theme};

        let context = egui::Context::default();
        apply_visual_style(&context);
        for (theme, accent) in [(Theme::Light, ACCENT_LIGHT), (Theme::Dark, ACCENT_DARK)] {
            let style = context.style_of(theme);
            assert_eq!(
                style.text_styles[&egui::TextStyle::Body].size,
                15.0,
                "{theme:?} 主题字号未生效"
            );
            assert_eq!(
                style.visuals.widgets.inactive.corner_radius,
                egui::CornerRadius::same(8),
                "{theme:?} 主题圆角未生效"
            );
            assert_eq!(
                style.visuals.selection.stroke.color, accent,
                "{theme:?} 主题强调色未生效"
            );
        }
    }

    #[cfg(feature = "gui")]
    #[test]
    fn process_tab_shows_the_source_line_of_each_instruction_event() {
        use eframe::egui;

        fn collect_text(shape: &egui::Shape, found: &mut String) {
            match shape {
                egui::Shape::Text(text) => found.push_str(text.galley.text()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_text(shape, found);
                    }
                }
                _ => {}
            }
        }

        let mut app = GuiApp::new();
        app.notebook = Some(crate::provenance::Notebook {
            version: crate::execution::CONTRACT_VERSION,
            mode: crate::execution::ObservationMode::Snapshots,
            events: vec![crate::execution::ExecutionEvent {
                version: Default::default(),
                step: 1,
                source_span: Some(crate::diagnostic::SourceSpan::point(7, 1)),
                stage: "instruction".into(),
                description: "执行指令".into(),
                inputs: Default::default(),
                outputs: Default::default(),
                artifacts: Vec::new(),
                warnings: Vec::new(),
                provenance: Vec::new(),
            }],
            truncated: false,
        });

        let context = egui::Context::default();
        context.data_mut(|data| {
            data.insert_temp(egui::Id::new("hengshu-result-tab"), ResultTab::Events)
        });
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0));
        let mut rendered = String::new();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| app.show_results(ui));
            },
        );
        for clipped in &output.shapes {
            collect_text(&clipped.shape, &mut rendered);
        }

        assert!(
            rendered.contains("#1 第7行 · 执行指令"),
            "过程页每条事件都应标注源码行：{rendered}"
        );
        assert!(
            rendered.contains("按字节码指令记录"),
            "过程页应说明事件粒度：{rendered}"
        );
    }
}
