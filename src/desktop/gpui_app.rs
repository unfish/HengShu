use super::{controls_for_tool, gui_evidence_with_run, ControlKind, DesktopModel, DesktopState};
use crate::execution::{Artifact, OutputKind, ParameterSpec};
use crate::provenance::Notebook;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    tab::TabBar,
    v_flex, ActiveTheme, Disableable as _, IconName, Root, Sizable as _, Theme, ThemeMode,
    TitleBar,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

const SIDEBAR_WIDTH: Pixels = px(336.);

pub fn run_gui() {
    crate::privacy::install_secret_scrubbing_panic_hook();
    let embedded = crate::pack::read_embedded().ok().flatten();
    let gui = embedded
        .as_ref()
        .and_then(|value| value.descriptor.tool.as_ref())
        .and_then(|tool| tool.gui.as_ref());
    let title = gui
        .map(|gui| gui.title.trim())
        .filter(|title| !title.is_empty())
        .unwrap_or("衡枢")
        .to_string();
    let width = gui.map_or(1080., |gui| gui.width.max(900) as f32);
    let height = gui.map_or(720., |gui| gui.height.max(620) as f32);

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            install_cjk_font(cx);
            cx.set_app_identity("cn.hengshu.desktop", "衡枢");
            cx.activate(true);

            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(900.), px(620.))),
                ..TitleBar::window_options()
            };
            let window_title = title.clone();
            cx.spawn(async move |cx| {
                let handle = cx
                    .open_window(options, |window, cx| {
                        let app = cx.new(|cx| GuiApp::new(window, cx));
                        cx.new(|cx| Root::new(app, window, cx))
                    })
                    .expect("无法创建衡枢窗口");
                handle
                    .update(cx, |_, window, _| {
                        window.set_window_title(&window_title);
                        window.activate_window();
                    })
                    .expect("无法激活衡枢窗口");
            })
            .detach();
        });
}

fn install_cjk_font(cx: &mut App) {
    let bytes = include_bytes!("../../assets/fonts/NotoSansSC-Regular.otf");
    if cx
        .text_system()
        .add_fonts(vec![Cow::Borrowed(bytes.as_slice())])
        .is_ok()
    {
        Theme::global_mut(cx).font_family = "Noto Sans SC".into();
        Theme::global_mut(cx).mono_font_family = "Noto Sans SC".into();
        Theme::sync_base(cx);
    }
}

struct ParameterField {
    name: String,
    label: String,
    kind: ControlKind,
    required: bool,
    secret: bool,
    description: String,
    input: Option<Entity<InputState>>,
    checked: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ResultTab {
    #[default]
    Output,
    Artifacts,
    Events,
    Diagnostics,
    Source,
}

impl ResultTab {
    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Artifacts,
            2 => Self::Events,
            3 => Self::Diagnostics,
            4 => Self::Source,
            _ => Self::Output,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Output => 0,
            Self::Artifacts => 1,
            Self::Events => 2,
            Self::Diagnostics => 3,
            Self::Source => 4,
        }
    }
}

struct GuiApp {
    model: DesktopModel,
    descriptor: Option<crate::pack::PackageDescriptor>,
    fields: Vec<ParameterField>,
    output_input: Entity<InputState>,
    result_textareas: Vec<Entity<TextareaState>>,
    last_output_root: Option<PathBuf>,
    source: String,
    message: String,
    program_output: Vec<String>,
    diagnostics: Vec<String>,
    report_warning: Option<String>,
    job: Option<GuiJob>,
    poll_task: Option<Task<()>>,
    artifacts: Vec<Artifact>,
    notebook: Option<Notebook>,
    last_parameters: BTreeMap<String, serde_json::Value>,
    last_inputs: Vec<crate::execution::InputObservation>,
    last_external_dependencies: Vec<crate::evidence::ExternalDependency>,
    last_diagnostic_reports: Vec<crate::api::DiagnosticReport>,
    last_resources: crate::execution::ResourceSummary,
    active_tab: ResultTab,
}

struct GuiJob {
    receiver: Receiver<GuiJobResult>,
    cancellation: Arc<AtomicBool>,
}

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

