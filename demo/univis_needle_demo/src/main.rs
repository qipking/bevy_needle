//! bevy_needle × univis_ui 演示：文字指令操控 UI。
//!
//! 数据流：
//! ```text
//! 键盘输入 → RunAgent → Needle 约束解码(本地 libneedle.so)
//!        → ToolCallRequested/Completed 消息 → univis_ui 组件突变
//! ```
//!
//! 运行： cargo run -p univis_needle_demo --release
//! 指令（英文，Needle 为英文工具调用模型）：
//!   set volume to 80          switch theme to sunset
//!   turn off the animation    set brightness to 20
//!   change the title to hello set the status to ready
//!   press the about button    make the title green   reset the ui

use std::collections::{HashMap, VecDeque};

use bevy::{
    input::{keyboard::KeyboardInput, ButtonState},
    prelude::*,
    render::view::screenshot::{save_to_disk, Screenshot},
    window::{Ime, WindowResolution},
};
use bevy_needle::prelude::*;
use serde_json::json;
use univis_ui::prelude::*;

// ---------------------------------------------------------------------------
// 资源与标记
// ---------------------------------------------------------------------------

/// 名字 → UI 实体（工具调用的定位表）。
#[derive(Resource, Default)]
struct UiTargets(HashMap<String, Entity>);

/// 控制台 agent。
#[derive(Resource)]
struct ConsoleAgent(Entity);

/// 键盘输入缓冲。
#[derive(Resource, Default)]
struct InputBuffer(String);

/// 聊天记录（最近 N 行）。
#[derive(Resource, Default)]
struct ChatLog(Vec<String>);

impl ChatLog {
    const MAX_LINES: usize = 12;
    fn push(&mut self, line: impl Into<String>) {
        self.0.push(line.into());
        if self.0.len() > Self::MAX_LINES {
            self.0.remove(0);
        }
    }
    fn text(&self) -> String {
        if self.0.is_empty() {
            "（暂无消息）".to_string()
        } else {
            self.0.join("\n")
        }
    }
}

/// 状态栏文本。
#[derive(Resource, Default)]
struct StatusLine(String);

/// 自动驾驶脚本：每项 = (帧延迟, 假装的输入文本)；None = 触发一次截图。
#[derive(Resource, Default)]
struct AutoPilot {
    steps: VecDeque<(usize, Option<String>)>,
    frame: usize,
    shot_index: usize,
}

/// 本次进程是否为截图/调试运行（环境变量 BEVY_NEEDLE_AUTOPILOT=1）。
#[derive(Resource)]
struct AutoPilotEnabled;

fn parse_autopilot() -> Option<AutoPilot> {
    let burst = std::env::var("BEVY_NEEDLE_BURST").is_ok_and(|v| v.trim() == "1");
    let autopilot = std::env::var("BEVY_NEEDLE_AUTOPILOT").is_ok_and(|v| v.trim() == "1");
    if !burst && !autopilot {
        return None;
    }
    if burst {
        // 闪烁诊断：frame 180 起每 6 帧一张，连拍 10 张（约 100ms 间隔）
        let steps = (0..10)
            .map(|i| (180 + i * 6, None))
            .collect::<VecDeque<_>>();
        return Some(AutoPilot { steps, frame: 0, shot_index: 0 });
    }
    // 每条指令间隔约 4 秒（60fps × 240 帧），给引擎留出解码时间
    let commands: &[&str] = if std::env::var("BEVY_NEEDLE_QUICK").is_ok_and(|v| v == "1") {
        &["turn the animation off"]
    } else {
        &[
            "set the volume to 80",
            "turn the animation off",
            "set the theme to sunset",
            "set brightness to 15",
            "change the title to hello",
            "make the title green",
        ]
    };
    // 每条指令后都截图：用于逐命令的像素级状态回归（防止"回执成功但 UI 没变"）
    let mut steps: VecDeque<(usize, Option<String>)> = VecDeque::new();
    steps.push_back((200, None)); // 初始状态基准帧
    for (i, cmd) in commands.iter().enumerate() {
        let fire = 240 + i as usize * 240;
        steps.push_back((fire, Some((*cmd).to_string())));
        steps.push_back((fire + 200, None)); // 指令解析约 2~3.5s 后状态已稳定
    }
    Some(AutoPilot {
        steps,
        frame: 0,
        shot_index: 0,
    })
}

/// 当前主题。
#[derive(Resource, Default, PartialEq, Clone, Copy)]
struct CurrentTheme(&'static str);

#[derive(Component)]
struct ChatHistoryLabel;

#[derive(Component)]
struct InputLineLabel;

#[derive(Component)]
struct StatusLabel;

/// 本地 UI 动作（按钮点击 / 工具触发共用）。
#[derive(Message)]
struct UiActionRequest {
    action: &'static str,
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "bevy_needle × univis_ui — 文字操控 UI 演示".into(),
                    resolution: WindowResolution::new(1280, 820),
                    ..default()
                }),
                ..default()
            }),
        )
        .add_plugins(UnivisUiPlugin)
        .add_plugins(BevyNeedlePlugin::default())
        .init_resource::<UiTargets>()
        .init_resource::<InputBuffer>()
        .init_resource::<ChatLog>()
        .init_resource::<StatusLine>()
        .init_resource::<CurrentTheme>()
        // 实验开关：BEVY_NEEDLE_FULL_SYNC=1 关闭 univis 增量渲染（每帧全量同步）
        .add_systems(Startup, setup_rollout_override)
        .add_systems(Startup, setup_autopilot)
        .add_systems(Update, (diagnose_frame_changes, run_autopilot, dump_ui_state))
        .add_message::<UiActionRequest>()
        .add_systems(Startup, (setup_camera, setup_ui, setup_agent).chain())
        .add_systems(
            Update,
            (
                capture_keyboard,
                sync_input_line,
                sync_status_line,
                sync_chat_log,
                poll_button_clicks,
                poll_widget_events,
                apply_tool_effects,
                log_tool_failures,
                report_run_outcomes,
            )
                .chain(),
        )
        .run();
}

fn setup_camera(mut commands: Commands) {
    commands.spawn(Camera2d);
}

fn setup_rollout_override(mut commands: Commands) {
    if std::env::var("BEVY_NEEDLE_FULL_SYNC").as_deref() == Ok("1") {
        let mut cfg = univis_ui::engine::schedule::UiRolloutConfig::default();
        cfg.use_incremental_render = false;
        cfg.use_incremental_solve = false;
        cfg.use_incremental_measure = false;
        commands.insert_resource(cfg);
        info!("univis 增量渲染已关闭（BEVY_NEEDLE_FULL_SYNC=1）");
    }
}

// ---------------------------------------------------------------------------
// UI 构建
// ---------------------------------------------------------------------------

const CJK_FONT_CANDIDATES: &[&str] = &[
    // 项目内资产（bevy AssetServer 默认拒绝绝对路径，见 UnapprovedPathMode）
    "fonts/NotoSansMonoCJK-VF.ttc",
];

#[derive(Resource)]
struct UiFont(Option<Handle<Font>>);

fn load_cjk_font(server: &AssetServer) -> UiFont {
    // AssetServer 相对 "assets/" 根解析；不能用 std::fs::exists 预判
    // （那检查的是进程工作目录）。直接交给 AssetServer，失败见资产日志。
    let path = CJK_FONT_CANDIDATES[0];
    info!("加载 CJK 字体资产: {path}");
    UiFont(Some(server.load(path)))
}

fn label(font: &UiFont, text: &str, size: f32, color: Color) -> UTextLabel {
    UTextLabel {
        text: text.to_string(),
        font_size: size,
        color,
        font: font.0.clone().unwrap_or_default(),
        ..default()
    }
}