impl GuiApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let embedded = crate::pack::read_embedded().ok().flatten();
        let source = embedded
            .as_ref()
            .map(|embedded| embedded.source.clone())
            .unwrap_or_default();
        let descriptor = embedded.map(|embedded| embedded.descriptor);
        let fields = descriptor
            .as_ref()
            .and_then(|descriptor| descriptor.tool.as_ref())
            .and_then(|tool| controls_for_tool(tool).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|control| {
                let input = (control.kind != ControlKind::Boolean).then(|| {
                    cx.new(|cx| {
                        let state =
                            InputState::new(window, cx).placeholder(control.description.clone());
                        if control.secret {
                            state.masked(true)
                        } else {
                            state
                        }
                    })
                });
                ParameterField {
                    name: control.name,
                    label: control.label,
                    kind: control.kind,
                    required: control.required,
                    secret: control.secret,
                    description: control.description,
                    input,
                    checked: false,
                }
            })
            .collect();
        let output_root = descriptor
            .as_ref()
            .and_then(|descriptor| descriptor.tool.as_ref())
            .map(|tool| tool.output_root(OutputKind::Gui).display().to_string())
            .unwrap_or_else(|| "结果".into());
        let output_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(output_root)
                .placeholder("结果")
        });
        let placeholders = [
            "运行任务后，输出会显示在这里",
            "暂无交付物",
            "暂无过程事件",
            "暂无诊断信息",
            "当前载荷没有源码快照",
        ];
        let result_textareas = placeholders
            .into_iter()
            .enumerate()
            .map(|(index, placeholder)| {
                let value = if index == ResultTab::Source.index() {
                    source.clone()
                } else {
                    String::new()
                };
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                        .soft_wrap(true)
                })
            })
            .collect();

        Self {
            model: DesktopModel::default(),
            descriptor,
            fields,
            output_input,
            result_textareas,
            last_output_root: None,
            source,
            message: String::new(),
            program_output: Vec::new(),
            diagnostics: Vec::new(),
            report_warning: None,
            job: None,
            poll_task: None,
            artifacts: Vec::new(),
            notebook: None,
            last_parameters: BTreeMap::new(),
            last_inputs: Vec::new(),
            last_external_dependencies: Vec::new(),
            last_diagnostic_reports: Vec::new(),
            last_resources: crate::execution::ResourceSummary::default(),
            active_tab: ResultTab::Output,
        }
    }

    fn output_root(&self, cx: &App) -> String {
        self.output_input.read(cx).value().to_string()
    }

    fn output_path(&self, base: &Path, cx: &App) -> Result<PathBuf, String> {
        let output_root = self.output_root(cx);
        let relative = Path::new(&output_root);
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

    fn run_embedded(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        let Some(embedded) = (match crate::pack::read_embedded() {
            Ok(value) => value,
            Err(error) => {
                self.message = error;
                cx.notify();
                return;
            }
        }) else {
            self.message = "当前程序没有内嵌衡枢载荷".into();
            cx.notify();
            return;
        };
        let Some(tool) = embedded.descriptor.tool.as_ref() else {
            self.message = "GUI 包缺少工具 schema".into();
            cx.notify();
            return;
        };
        let text_parameters = self
            .fields
            .iter()
            .filter_map(|field| {
                let value = if field.kind == ControlKind::Boolean {
                    field.checked.to_string()
                } else {
                    field.input.as_ref()?.read(cx).value().to_string()
                };
                let present = if field.kind == ControlKind::Boolean {
                    field.checked || field.required
                } else {
                    !value.is_empty() || field.required
                };
                present.then(|| (field.name.clone(), value))
            })
            .collect::<BTreeMap<_, _>>();
        for field in &self.fields {
            if field.secret {
                if let Some(value) = text_parameters.get(&field.name) {
                    crate::privacy::register_secret(value);
                }
            }
        }
        let parameters = match tool
            .parse_text_parameters(&text_parameters)
            .and_then(|value| tool.validate_parameters(&value))
        {
            Ok(value) => value,
            Err(error) => {
                self.message = error;
                cx.notify();
                return;
            }
        };
        let args = match tool.encode_program_args(&parameters) {
            Ok(args) => args,
            Err(error) => {
                self.message = error;
                cx.notify();
                return;
            }
        };
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
        let output_root = match self.output_path(&base, cx) {
            Ok(path) => path,
            Err(error) => {
                self.message = error;
                cx.notify();
                return;
            }
        };
        if let Err(error) = fs::create_dir_all(&output_root) {
            self.message = format!("创建输出目录失败：{error}");
            cx.notify();
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
                peak_events: notebook.as_ref().map_or(0, |book| book.events.len()),
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

        for field in &self.fields {
            if field.secret {
                if let Some(input) = &field.input {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
        }
        self.model.state = DesktopState::Running { progress: None };
        self.message = "正在运行...".into();
        self.job = Some(GuiJob {
            receiver,
            cancellation,
        });
        self.start_polling(window, cx);
        cx.notify();
    }

    fn start_polling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.poll_task = Some(cx.spawn_in(window, async move |this, window| loop {
            window
                .background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let finished = this
                .update_in(window, |this, window, cx| {
                    let finished = this.poll_job(window, cx);
                    cx.notify();
                    finished
                })
                .unwrap_or(true);
            if finished {
                break;
            }
        }));
    }

    fn poll_job(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let result = self
            .job
            .as_ref()
            .and_then(|job| job.receiver.try_recv().ok());
        let Some(result) = result else {
            return self.job.is_none();
        };
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
                self.last_output_root = Some(output_root);
                self.model.state = DesktopState::Succeeded {
                    artifacts: self.artifacts.clone(),
                };
                self.active_tab = ResultTab::Output;
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
                self.active_tab = ResultTab::Diagnostics;
            }
        }
        self.refresh_result_textareas(window, cx);
        true
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = &self.job {
            job.cancellation.store(true, Ordering::Relaxed);
            self.model.state = DesktopState::Cancelling;
            self.message = "正在取消...".into();
            cx.notify();
        }
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model = DesktopModel::default();
        self.message.clear();
        self.program_output.clear();
        self.diagnostics.clear();
        self.report_warning = None;
        self.artifacts.clear();
        self.notebook = None;
        self.last_output_root = None;
        self.active_tab = ResultTab::Output;
        self.refresh_result_textareas(window, cx);
        cx.notify();
    }

    fn refresh_result_textareas(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = [
            self.program_output.join("\n"),
            self.artifacts_text(),
            self.events_text(),
            self.diagnostics_text(),
            self.source.clone(),
        ];
        for (state, value) in self.result_textareas.iter().zip(values) {
            if state.read(cx).value().as_ref() != value {
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
    }

    fn save_notebook(&mut self, html: bool, cx: &mut Context<Self>) {
        let Some(notebook) = self.notebook.as_ref() else {
            self.message = "当前没有可保存的过程报告".into();
            cx.notify();
            return;
        };
        let path = self
            .last_output_root
            .clone()
            .unwrap_or_else(|| application_base_dir().join(self.output_root(cx)))
            .join(if html {
                "过程报告.html"
            } else {
                "过程报告.json"
            });
        let content = if html {
            notebook.to_html()
        } else {
            notebook.to_json()
        };
        let permissions = anchored_permissions();
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
        cx.notify();
    }

    fn save_evidence(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.model.state, DesktopState::Succeeded { .. }) {
            self.message = "没有已完成的运行，无法写出凭证".into();
            cx.notify();
            return;
        }
        let embedded = match crate::pack::read_embedded() {
            Ok(Some(embedded)) => embedded,
            Ok(None) => {
                self.message = "当前程序没有内嵌衡枢载荷".into();
                cx.notify();
                return;
            }
            Err(error) => {
                self.message = error;
                cx.notify();
                return;
            }
        };
        let base = application_base_dir();
        let path = self
            .last_output_root
            .clone()
            .unwrap_or_else(|| application_base_dir().join(self.output_root(cx)))
            .join("运行.凭证");
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
        cx.notify();
    }

    fn title(&self) -> &str {
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
            .unwrap_or("衡枢工具")
    }

    fn state_label(&self) -> &'static str {
        match self.model.state {
            DesktopState::Ready => "就绪",
            DesktopState::Validating => "校验中",
            DesktopState::Running { .. } => "运行中",
            DesktopState::Cancelling => "取消中",
            DesktopState::Succeeded { .. } => "已完成",
            DesktopState::Failed { .. } => "失败",
        }
    }

    fn state_color(&self, cx: &App) -> Hsla {
        match self.model.state {
            DesktopState::Ready => cx.theme().muted_foreground,
            DesktopState::Validating | DesktopState::Running { .. } => cx.theme().info,
            DesktopState::Cancelling => cx.theme().warning,
            DesktopState::Succeeded { .. } => cx.theme().success,
            DesktopState::Failed { .. } => cx.theme().danger,
        }
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_dark = cx.theme().mode.is_dark();
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .size(px(20.))
                                .rounded(px(5.))
                                .bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                                .text_xs()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child("衡"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(self.title().to_string()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("可信重复执行"),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_xs()
                                .text_color(self.state_color(cx))
                                .child("●")
                                .child(self.state_label()),
                        )
                        .child(
                            Button::new("theme")
                                .ghost()
                                .xsmall()
                                .icon(if is_dark {
                                    IconName::Sun
                                } else {
                                    IconName::Moon
                                })
                                .tooltip(if is_dark {
                                    "切换浅色主题"
                                } else {
                                    "切换深色主题"
                                })
                                .on_click(|_, window, cx| {
                                    let mode = if cx.theme().mode.is_dark() {
                                        ThemeMode::Light
                                    } else {
                                        ThemeMode::Dark
                                    };
                                    Theme::change(mode, Some(window), cx);
                                }),
                        ),
                ),
        )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.job.is_some();
        let fields =
            self.fields
                .iter()
                .enumerate()
                .fold(v_flex().gap_4(), |column, (index, field)| {
                    let label = if field.required {
                        format!("{}  *", field.label)
                    } else {
                        field.label.clone()
                    };
                    let control = if field.kind == ControlKind::Boolean {
                        Checkbox::new(("parameter-check", index))
                            .checked(field.checked)
                            .label("启用")
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                this.fields[index].checked = *checked;
                                cx.notify();
                            }))
                            .into_any_element()
                    } else {
                        Input::new(field.input.as_ref().expect("text field has input"))
                            .when(field.secret, |input| input.mask_toggle())
                            .into_any_element()
                    };
                    column.child(
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(control_kind_label(field.kind)),
                                    ),
                            )
                            .child(control)
                            .when(!field.description.is_empty(), |element| {
                                element.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(field.description.clone()),
                                )
                            }),
                    )
                });

        v_flex()
            .w(SIDEBAR_WIDTH)
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                v_flex()
                    .id("parameter-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_5()
                    .gap_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("任务参数"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("填写本次运行需要的输入"),
                            ),
                    )
                    .child(if self.fields.is_empty() {
                        div()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted)
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("当前工具没有声明参数")
                            .into_any_element()
                    } else {
                        fields.into_any_element()
                    })
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("输出目录"),
                            )
                            .child(Input::new(&self.output_input).prefix(IconName::FolderOpen))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("相对于应用所在目录"),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .p_4()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("run")
                            .primary()
                            .w_full()
                            .icon(IconName::Play)
                            .label(if running { "运行中" } else { "运行" })
                            .loading(running)
                            .disabled(running)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.run_embedded(window, cx)),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("cancel")
                                    .outline()
                                    .flex_1()
                                    .icon(IconName::CircleX)
                                    .label("取消")
                                    .disabled(!running)
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                            )
                            .child(
                                Button::new("reset")
                                    .outline()
                                    .flex_1()
                                    .icon(IconName::RotateCw)
                                    .label("重置")
                                    .disabled(running)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.reset(window, cx)),
                                    ),
                            ),
                    ),
            )
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let events = self.notebook.as_ref().map_or(0, |book| book.events.len());
        let diagnostics = self.diagnostics.len() + usize::from(self.report_warning.is_some());
        TabBar::new("result-tabs")
            .underline()
            .w_full()
            .selected_index(self.active_tab.index())
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                this.active_tab = ResultTab::from_index(*index);
                cx.notify();
            }))
            .px_5()
            .border_b_1()
            .border_color(cx.theme().border)
            .children([
                format!("输出  {}", self.program_output.len()),
                format!("交付物  {}", self.artifacts.len()),
                format!("过程  {events}"),
                format!("诊断  {diagnostics}"),
                "源码".into(),
            ])
    }

    fn render_result(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (heading, hint) = match self.active_tab {
            ResultTab::Output => ("程序输出", "按行显示本次运行的标准输出"),
            ResultTab::Artifacts => ("交付物", "输出文件及其可复核摘要"),
            ResultTab::Events => ("执行过程", "按字节码指令记录，一条语句通常对应多条指令"),
            ResultTab::Diagnostics => ("诊断", "运行错误与过程报告警告"),
            ResultTab::Source => ("内嵌源码", "当前工具包中的只读源码快照"),
        };
        let textarea = &self.result_textareas[self.active_tab.index()];
        v_flex()
            .flex_1()
            .min_h_0()
            .p_5()
            .gap_4()
            .child(
                h_flex()
                    .items_start()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(heading),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(hint),
                            ),
                    )
                    .when(self.active_tab == ResultTab::Events, |row| {
                        row.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("save-json")
                                        .outline()
                                        .small()
                                        .icon(IconName::FileText)
                                        .label("JSON")
                                        .disabled(self.notebook.is_none())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.save_notebook(false, cx)
                                        })),
                                )
                                .child(
                                    Button::new("save-html")
                                        .outline()
                                        .small()
                                        .icon(IconName::FileText)
                                        .label("HTML")
                                        .disabled(self.notebook.is_none())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.save_notebook(true, cx)
                                        })),
                                ),
                        )
                    })
                    .when(self.active_tab == ResultTab::Artifacts, |row| {
                        row.child(
                            Button::new("save-evidence")
                                .outline()
                                .small()
                                .icon(IconName::Check)
                                .label("保存凭证")
                                .disabled(!matches!(
                                    self.model.state,
                                    DesktopState::Succeeded { .. }
                                ))
                                .on_click(cx.listener(|this, _, _, cx| this.save_evidence(cx))),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted.opacity(0.35))
                    .child(
                        Textarea::new(textarea)
                            .h(relative(1.))
                            .appearance(false)
                            .bordered(false)
                            .readonly(true)
                            .aria_label(heading),
                    ),
            )
    }

    fn artifacts_text(&self) -> String {
        self.artifacts
            .iter()
            .map(|artifact| {
                format!(
                    "{}  ·  {} 字节\nSHA-256  {}",
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
                format!("#{}  {}  ·  {}", event.step, position, event.description)
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

impl Render for GuiApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if self.message.is_empty() {
            format!("输出目录：{}", self.output_root(cx))
        } else {
            self.message.clone()
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.render_sidebar(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_tabs(cx))
                            .child(self.render_result(cx)),
                    ),
            )
            .child(
                h_flex()
                    .h(px(30.))
                    .flex_none()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .text_xs()
                    .text_color(if matches!(self.model.state, DesktopState::Failed { .. }) {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(IconName::SquareTerminal)
                    .child(status),
            )
    }
}

fn control_kind_label(kind: ControlKind) -> &'static str {
    match kind {
        ControlKind::Text => "文本",
        ControlKind::Integer => "整数",
        ControlKind::Decimal => "数字",
        ControlKind::Date => "日期",
        ControlKind::DateTime => "日期时间",
        ControlKind::Boolean => "开关",
        ControlKind::Enum => "选项",
        ControlKind::File => "文件",
        ControlKind::Folder => "文件夹",
        ControlKind::Secret => "秘密",
    }
}

fn anchored_permissions() -> crate::security::PermissionSet {
    crate::pack::read_embedded()
        .ok()
        .flatten()
        .map(|embedded| {
            let mut permissions = embedded.permissions;
            permissions.anchor_file_roots(&application_base_dir());
            permissions
        })
        .unwrap_or_else(|| crate::security::PermissionSet::for_source(Path::new(".")))
}

fn application_base_dir() -> PathBuf {
    std::env::current_exe()
        .map(|path| application_base_dir_from(&path))
        .unwrap_or_else(|_| PathBuf::from("."))
}

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

#[cfg(test)]
mod tests {
    use super::{application_base_dir_from, Path, PathBuf};

    #[test]
    fn macos_app_output_base_is_next_to_bundle() {
        let executable = Path::new("/发布/平台工具.app/Contents/MacOS/主程序");
        let base = application_base_dir_from(executable);
        #[cfg(target_os = "macos")]
        assert_eq!(base, PathBuf::from("/发布"));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(base, PathBuf::from("/发布/平台工具.app/Contents/MacOS"));
    }
}