fn setup_ui(mut commands: Commands, server: Res<AssetServer>, mut targets: ResMut<UiTargets>) {
    let font = load_cjk_font(&server);

    let root = commands
        .spawn((
            URootUi::screen(),
            UNode {
                width: UVal::Percent(1.0),
                height: UVal::Percent(1.0),
                background_color: Color::srgb(0.05, 0.07, 0.10),
                padding: USides::all(40.0),
                ..default()
            },
            ULayout {
                display: UDisplay::Flex,
                flex_direction: UFlexDirection::Row,
                justify_content: UJustifyContent::Center,
                align_items: UAlignItems::Center,
                gap: 28.0,
                ..default()
            },
        ))
        .id();
    targets.0.insert("root".into(), root);

    // ---------------- 左：控制面板 ----------------
    let panel = commands
        .spawn((
            ChildOf(root),
            UPanel::glass().with_gap(18.0),
            UNode {
                width: UVal::Px(430.0),
                padding: USides::all(28.0),
                ..default()
            },
            ULayout {
                display: UDisplay::Flex,
                flex_direction: UFlexDirection::Column,
                gap: 18.0,
                ..default()
            },
        ))
        .id();

    let title = commands
        .spawn((
            ChildOf(panel),
            label(&font, "Needle 控制台", 26.0, Color::WHITE),
            Name::new("title"),
        ))
        .id();
    targets.0.insert("title".into(), title);
    let status = commands
        .spawn((
            ChildOf(panel),
            label(&font, "就绪 · 输入英文指令后回车", 15.0, Color::srgb(0.55, 0.75, 0.95)),
            Name::new("status"),
            StatusLabel,
        ))
        .id();
    targets.0.insert("status".into(), status);

    // 设置子面板
    let settings = commands
        .spawn((
            ChildOf(panel),
            UNode {
                ..default()
            },
            ULayout {
                display: UDisplay::Flex,
                flex_direction: UFlexDirection::Column,
                gap: 14.0,
                ..default()
            },
            Name::new("settings_panel"),
        ))
        .id();
    targets.0.insert("settings_panel".into(), settings);

    // 主题选择
    let theme_row = row(&mut commands, settings);
    commands.spawn((ChildOf(theme_row), label(&font, "主题", 16.0, Color::srgb(0.8, 0.85, 0.92))));
    let theme_select = commands
        .spawn((
            ChildOf(theme_row),
            USelect::new()
                .with_options(vec![
                    USelectOption::new("Ocean 海洋", "ocean"),
                    USelectOption::new("Sunset 落日", "sunset"),
                    USelectOption::new("Forest 森林", "forest"),
                    USelectOption::new("Mono 黑白", "mono"),
                ])
                .with_selected_index(0)
                .with_size(230.0, 38.0),
            Name::new("theme_select"),
        ))
        .id();
    targets.0.insert("theme_select".into(), theme_select);

    // 开关行
    let toggle_row = row(&mut commands, settings);
    commands.spawn((ChildOf(toggle_row), label(&font, "动画", 16.0, Color::srgb(0.8, 0.85, 0.92))));
    let anim_toggle = commands
        .spawn((
            ChildOf(toggle_row),
            UToggle::ios_style().with_checked(true),
            Name::new("anim_toggle"),
        ))
        .id();
    targets.0.insert("anim_toggle".into(), anim_toggle);
    commands.spawn((ChildOf(toggle_row), label(&font, "音效", 16.0, Color::srgb(0.8, 0.85, 0.92))));
    let sound_toggle = commands
        .spawn((
            ChildOf(toggle_row),
            UToggle::ios_style().with_checked(false),
            Name::new("sound_toggle"),
        ))
        .id();
    targets.0.insert("sound_toggle".into(), sound_toggle);

    // 音量
    let volume_row = row(&mut commands, settings);
    commands.spawn((ChildOf(volume_row), label(&font, "音量", 16.0, Color::srgb(0.8, 0.85, 0.92))));
    let volume_seekbar = commands
        .spawn((
            ChildOf(volume_row),
            USeekBar::new().with_value(0.45),
            Name::new("volume_seekbar"),
        ))
        .id();
    targets.0.insert("volume_seekbar".into(), volume_seekbar);

    // 亮度
    let bright_row = row(&mut commands, settings);
    commands.spawn((ChildOf(bright_row), label(&font, "亮度", 16.0, Color::srgb(0.8, 0.85, 0.92))));
    let brightness = commands
        .spawn((
            ChildOf(bright_row),
            UProgressBar {
                value: 0.7,
                ..default()
            },
            Name::new("brightness_progress"),
        ))
        .id();
    targets.0.insert("brightness_progress".into(), brightness);

    // 按钮行
    let button_row = row(&mut commands, settings);
    let reset_button = commands
        .spawn((ChildOf(button_row), UButton::primary(), Name::new("reset_button")))
        .with_children(|b| {
            b.spawn(label(&font, "重置", 15.0, Color::WHITE));
        })
        .id();
    targets.0.insert("reset_button".into(), reset_button);
    let about_button = commands
        .spawn((ChildOf(button_row), UButton::secondary(), Name::new("about_button")))
        .with_children(|b| {
            b.spawn(label(&font, "关于", 15.0, Color::WHITE));
        })
        .id();
    targets.0.insert("about_button".into(), about_button);

    // ---------------- 右：对话面板 ----------------
    let chat = commands
        .spawn((
            ChildOf(root),
            UPanel::glass().with_gap(14.0),
            UNode {
                width: UVal::Px(560.0),
                height: UVal::Px(560.0),
                padding: USides::all(24.0),
                ..default()
            },
            ULayout {
                display: UDisplay::Flex,
                flex_direction: UFlexDirection::Column,
                gap: 14.0,
                ..default()
            },
        ))
        .id();

    commands.spawn((
        ChildOf(chat),
        label(&font, "（暂无消息）", 15.0, Color::srgb(0.88, 0.92, 0.97)),
        ChatHistoryLabel,
    ));

    let input_wrap = commands
        .spawn((
            ChildOf(chat),
            UNode {
                background_color: Color::srgba(0.0, 0.0, 0.0, 0.35),
                border_radius: UCornerRadius::all(10.0),
                padding: USides::axes(14.0, 10.0),
                ..default()
            },
            ULayout {
                display: UDisplay::Flex,
                align_items: UAlignItems::Center,
                ..default()
            },
        ))
        .id();
    commands.spawn((ChildOf(input_wrap), label(&font, "> _", 16.0, Color::srgb(0.65, 0.95, 0.7)), InputLineLabel));

    commands.spawn((
        ChildOf(chat),
        label(
            &font,
            "试一试: set volume to 80 / turn off animation / switch theme to sunset\nset brightness to 20 / change the title to hello / press the reset button / reset the ui",
            13.0,
            Color::srgb(0.55, 0.60, 0.68),
        ),
    ));

    targets.0.insert("chat_panel".into(), chat);
}

fn row(commands: &mut Commands, parent: Entity) -> Entity {
    commands
        .spawn((
            ChildOf(parent),
            UNode::default(),
            ULayout {
                display: UDisplay::Flex,
                flex_direction: UFlexDirection::Row,
                align_items: UAlignItems::Center,
                gap: 14.0,
                ..default()
            },
        ))
        .id()
}

// ---------------------------------------------------------------------------
// Agent / 工具注册
// ---------------------------------------------------------------------------

fn setup_agent(world: &mut World) {
    let world = world;
    let mk_tool = |world: &mut World, name: &str, description: &str, params: serde_json::Value| -> Entity {
        world
            .spawn(ToolBundle::new(ToolSpec::new(name, description, params)))
            .id()
    };

    let theme_tool = mk_tool(
        world,
        "set_theme",
        "Change the console theme and background color.",
        ParametersBuilder::new()
            .str_enum("theme", &["ocean", "sunset", "forest", "mono"], "theme name")
            .build(),
    );
    let volume_tool = mk_tool(
        world,
        "set_volume",
        "Set the volume slider percentage.",
        ParametersBuilder::new()
            .int_range("percent", 0, 100, "volume percent")
            .build(),
    );
    let brightness_tool = mk_tool(
        world,
        "set_brightness",
        "Set the brightness bar percentage.",
        ParametersBuilder::new()
            .int_range("percent", 0, 100, "brightness percent")
            .build(),
    );
    let animation_on_tool = mk_tool(
        world,
        "animation_on",
        "Turn the animation toggle ON.",
        ParametersBuilder::new().build(),
    );
    let animation_off_tool = mk_tool(
        world,
        "animation_off",
        "Turn the animation toggle OFF.",
        ParametersBuilder::new().build(),
    );
    let title_tool = mk_tool(
        world,
        "set_title",
        "Rename the title text. Use when the user asks to change, set or rename the title.",
        ParametersBuilder::new()
            .string("text", "new title text")
            .build(),
    );
    let color_tool = mk_tool(
        world,
        "set_title_color",
        "Change the title text color.",
        ParametersBuilder::new()
            .str_enum(
                "color",
                &["blue", "green", "red", "white", "amber"],
                "color name",
            )
            .build(),
    );

    // 纯函数 handler：校验 + 归一化，真实 UI 效果由 ECS 效果系统施加
    register_tool_handler(world, "set_theme", |call| {
        Ok(ToolOutput::json(json!({
            "theme": call.args.get("theme").cloned().unwrap_or(json!("mono")),
        })))
    });
    register_tool_handler(world, "set_volume", |call| {
        let percent = call.args.get("percent").and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(ToolOutput::json(json!({ "percent": percent })))
    });
    register_tool_handler(world, "set_brightness", |call| {
        let percent = call.args.get("percent").and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(ToolOutput::json(json!({ "percent": percent })))
    });
    register_tool_handler(world, "animation_on", |_call| {
        Ok(ToolOutput::json(json!({ "on": true })))
    });
    register_tool_handler(world, "animation_off", |_call| {
        Ok(ToolOutput::json(json!({ "on": false })))
    });
    register_tool_handler(world, "set_title", |call| {
        Ok(ToolOutput::json(json!({
            "text": call.args.get("text").cloned().unwrap_or(json!("")),
        })))
    });
    register_tool_handler(world, "set_title_color", |call| {
        Ok(ToolOutput::json(json!({
            "color": call.args.get("color").cloned().unwrap_or(json!("white")),
        })))
    });

    // NOTE: 实测本引擎（2.0.3）的正确调用 confidence 波动极大（0.0002 ~ 0.9），
    // 固定门限会误拦好调用 —— 与 bevy_needle2 的实测一致。默认不设门限；
    // 门控机制保留在插件里，按产品实测校准后再启用。
    let handles = spawn_agent(
        world,
        NeedleAgentSpec::new("console-agent")
            .with_system_facts("device: desktop; app: univis console; locale: en-US")
            .with_max_steps(6),
    );
    for tool in [
        theme_tool,
        volume_tool,
        brightness_tool,
        animation_on_tool,
        animation_off_tool,
        title_tool,
        color_tool,
    ] {
        attach_tool(world, handles.agent, tool).expect("attach tool");
    }
    world.insert_resource(ConsoleAgent(handles.agent));
}

// ---------------------------------------------------------------------------
// 自动驾驶（截图/无头调试）
// ---------------------------------------------------------------------------

/// 终局状态转储：读出全部目标实体的真实 ECS 状态（区分"ECS 没变"与"视觉没跟上"）。
fn dump_ui_state(
    enabled: Option<Res<AutoPilotEnabled>>,
    pilot: Option<Res<AutoPilot>>,
    targets: Res<UiTargets>,
    mut toggles: Query<&mut UToggle>,
    mut seeks: Query<&mut USeekBar>,
    mut bars: Query<&mut UProgressBar>,
    mut selects: Query<&mut USelect>,
    mut labels: Query<&mut UTextLabel>,
    mut nodes: Query<&mut UNode>,
) {
    // 取消可变性的简单方法：用不可变查询读取（这里为了触发 flush 顺便保持简单）
    let Some(_) = enabled else { return };
    let Some(pilot) = pilot else { return };
    if pilot.frame != 1750 {
        return;
    }
    let get = |key: &str| targets.0.get(key).copied();
    info!("=== 终局状态转储 ===");
    if let Some(e) = get("anim_toggle")
        && let Ok(t) = toggles.get(e)
    {
        info!("anim_toggle: checked={} offset={:.3}", t.checked, t.current_offset);
    }
    if let Some(e) = get("sound_toggle")
        && let Ok(t) = toggles.get(e)
    {
        info!("sound_toggle: checked={} offset={:.3}", t.checked, t.current_offset);
    }
    if let Some(e) = get("volume_seekbar")
        && let Ok(sb) = seeks.get(e)
    {
        info!("volume_seekbar: value={:.3} range=[{},{}]", sb.value, sb.min_value, sb.max_value);
    }
    if let Some(e) = get("brightness_progress")
        && let Ok(b) = bars.get(e)
    {
        info!("brightness_progress: value={:.3}", b.value);
    }
    if let Some(e) = get("theme_select")
        && let Ok(sel) = selects.get(e)
    {
        info!("theme_select: selected_index={:?} value={:?}", sel.selected_index, sel.options.get(sel.selected_index.unwrap_or(0)).map(|o| &o.value));
    }
    if let Some(e) = get("title")
        && let Ok(l) = labels.get(e)
    {
        info!("title: text={:?} color={:?}", l.text, l.color);
    }
    if let Some(e) = get("root")
        && let Ok(n) = nodes.get(e)
    {
        info!("root: background={:?}", n.background_color);
    }
}

fn setup_autopilot(mut commands: Commands) {
    match parse_autopilot() {
        Some(pilot) => {
            commands.insert_resource(pilot);
            commands.insert_resource(AutoPilotEnabled);
            info!("自动驾驶已启用（BEVY_NEEDLE_AUTOPILOT=1）");
        }
        None => {}
    }
    if std::env::var("BEVY_NEEDLE_DIAG").is_ok_and(|v| v == "1") {
        commands.insert_resource(DiagEnabled);
    }
}

/// 帧诊断开关（BEVY_NEEDLE_DIAG=1 时启用脏写统计输出）。
#[derive(Resource)]
struct DiagEnabled;
fn diagnose_frame_changes(
    diag_enabled: Option<Res<DiagEnabled>>,
    enabled: Option<Res<AutoPilotEnabled>>,
    labels: Query<(Entity, &Name), Changed<UTextLabel>>,
    toggles: Query<(Entity, &Name), Changed<UToggle>>,
    nodes: Query<(Entity, &Name), Changed<UNode>>,
) {
    if diag_enabled.is_none() || enabled.is_none() {
        return;
    }
    let mut parts = Vec::new();
    for (e, name) in &labels {
        parts.push(format!("label {:?} {}", e, name.as_str()));
    }
    for (e, name) in &toggles {
        parts.push(format!("toggle {:?} {}", e, name.as_str()));
    }
    for (e, name) in &nodes {
        parts.push(format!("node {:?} {}", e, name.as_str()));
    }
    if !parts.is_empty() {
        info!("[diag] {}", parts.join(" | "));
    }
}

fn run_autopilot(
    mut commands: Commands,
    enabled: Option<Res<AutoPilotEnabled>>,
    mut pilot: Option<ResMut<AutoPilot>>,
    agent: Option<Res<ConsoleAgent>>,
    mut runs: MessageWriter<RunAgent>,
    mut log: ResMut<ChatLog>,
) {
    let Some(_enabled) = enabled else {
        return;
    };
    let Some(pilot) = pilot.as_deref_mut() else {
        return;
    };
    let Some(agent) = agent else {
        return;
    };

    pilot.frame += 1;
    let frame = pilot.frame;

    // 到点触发截图
    let wants_shot = pilot
        .steps
        .iter()
        .any(|(at, text)| *at == frame && text.is_none());

    while let Some((at, _text)) = pilot.steps.front() {
        if *at > frame {
            break;
        }
        let (_, text) = pilot.steps.pop_front().unwrap();
        let Some(text) = text else { continue };
        log.push(format!("你: {}", text));
        runs.write(RunAgent::new(agent.0, text.clone()));
        info!("自动驾驶发送指令: {text}");
    }

    if wants_shot {
        let path = format!(
            "examples/univis_needle_demo/screenshots/autopilot_{}.png",
            pilot.shot_index
        );
        pilot.shot_index += 1;
        info!("截图 -> {path}");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
}

// ---------------------------------------------------------------------------
// 输入
// ---------------------------------------------------------------------------

fn capture_keyboard(
    mut keys: MessageReader<KeyboardInput>,
    mut ime: MessageReader<Ime>,
    mut buffer: ResMut<InputBuffer>,
    mut log: ResMut<ChatLog>,
    mut status: ResMut<StatusLine>,
    agent: Option<Res<ConsoleAgent>>,
    mut runs: MessageWriter<RunAgent>,
) {
    // IME 提交（中文等）
    for event in ime.read() {
        if let Ime::Commit { value, .. } = event {
            buffer.0.push_str(value.as_str());
        }
    }

    for event in keys.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        match event.key_code {
            KeyCode::Enter => {
                let prompt = buffer.0.trim().to_string();
                if prompt.is_empty() {
                    continue;
                }
                log.push(format!("你: {prompt}"));
                if let Some(agent) = agent.as_ref() {
                    runs.write(RunAgent::new(agent.0, prompt));
                    status.0 = "Needle 思考中…".into();
                } else {
                    log.push("[系统] agent 尚未就绪".to_string());
                }
                buffer.0.clear();
            }
            KeyCode::Backspace => {
                buffer.0.pop();
            }
            KeyCode::Escape => {
                buffer.0.clear();
            }
            _ => {
                if let Some(text) = event.text.as_ref() {
                    for ch in text.chars() {
                        if !ch.is_control() {
                            buffer.0.push(ch);
                        }
                    }
                }
            }
        }
    }
}

fn sync_input_line(buffer: Res<InputBuffer>, mut labels: Query<&mut UTextLabel, With<InputLineLabel>>) {
    if buffer.is_changed() {
        for mut text_label in &mut labels {
            let cursor = if buffer.0.is_empty() { "_" } else { "▏" };
            text_label.text = format!("> {}{cursor}", buffer.0);
        }
    }
}

fn sync_status_line(status: Res<StatusLine>, mut labels: Query<&mut UTextLabel, With<StatusLabel>>) {
    if status.is_changed() && !status.0.is_empty() {
        for mut text_label in &mut labels {
            text_label.text = status.0.clone();
        }
    }
}

fn sync_chat_log(log: Res<ChatLog>, mut labels: Query<&mut UTextLabel, With<ChatHistoryLabel>>) {
    if log.is_changed() {
        for mut text_label in &mut labels {
            text_label.text = log.text();
        }
    }
}

// ---------------------------------------------------------------------------
// 物理按钮点击（真实指针交互）
// ---------------------------------------------------------------------------

fn poll_button_clicks(
    interactions: Query<(Entity, &Name, &UInteraction), Changed<UInteraction>>,
    mut actions: MessageWriter<UiActionRequest>,
    mut log: ResMut<ChatLog>,
) {
    for (_, name, interaction) in &interactions {
        if *interaction != UInteraction::Clicked {
            continue;
        }
        match name.as_str() {
            "reset_button" => {
                actions.write(UiActionRequest { action: "reset" });
                log.push("🖱 按下「重置」");
            }
            "about_button" => {
                actions.write(UiActionRequest { action: "about" });
                log.push("🖱 按下「关于」");
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 控件事件回显（真实 UI 交互 → 控制台）
// ---------------------------------------------------------------------------

fn poll_widget_events(
    mut toggles: MessageReader<ToggleChangedEvent>,
    mut selects: MessageReader<SelectChangedEvent>,
    mut seekbars: MessageReader<SeekBarChangedEvent>,
    mut log: ResMut<ChatLog>,
) {
    for event in toggles.read() {
        log.push(format!("[UI] 开关 {} → {}", event.entity, if event.checked { "on" } else { "off" }));
    }
    for event in selects.read() {
        log.push(format!("[UI] 主题选择 → {} ({})", event.label, event.value));
    }
    for event in seekbars.read() {
        log.push(format!("[UI] 滑杆 → {:.2}", event.value));
    }
}

// ---------------------------------------------------------------------------
// 工具效果：模型调用 → univis_ui 组件突变
// ---------------------------------------------------------------------------

fn color_from_name(name: &str) -> Color {
    match name {
        "blue" => Color::srgb(0.35, 0.65, 1.0),
        "green" => Color::srgb(0.35, 0.9, 0.5),
        "red" => Color::srgb(1.0, 0.4, 0.4),
        "amber" => Color::srgb(1.0, 0.75, 0.3),
        _ => Color::WHITE,
    }
}

fn theme_background(theme: &str) -> Color {
    match theme {
        "sunset" => Color::srgb(0.14, 0.07, 0.05),
        "forest" => Color::srgb(0.04, 0.10, 0.06),
        "mono" => Color::srgb(0.08, 0.08, 0.08),
        _ => Color::srgb(0.05, 0.07, 0.10), // ocean
    }
}

fn theme_accent(theme: &str) -> Color {
    match theme {
        "sunset" => Color::srgb(1.0, 0.6, 0.35),
        "forest" => Color::srgb(0.5, 0.95, 0.6),
        "mono" => Color::srgb(0.9, 0.9, 0.9),
        _ => Color::srgb(0.4, 0.75, 1.0),
    }
}

fn apply_tool_effects(
    mut completed: MessageReader<ToolCallCompleted>,
    mut actions: MessageReader<UiActionRequest>,
    targets: Res<UiTargets>,
    mut theme: ResMut<CurrentTheme>,
    mut nodes: Query<&mut UNode>,
    mut labels: Query<&mut UTextLabel>,
    mut toggles: Query<&mut UToggle>,
    mut seekbars: Query<&mut USeekBar>,
    mut progress: Query<&mut UProgressBar>,
    mut selects: Query<&mut USelect>,
    mut log: ResMut<ChatLog>,
) {
    // 真实按钮/工具触发的本地动作
    for action in actions.read() {
        match action.action {
            "reset" => {
                reset_ui_defaults(
                    &targets, &mut theme, &mut nodes, &mut labels, &mut toggles, &mut seekbars,
                    &mut progress, &mut selects,
                );
                log.push("↺ 已恢复默认设置");
            }
            "about" => {
                log.push("bevy_needle 演示 · Needle 2 (45M 本地模型) × univis_ui (Bevy 0.19)");
            }
            _ => {}
        }
    }

    // 模型工具调用
    for message in completed.read() {
        let name = message.call.name.as_str();
        let args = &message.call.args;
        // 统一的 target 解析：未注册/未命中给 warn，绝不静默失败
        let resolve_target = |targets: &UiTargets, key: &str| -> Option<Entity> {
            match targets.0.get(key) {
                Some(entity) => Some(*entity),
                None => {
                    warn!("[effect] target 未注册: {key:?}（工具 {name} 已回执成功但无处生效）");
                    None
                }
            }
        };

        let detail = match name {
            "set_theme" => {
                let theme_name = args.get("theme").and_then(|v| v.as_str()).unwrap_or("ocean");
                let bg = theme_background(theme_name);
                if let Some(root) = resolve_target(&targets, "root")
                    && let Ok(mut node) = nodes.get_mut(root)
                {
                    node.background_color = bg;
                }
                *theme = CurrentTheme(match theme_name {
                    "sunset" => "sunset",
                    "forest" => "forest",
                    "mono" => "mono",
                    _ => "ocean",
                });
                if let Some(title) = resolve_target(&targets, "title")
                    && let Ok(mut text_label) = labels.get_mut(title)
                {
                    text_label.color = theme_accent(theme.name_static());
                }
                // 同步下拉选择器的显示，避免"主题已换但选择器还显示旧值"
                if let Some(select_entity) = resolve_target(&targets, "theme_select")
                    && let Ok(mut select) = selects.get_mut(select_entity)
                    && let Some(index) = select
                        .options
                        .iter()
                        .position(|o| o.value == theme_name)
                {
                    select.selected_index = Some(index);
                }
                format!("主题 → {theme_name}")
            }
            "set_volume" => {
                let percent = args.get("percent").and_then(|v| v.as_i64()).unwrap_or(0);
                if let Some(entity) = resolve_target(&targets, "volume_seekbar")
                    && let Ok(mut seekbar) = seekbars.get_mut(entity)
                {
                    seekbar.value = (percent as f32 / 100.0).clamp(0.0, 1.0);
                }
                format!("音量 → {percent}%")
            }
            "set_brightness" => {
                let percent = args.get("percent").and_then(|v| v.as_i64()).unwrap_or(0);
                if let Some(entity) = resolve_target(&targets, "brightness_progress")
                    && let Ok(mut bar) = progress.get_mut(entity)
                {
                    bar.value = (percent as f32 / 100.0).clamp(0.0, 1.0);
                }
                format!("亮度 → {percent}%")
            }
            "animation_on" | "animation_off" => {
                let on = name == "animation_on";
                if let Some(entity) = resolve_target(&targets, "anim_toggle")
                    && let Ok(mut toggle) = toggles.get_mut(entity)
                {
                    toggle.checked = on;
                    // 读回探针：确认 ECS 状态真的翻转了（区分"ECS没变" vs "视觉没跟上"）
                    if let Ok(readback) = toggles.get(entity) {
                        info!("[verify] anim_toggle.checked={} offset={:.3}", readback.checked, readback.current_offset);
                    }
                }
                format!("动画 → {}", if on { "on" } else { "off" })
            }
            "set_title" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(entity) = resolve_target(&targets, "title")
                    && let Ok(mut text_label) = labels.get_mut(entity)
                {
                    text_label.text = text.to_string();
                }
                format!("标题 → {text:?}")
            }
            "set_title_color" => {
                let color_name = args.get("color").and_then(|v| v.as_str()).unwrap_or("white");
                if let Some(title) = resolve_target(&targets, "title")
                    && let Ok(mut text_label) = labels.get_mut(title)
                {
                    text_label.color = color_from_name(color_name);
                }
                format!("标题颜色 → {color_name}")
            }
            other => format!("（未实现的工具 {other}）"),
        };
        log.push(format!("→ {detail} ✓"));
        info!("[effect] {name} -> {detail}");
    }
}

#[allow(clippy::too_many_arguments)]
fn reset_ui_defaults(
    targets: &UiTargets,
    theme: &mut CurrentTheme,
    nodes: &mut Query<&mut UNode>,
    labels: &mut Query<&mut UTextLabel>,
    toggles: &mut Query<&mut UToggle>,
    seekbars: &mut Query<&mut USeekBar>,
    progress: &mut Query<&mut UProgressBar>,
    selects: &mut Query<&mut USelect>,
) {
    *theme = CurrentTheme("ocean");
    if let Some(root) = targets.0.get("root") {
        if let Ok(mut node) = nodes.get_mut(*root) {
            node.background_color = theme_background("ocean");
        }
    }
    if let Some(title) = targets.0.get("title") {
        if let Ok(mut text_label) = labels.get_mut(*title) {
            text_label.text = "Needle 控制台".into();
            text_label.color = theme_accent("ocean");
        }
    }
    for target in ["anim_toggle", "sound_toggle"] {
        if let Some(entity) = targets.0.get(target) {
            if let Ok(mut toggle) = toggles.get_mut(*entity) {
                toggle.checked = target == "anim_toggle";
            }
        }
    }
    if let Some(entity) = targets.0.get("volume_seekbar") {
        if let Ok(mut seekbar) = seekbars.get_mut(*entity) {
            seekbar.value = 0.45;
        }
    }
    if let Some(entity) = targets.0.get("brightness_progress") {
        if let Ok(mut bar) = progress.get_mut(*entity) {
            bar.value = 0.7;
        }
    }
    if let Some(entity) = targets.0.get("theme_select") {
        if let Ok(mut select) = selects.get_mut(*entity) {
            select.selected_index = Some(0);
        }
    }
}

impl CurrentTheme {
    fn name_static(&self) -> &'static str {
        self.0
    }
}

fn log_tool_failures(mut failed: MessageReader<ToolCallFailed>, mut log: ResMut<ChatLog>) {
    for message in failed.read() {
        log.push(format!("→ {} 失败: {}", message.call.name, message.error));
    }
}

// ---------------------------------------------------------------------------
// run 结果回显
// ---------------------------------------------------------------------------

fn report_run_outcomes(
    mut committed: MessageReader<RunCommitted>,
    mut failed: MessageReader<RunFailed>,
    mut escalations: MessageReader<RunEscalation>,
    runs: Query<(Entity, Option<&RunResultText>, Option<&RunLastResponse>)>,
    mut log: ResMut<ChatLog>,
    mut status: ResMut<StatusLine>,
) {
    for escalation in escalations.read() {
        log.push(format!(
            "⚠ 置信度 {:.2} 低于门限 {:.2}，已按约定升级（未执行）",
            escalation.confidence, escalation.threshold
        ));
    }
    for message in failed.read() {
        if let Some(run) = message.run {
            log.push(format!("✗ 失败: {}", message.error));
            if let Ok((_, _, last)) = runs.get(run) {
                if let Some(conf) = last.and_then(|l| l.0.as_ref()).and_then(|v| v.get("confidence")).and_then(|c| c.as_f64()) {
                    status.0 = format!("失败（confidence {conf:.2}）");
                }
            }
        }
    }
    for message in committed.read() {
        if let Ok((_, text, last)) = runs.get(message.run) {
            let answer = text.map(|t| t.0.clone()).unwrap_or_default();
            let confidence = last
                .and_then(|l| l.0.as_ref())
                .and_then(|v| v.get("confidence"))
                .and_then(|c| c.as_f64());
            log.push(format!(
                "Needle: {answer} (conf {})",
                confidence
                    .map(|c| format!("{c:.2}"))
                    .unwrap_or_else(|| "n/a".into())
            ));
            status.0 = match confidence {
                Some(c) => format!("完成 · confidence {c:.2}"),
                None => "完成".into(),
            };
        }
    }
}
