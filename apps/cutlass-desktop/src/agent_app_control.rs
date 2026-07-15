//! Synchronous, bounded app-control tools for the desktop agent.
//!
//! Calls originate on the agent thread. Argument parsing and settings IO stay
//! there; every Slint/window access is dispatched to the UI event loop and
//! acknowledged over a bounded channel.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, bounded};
use cutlass_ai::{HostToolSpec, ToolOutput, ToolTier};
use serde_json::{Map, Value, json};
use slint::{ComponentHandle, Model, SharedString};

const APP_STATE: &str = "app_state";
const APP_PLAYBACK: &str = "app_playback";
const APP_SEEK: &str = "app_seek";
const APP_LOOP_RANGE: &str = "app_loop_range";
const APP_TIMELINE_ZOOM: &str = "app_timeline_zoom";
const APP_SELECT_CLIP: &str = "app_select_clip";
const APP_PANEL: &str = "app_panel";
const APP_THEME: &str = "app_theme";
const APP_WINDOW: &str = "app_window";
const APP_WINDOW_MOVE: &str = "app_window_move";
const APP_WINDOW_RESIZE: &str = "app_window_resize";
const APP_CLOSE: &str = "app_close";

const TOOL_NAMES: [&str; 12] = [
    APP_STATE,
    APP_PLAYBACK,
    APP_SEEK,
    APP_LOOP_RANGE,
    APP_TIMELINE_ZOOM,
    APP_SELECT_CLIP,
    APP_PANEL,
    APP_THEME,
    APP_WINDOW,
    APP_WINDOW_MOVE,
    APP_WINDOW_RESIZE,
    APP_CLOSE,
];

const MIN_ZOOM: f64 = 0.05;
const MAX_ZOOM: f64 = 40.0;
const MIN_WINDOW_WIDTH: i64 = 640;
const MAX_WINDOW_WIDTH: i64 = 7_680;
const MIN_WINDOW_HEIGHT: i64 = 480;
const MAX_WINDOW_HEIGHT: i64 = 4_320;
const MIN_WINDOW_COORD: i64 = -100_000;
const MAX_WINDOW_COORD: i64 = 100_000;

const UI_WAIT_SLICE: Duration = Duration::from_millis(25);
const UI_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

type UiResponse = Result<Value, String>;

/// The complete Phase 2a desktop-control registry.
pub fn specs() -> Vec<HostToolSpec> {
    vec![
        spec(
            APP_STATE,
            "Read compact desktop state: transport, playhead, timeline selection and zoom, theme, panels, dialogs, and physical window state.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {},
                "required": []
            }),
            ToolTier::ReadOnly,
        ),
        spec(
            APP_PLAYBACK,
            "Control timeline playback. Stop pauses and seeks to the start.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["play", "pause", "toggle", "stop"],
                        "description": "Playback action."
                    }
                },
                "required": ["action"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_SEEK,
            "Seek the timeline playhead to a non-negative time in seconds; the app clamps it to the sequence.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "seconds": {
                        "type": "number",
                        "minimum": 0,
                        "description": "Target time in seconds."
                    }
                },
                "required": ["seconds"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_LOOP_RANGE,
            "Set, enable, disable, or clear the timeline loop range. Setting a range enables looping and requires start_seconds < end_seconds within the sequence.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["set", "enable", "disable", "clear"],
                        "description": "Loop-range action."
                    },
                    "start_seconds": {
                        "type": "number",
                        "minimum": 0,
                        "description": "Range start, required only for set."
                    },
                    "end_seconds": {
                        "type": "number",
                        "minimum": 0,
                        "description": "Exclusive range end, required only for set."
                    }
                },
                "required": ["action"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_TIMELINE_ZOOM,
            "Set, step, or fit timeline zoom while preserving the app's normal zoom anchoring.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["set", "in", "out", "fit"],
                        "description": "Timeline zoom action."
                    },
                    "value": {
                        "type": "number",
                        "minimum": MIN_ZOOM,
                        "maximum": MAX_ZOOM,
                        "description": "Zoom value required only when action is set."
                    }
                },
                "required": ["action"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_SELECT_CLIP,
            "Select, toggle, or clear timeline clip selection using normal linkage rules. Locked tracks cannot be selected.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["select", "toggle", "clear"],
                        "description": "Selection action."
                    },
                    "clip_id": {
                        "oneOf": [
                            { "type": "string", "minLength": 1 },
                            { "type": "integer", "minimum": 0 }
                        ],
                        "description": "Clip id required for select or toggle."
                    }
                },
                "required": ["action"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_PANEL,
            "Show, hide, or toggle a reversible editor panel or dialog.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "panel": {
                        "type": "string",
                        "enum": ["library", "inspector", "assistant", "timeline", "preview", "settings", "export", "canvas"],
                        "description": "Panel or dialog to control."
                    },
                    "action": {
                        "type": "string",
                        "enum": ["show", "hide", "toggle"],
                        "description": "Visibility action."
                    }
                },
                "required": ["panel", "action"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_THEME,
            "Apply a bundled theme immediately and persist it without replacing malformed settings.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "theme": {
                        "type": "string",
                        "enum": ["default", "ember", "dark-blue"],
                        "description": "Bundled theme key."
                    }
                },
                "required": ["theme"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_WINDOW,
            "Request a native window state change.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["minimize", "restore", "maximize", "fullscreen", "exit-fullscreen"],
                        "description": "Window state action."
                    }
                },
                "required": ["action"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_WINDOW_MOVE,
            "Request a physical-pixel window move; negative coordinates support monitors left or above the primary display.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "x": {
                        "type": "integer",
                        "minimum": MIN_WINDOW_COORD,
                        "maximum": MAX_WINDOW_COORD,
                        "description": "Physical screen x coordinate."
                    },
                    "y": {
                        "type": "integer",
                        "minimum": MIN_WINDOW_COORD,
                        "maximum": MAX_WINDOW_COORD,
                        "description": "Physical screen y coordinate."
                    }
                },
                "required": ["x", "y"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_WINDOW_RESIZE,
            "Request a physical-pixel window size within desktop-safe bounds.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "width": {
                        "type": "integer",
                        "minimum": MIN_WINDOW_WIDTH,
                        "maximum": MAX_WINDOW_WIDTH,
                        "description": "Physical content width."
                    },
                    "height": {
                        "type": "integer",
                        "minimum": MIN_WINDOW_HEIGHT,
                        "maximum": MAX_WINDOW_HEIGHT,
                        "description": "Physical content height."
                    }
                },
                "required": ["width", "height"]
            }),
            ToolTier::Workspace,
        ),
        spec(
            APP_CLOSE,
            "Request Cutlass's context-aware close action: return to the launch gallery from an editor session, or quit from the gallery.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {},
                "required": []
            }),
            ToolTier::System,
        ),
    ]
}

/// Parse and execute one registered app-control tool.
///
/// This function is called from the agent thread. It never upgrades the Slint
/// weak handle or reads a generated global until the event-loop closure runs.
pub fn call(
    app: slint::Weak<crate::AppWindow>,
    name: &str,
    arguments: &Value,
    cancel: &AtomicBool,
) -> Result<ToolOutput, String> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled before the app-control tool could run".into());
    }

    let request = parse_request(name, arguments)?;

    // Theme IO deliberately precedes event-loop dispatch. A malformed config
    // fails closed, and cutlass-settings' format-preserving save protects
    // comments and unknown/newer keys. Once the save succeeds, finish the
    // matching live update even if cancellation arrives between the two
    // operations, so persisted and visible state cannot diverge.
    if let Request::Theme(theme) = &request {
        persist_theme_at(&cutlass_settings::default_config_path(), *theme)?;
        let finish_live_update = AtomicBool::new(false);
        return dispatch_ui(app, &finish_live_update, move |app| {
            execute_ui(&app, request)
        });
    }

    dispatch_ui(app, cancel, move |app| execute_ui(&app, request))
}

fn spec(name: &str, description: &str, parameters: Value, tier: ToolTier) -> HostToolSpec {
    HostToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
        tier,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Request {
    State,
    Playback(PlaybackAction),
    Seek(f64),
    LoopRange(LoopRangeAction),
    TimelineZoom(ZoomAction),
    SelectClip(SelectionAction),
    Panel {
        panel: Panel,
        action: VisibilityAction,
    },
    Theme(cutlass_settings::ThemeChoice),
    Window(WindowAction),
    WindowMove {
        x: i32,
        y: i32,
    },
    WindowResize {
        width: u32,
        height: u32,
    },
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackAction {
    Play,
    Pause,
    Toggle,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LoopRangeAction {
    Set {
        start_seconds: f64,
        end_seconds: f64,
    },
    Enable,
    Disable,
    Clear,
}

impl LoopRangeAction {
    fn key(self) -> &'static str {
        match self {
            Self::Set { .. } => "set",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Clear => "clear",
        }
    }
}

impl PlaybackAction {
    fn key(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Toggle => "toggle",
            Self::Stop => "stop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ZoomAction {
    Set(f32),
    In,
    Out,
    Fit,
}

impl ZoomAction {
    fn key(self) -> &'static str {
        match self {
            Self::Set(_) => "set",
            Self::In => "in",
            Self::Out => "out",
            Self::Fit => "fit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectionAction {
    Select(String),
    Toggle(String),
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Library,
    Inspector,
    Assistant,
    Timeline,
    Preview,
    Settings,
    Export,
    Canvas,
}

impl Panel {
    fn key(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Inspector => "inspector",
            Self::Assistant => "assistant",
            Self::Timeline => "timeline",
            Self::Preview => "preview",
            Self::Settings => "settings",
            Self::Export => "export",
            Self::Canvas => "canvas",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisibilityAction {
    Show,
    Hide,
    Toggle,
}

impl VisibilityAction {
    fn apply(self, current: bool) -> bool {
        match self {
            Self::Show => true,
            Self::Hide => false,
            Self::Toggle => !current,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowAction {
    Minimize,
    Restore,
    Maximize,
    Fullscreen,
    ExitFullscreen,
}

impl WindowAction {
    fn key(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Restore => "restore",
            Self::Maximize => "maximize",
            Self::Fullscreen => "fullscreen",
            Self::ExitFullscreen => "exit-fullscreen",
        }
    }
}

fn parse_request(name: &str, arguments: &Value) -> Result<Request, String> {
    if !TOOL_NAMES.contains(&name) {
        return Err(format!("unknown app-control tool '{name}'"));
    }
    let object = arguments
        .as_object()
        .ok_or_else(|| format!("{name} arguments must be an object"))?;

    match name {
        APP_STATE => {
            validate_fields(name, object, &[], &[])?;
            Ok(Request::State)
        }
        APP_PLAYBACK => {
            validate_fields(name, object, &["action"], &["action"])?;
            let action = match string_argument(name, object, "action")? {
                "play" => PlaybackAction::Play,
                "pause" => PlaybackAction::Pause,
                "toggle" => PlaybackAction::Toggle,
                "stop" => PlaybackAction::Stop,
                other => {
                    return Err(enum_error(
                        name,
                        "action",
                        other,
                        &["play", "pause", "toggle", "stop"],
                    ));
                }
            };
            Ok(Request::Playback(action))
        }
        APP_SEEK => {
            validate_fields(name, object, &["seconds"], &["seconds"])?;
            let seconds = finite_number_argument(name, object, "seconds")?;
            if seconds < 0.0 {
                return Err(format!("{name} argument 'seconds' must be at least 0"));
            }
            Ok(Request::Seek(seconds))
        }
        APP_LOOP_RANGE => {
            validate_fields(
                name,
                object,
                &["action", "start_seconds", "end_seconds"],
                &["action"],
            )?;
            let action = match string_argument(name, object, "action")? {
                "set" => {
                    let start_seconds = finite_number_argument(name, object, "start_seconds")?;
                    let end_seconds = finite_number_argument(name, object, "end_seconds")?;
                    if start_seconds < 0.0 || end_seconds < 0.0 {
                        return Err(format!("{name} range times must be non-negative"));
                    }
                    if start_seconds >= end_seconds {
                        return Err(format!(
                            "{name} requires start_seconds to be less than end_seconds"
                        ));
                    }
                    LoopRangeAction::Set {
                        start_seconds,
                        end_seconds,
                    }
                }
                "enable" => {
                    reject_range_arguments(name, object, "action 'enable'")?;
                    LoopRangeAction::Enable
                }
                "disable" => {
                    reject_range_arguments(name, object, "action 'disable'")?;
                    LoopRangeAction::Disable
                }
                "clear" => {
                    reject_range_arguments(name, object, "action 'clear'")?;
                    LoopRangeAction::Clear
                }
                other => {
                    return Err(enum_error(
                        name,
                        "action",
                        other,
                        &["set", "enable", "disable", "clear"],
                    ));
                }
            };
            Ok(Request::LoopRange(action))
        }
        APP_TIMELINE_ZOOM => {
            validate_fields(name, object, &["action", "value"], &["action"])?;
            let action = match string_argument(name, object, "action")? {
                "set" => {
                    let value = finite_number_argument(name, object, "value")?;
                    if !(MIN_ZOOM..=MAX_ZOOM).contains(&value) {
                        return Err(format!(
                            "{name} argument 'value' must be between {MIN_ZOOM} and {MAX_ZOOM}"
                        ));
                    }
                    ZoomAction::Set(value as f32)
                }
                "in" => {
                    reject_present(name, object, "value", "action 'in'")?;
                    ZoomAction::In
                }
                "out" => {
                    reject_present(name, object, "value", "action 'out'")?;
                    ZoomAction::Out
                }
                "fit" => {
                    reject_present(name, object, "value", "action 'fit'")?;
                    ZoomAction::Fit
                }
                other => {
                    return Err(enum_error(
                        name,
                        "action",
                        other,
                        &["set", "in", "out", "fit"],
                    ));
                }
            };
            Ok(Request::TimelineZoom(action))
        }
        APP_SELECT_CLIP => {
            validate_fields(name, object, &["action", "clip_id"], &["action"])?;
            let action = match string_argument(name, object, "action")? {
                "select" => SelectionAction::Select(clip_id_argument(name, object)?),
                "toggle" => SelectionAction::Toggle(clip_id_argument(name, object)?),
                "clear" => {
                    reject_present(name, object, "clip_id", "action 'clear'")?;
                    SelectionAction::Clear
                }
                other => {
                    return Err(enum_error(
                        name,
                        "action",
                        other,
                        &["select", "toggle", "clear"],
                    ));
                }
            };
            Ok(Request::SelectClip(action))
        }
        APP_PANEL => {
            validate_fields(name, object, &["panel", "action"], &["panel", "action"])?;
            let panel = match string_argument(name, object, "panel")? {
                "library" => Panel::Library,
                "inspector" => Panel::Inspector,
                "assistant" => Panel::Assistant,
                "timeline" => Panel::Timeline,
                "preview" => Panel::Preview,
                "settings" => Panel::Settings,
                "export" => Panel::Export,
                "canvas" => Panel::Canvas,
                other => {
                    return Err(enum_error(
                        name,
                        "panel",
                        other,
                        &[
                            "library",
                            "inspector",
                            "assistant",
                            "timeline",
                            "preview",
                            "settings",
                            "export",
                            "canvas",
                        ],
                    ));
                }
            };
            let action = match string_argument(name, object, "action")? {
                "show" => VisibilityAction::Show,
                "hide" => VisibilityAction::Hide,
                "toggle" => VisibilityAction::Toggle,
                other => {
                    return Err(enum_error(
                        name,
                        "action",
                        other,
                        &["show", "hide", "toggle"],
                    ));
                }
            };
            Ok(Request::Panel { panel, action })
        }
        APP_THEME => {
            validate_fields(name, object, &["theme"], &["theme"])?;
            parse_theme_choice(string_argument(name, object, "theme")?).map(Request::Theme)
        }
        APP_WINDOW => {
            validate_fields(name, object, &["action"], &["action"])?;
            let action = match string_argument(name, object, "action")? {
                "minimize" => WindowAction::Minimize,
                "restore" => WindowAction::Restore,
                "maximize" => WindowAction::Maximize,
                "fullscreen" => WindowAction::Fullscreen,
                "exit-fullscreen" => WindowAction::ExitFullscreen,
                other => {
                    return Err(enum_error(
                        name,
                        "action",
                        other,
                        &[
                            "minimize",
                            "restore",
                            "maximize",
                            "fullscreen",
                            "exit-fullscreen",
                        ],
                    ));
                }
            };
            Ok(Request::Window(action))
        }
        APP_WINDOW_MOVE => {
            validate_fields(name, object, &["x", "y"], &["x", "y"])?;
            let x = integer_argument(name, object, "x", MIN_WINDOW_COORD, MAX_WINDOW_COORD)? as i32;
            let y = integer_argument(name, object, "y", MIN_WINDOW_COORD, MAX_WINDOW_COORD)? as i32;
            Ok(Request::WindowMove { x, y })
        }
        APP_WINDOW_RESIZE => {
            validate_fields(name, object, &["width", "height"], &["width", "height"])?;
            let width =
                integer_argument(name, object, "width", MIN_WINDOW_WIDTH, MAX_WINDOW_WIDTH)? as u32;
            let height =
                integer_argument(name, object, "height", MIN_WINDOW_HEIGHT, MAX_WINDOW_HEIGHT)?
                    as u32;
            Ok(Request::WindowResize { width, height })
        }
        APP_CLOSE => {
            validate_fields(name, object, &[], &[])?;
            Ok(Request::Close)
        }
        _ => Err(format!("unknown app-control tool '{name}'")),
    }
}

fn validate_fields(
    tool: &str,
    object: &Map<String, Value>,
    allowed: &[&str],
    required: &[&str],
) -> Result<(), String> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("{tool} has unknown argument '{field}'"));
    }
    if let Some(field) = required.iter().find(|field| !object.contains_key(**field)) {
        return Err(format!("{tool} is missing required argument '{field}'"));
    }
    Ok(())
}

fn string_argument<'a>(
    tool: &str,
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .ok_or_else(|| format!("{tool} is missing required argument '{field}'"))?
        .as_str()
        .ok_or_else(|| format!("{tool} argument '{field}' must be a string"))
}

fn clip_id_argument(tool: &str, object: &Map<String, Value>) -> Result<String, String> {
    let value = object
        .get("clip_id")
        .ok_or_else(|| format!("{tool} is missing required argument 'clip_id'"))?;
    if let Some(id) = value.as_str() {
        if id.is_empty() {
            return Err(format!("{tool} argument 'clip_id' must not be empty"));
        }
        return Ok(id.to_string());
    }
    if let Some(id) = value.as_u64() {
        return Ok(id.to_string());
    }
    Err(format!(
        "{tool} argument 'clip_id' must be a non-negative integer or non-empty string"
    ))
}

fn finite_number_argument(
    tool: &str,
    object: &Map<String, Value>,
    field: &str,
) -> Result<f64, String> {
    let value = object
        .get(field)
        .ok_or_else(|| format!("{tool} is missing required argument '{field}'"))?
        .as_f64()
        .ok_or_else(|| format!("{tool} argument '{field}' must be a number"))?;
    if !value.is_finite() {
        return Err(format!("{tool} argument '{field}' must be finite"));
    }
    Ok(value)
}

fn integer_argument(
    tool: &str,
    object: &Map<String, Value>,
    field: &str,
    minimum: i64,
    maximum: i64,
) -> Result<i64, String> {
    let value = object
        .get(field)
        .ok_or_else(|| format!("{tool} is missing required argument '{field}'"))?
        .as_i64()
        .ok_or_else(|| format!("{tool} argument '{field}' must be an integer"))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!(
            "{tool} argument '{field}' must be between {minimum} and {maximum}"
        ));
    }
    Ok(value)
}

fn reject_present(
    tool: &str,
    object: &Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<(), String> {
    if object.contains_key(field) {
        return Err(format!(
            "{tool} argument '{field}' is not valid with {context}"
        ));
    }
    Ok(())
}

fn reject_range_arguments(
    tool: &str,
    object: &Map<String, Value>,
    context: &str,
) -> Result<(), String> {
    reject_present(tool, object, "start_seconds", context)?;
    reject_present(tool, object, "end_seconds", context)
}

fn enum_error(tool: &str, field: &str, value: &str, allowed: &[&str]) -> String {
    format!(
        "{tool} argument '{field}' has unknown value '{value}'; expected one of {}",
        allowed.join(", ")
    )
}

fn parse_theme_choice(value: &str) -> Result<cutlass_settings::ThemeChoice, String> {
    match value {
        "default" => Ok(cutlass_settings::ThemeChoice::Default),
        "ember" => Ok(cutlass_settings::ThemeChoice::Ember),
        "dark-blue" => Ok(cutlass_settings::ThemeChoice::DarkBlue),
        other => Err(enum_error(
            APP_THEME,
            "theme",
            other,
            &["default", "ember", "dark-blue"],
        )),
    }
}

fn theme_key_from_index(index: i32) -> &'static str {
    cutlass_settings::ThemeChoice::from_index(index).key()
}

fn persist_theme_at(path: &Path, theme: cutlass_settings::ThemeChoice) -> Result<(), String> {
    let mut settings = cutlass_settings::load(path).map_err(|error| {
        tracing::error!(%error, "agent app theme could not load settings");
        "theme was not changed because the settings file is malformed or unreadable".to_string()
    })?;
    settings.appearance.theme = theme;
    cutlass_settings::save(path, &settings).map_err(|error| {
        tracing::error!(%error, "agent app theme could not save settings");
        "theme could not be saved; check that the settings directory is writable".to_string()
    })
}

fn dispatch_ui(
    app: slint::Weak<crate::AppWindow>,
    cancel: &AtomicBool,
    operation: impl FnOnce(crate::AppWindow) -> UiResponse + Send + 'static,
) -> Result<ToolOutput, String> {
    if cancel.load(Ordering::Acquire) {
        return Err("cancelled before the app-control UI dispatch".into());
    }

    let (response_tx, response_rx) = bounded::<UiResponse>(1);
    let abandoned = Arc::new(AtomicBool::new(false));
    let event_loop_abandoned = Arc::clone(&abandoned);

    if let Err(error) = slint::invoke_from_event_loop(move || {
        if event_loop_abandoned.load(Ordering::Acquire) {
            return;
        }
        let response = app
            .upgrade()
            .ok_or_else(|| "the Cutlass window is closed".to_string())
            .and_then(operation);
        if !event_loop_abandoned.load(Ordering::Acquire) {
            let _ = response_tx.try_send(response);
        }
    }) {
        abandoned.store(true, Ordering::Release);
        tracing::debug!(%error, "agent app-control event-loop dispatch failed");
        return Err("the Cutlass event loop is unavailable; the app may be closing".to_string());
    }

    let value = wait_for_ui_response(
        &response_rx,
        cancel,
        &abandoned,
        UI_WAIT_TIMEOUT,
        UI_WAIT_SLICE,
    )??;
    serde_json::to_string(&value)
        .map(ToolOutput::text)
        .map_err(|error| format!("could not encode app-control response: {error}"))
}

fn wait_for_ui_response(
    receiver: &Receiver<UiResponse>,
    cancel: &AtomicBool,
    abandoned: &AtomicBool,
    timeout: Duration,
    wait_slice: Duration,
) -> Result<UiResponse, String> {
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Acquire) {
            abandoned.store(true, Ordering::Release);
            return Err("cancelled while waiting for the Cutlass UI".into());
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            abandoned.store(true, Ordering::Release);
            return Err(
                "the Cutlass UI did not respond within 2 seconds; it may be busy or closing".into(),
            );
        }

        match receiver.recv_timeout(wait_slice.min(remaining)) {
            Ok(response) => {
                if cancel.load(Ordering::Acquire) {
                    abandoned.store(true, Ordering::Release);
                    return Err("cancelled while waiting for the Cutlass UI".into());
                }
                return Ok(response);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                abandoned.store(true, Ordering::Release);
                return Err(
                    "the Cutlass UI response channel closed before the action completed".into(),
                );
            }
        }
    }
}

fn execute_ui(app: &crate::AppWindow, request: Request) -> UiResponse {
    if request == Request::State {
        return Ok(app_state_value(app));
    }
    let result = match request {
        Request::State => unreachable!("state returned above"),
        Request::Playback(action) => execute_playback(app, action),
        Request::Seek(seconds) => execute_seek(app, seconds),
        Request::LoopRange(action) => execute_loop_range(app, action),
        Request::TimelineZoom(action) => execute_timeline_zoom(app, action),
        Request::SelectClip(action) => execute_selection(app, action),
        Request::Panel { panel, action } => Ok(execute_panel(app, panel, action)),
        Request::Theme(theme) => {
            app.global::<crate::AppStore>().set_theme_id(theme.index());
            Ok(json!({
                "theme": theme.key(),
                "status": "applied-and-saved"
            }))
        }
        Request::Window(action) => Ok(execute_window(app, action)),
        Request::WindowMove { x, y } => {
            app.window()
                .set_position(slint::PhysicalPosition::new(x, y));
            Ok(json!({
                "status": "requested",
                "position": { "x": x, "y": y }
            }))
        }
        Request::WindowResize { width, height } => {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            Ok(json!({
                "status": "requested",
                "size": { "width": width, "height": height }
            }))
        }
        Request::Close => {
            app.global::<crate::WindowBackend>().invoke_close();
            Ok(json!({ "status": "requested", "action": "close" }))
        }
    }?;
    Ok(state_echo(result, app_state_value(app)))
}

fn state_echo(result: Value, state: Value) -> Value {
    json!({ "result": result, "state": state })
}

fn app_state_value(app: &crate::AppWindow) -> Value {
    let timeline = app.global::<crate::TimelineStore>();
    let editor = app.global::<crate::EditorStore>();
    let state = app.global::<crate::AppState>();
    let app_store = app.global::<crate::AppStore>();
    let view = app.global::<crate::TimelineViewState>();
    let project = editor.get_project();
    let playhead_ticks = timeline.get_playhead_tick();
    let playhead_seconds = ticks_to_seconds(playhead_ticks, &project.sequence.fps);
    let duration_ticks = sequence_end_tick(app, project.sequence.clone());
    let duration_seconds = ticks_to_seconds(duration_ticks, &project.sequence.fps);
    let range_in_tick = timeline.get_range_in_tick();
    let range_out_tick = timeline.get_range_out_tick();
    let range_in_seconds =
        (range_in_tick >= 0).then(|| ticks_to_seconds(range_in_tick, &project.sequence.fps));
    let range_out_seconds =
        (range_out_tick >= 0).then(|| ticks_to_seconds(range_out_tick, &project.sequence.fps));
    let selected_ids = model_strings(&timeline.get_selected_ids());
    let position = app.window().position();
    let size = app.window().size();

    json!({
        "transport": {
            "playing": timeline.get_playing(),
            "playhead_tick": playhead_ticks,
            "playhead_seconds": playhead_seconds,
            "speed_num": timeline.get_play_speed_num(),
            "speed_den": timeline.get_play_speed_den(),
            "loop_enabled": timeline.get_loop_enabled(),
            "range_in_tick": range_in_tick,
            "range_out_tick": range_out_tick,
            "range_in_seconds": range_in_seconds,
            "range_out_seconds": range_out_seconds
        },
        "timeline": {
            "zoom": timeline.get_zoom(),
            "scroll_x": view.get_scroll_x(),
            "scroll_y": view.get_scroll_y(),
            "selected_track_id": timeline.get_selected_track_id().as_str(),
            "selected_clip_id": timeline.get_selected_clip_id().as_str(),
            "selected_clip_ids": selected_ids
        },
        "sequence": {
            "fps_num": project.sequence.fps.num,
            "fps_den": project.sequence.fps.den,
            "duration_ticks": duration_ticks,
            "duration_seconds": duration_seconds
        },
        "theme": theme_key_from_index(app_store.get_theme_id()),
        "panels": {
            "library": state.get_library_panel_open(),
            "inspector": state.get_inspector_panel_open(),
            "assistant": state.get_agent_panel_open(),
            "timeline": state.get_timeline_panel_open(),
            "preview": state.get_preview_maximized(),
            "library_width": app.get_left_pane_width(),
            "inspector_width": app.get_right_pane_width(),
            "assistant_width": app.get_agent_pane_width(),
            "timeline_height": app.get_timeline_height()
        },
        "dialogs": {
            "settings": state.get_settings_open(),
            "export": state.get_export_dialog_open(),
            "canvas": state.get_canvas_dialog_open(),
            "relink": state.get_relink_dialog_open(),
            "confirm_delete_media": state.get_confirm_delete_media_open()
        },
        "launch_visible": state.get_launch_visible(),
        "window": {
            "position": { "x": position.x, "y": position.y },
            "size": { "width": size.width, "height": size.height },
            "minimized": app.window().is_minimized(),
            "maximized": app.window().is_maximized(),
            "fullscreen": app.window().is_fullscreen()
        }
    })
}

fn execute_playback(app: &crate::AppWindow, action: PlaybackAction) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let timeline = app.global::<crate::TimelineStore>();
    let actions = app.global::<crate::TimelineActions>();
    let was_playing = timeline.get_playing();

    match action {
        PlaybackAction::Play if !was_playing => actions.invoke_play(),
        PlaybackAction::Pause if was_playing => actions.invoke_pause(),
        PlaybackAction::Toggle => actions.invoke_toggle_play(),
        PlaybackAction::Stop => {
            actions.invoke_pause();
            actions.invoke_seek_start();
        }
        PlaybackAction::Play | PlaybackAction::Pause => {}
    }

    Ok(json!({
        "action": action.key(),
        "playing": timeline.get_playing(),
        "playhead_tick": timeline.get_playhead_tick()
    }))
}

fn execute_seek(app: &crate::AppWindow, seconds: f64) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let editor = app.global::<crate::EditorStore>();
    let project = editor.get_project();
    let fps = project.sequence.fps.clone();
    if fps.num <= 0 || fps.den <= 0 {
        return Err("cannot seek because the timeline frame rate is invalid".into());
    }

    let requested = seconds_to_ticks(seconds, &fps);
    let clamped = app
        .global::<crate::TimelineActions>()
        .invoke_clamp_playhead(requested);
    let timeline = app.global::<crate::TimelineStore>();
    timeline.set_playhead_tick(clamped);

    Ok(json!({
        "playhead_tick": clamped,
        "playhead_seconds": ticks_to_seconds(clamped, &fps)
    }))
}

fn execute_loop_range(app: &crate::AppWindow, action: LoopRangeAction) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let timeline = app.global::<crate::TimelineStore>();
    if let LoopRangeAction::Set {
        start_seconds,
        end_seconds,
    } = action
    {
        let sequence = app.global::<crate::EditorStore>().get_project().sequence;
        let fps = sequence.fps.clone();
        if fps.num <= 0 || fps.den <= 0 {
            return Err(
                "cannot set a loop range because the timeline frame rate is invalid".into(),
            );
        }
        let duration_tick = sequence_end_tick(app, sequence);
        let (start_tick, end_tick) =
            loop_range_ticks(start_seconds, end_seconds, &fps, duration_tick)?;
        timeline.set_range_in_tick(start_tick);
        timeline.set_range_out_tick(end_tick);
        timeline.set_loop_enabled(true);
    } else {
        match action {
            LoopRangeAction::Enable => {
                if timeline.get_range_in_tick() < 0
                    || timeline.get_range_out_tick() <= timeline.get_range_in_tick()
                {
                    return Err("cannot enable looping until a valid range is set".into());
                }
                timeline.set_loop_enabled(true);
            }
            LoopRangeAction::Disable => timeline.set_loop_enabled(false),
            LoopRangeAction::Clear => {
                timeline.set_loop_enabled(false);
                timeline.set_range_in_tick(-1);
                timeline.set_range_out_tick(-1);
            }
            LoopRangeAction::Set { .. } => unreachable!("set handled above"),
        }
    }

    Ok(json!({
        "action": action.key(),
        "loop_enabled": timeline.get_loop_enabled(),
        "range_in_tick": timeline.get_range_in_tick(),
        "range_out_tick": timeline.get_range_out_tick()
    }))
}

fn execute_timeline_zoom(app: &crate::AppWindow, action: ZoomAction) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let actions = app.global::<crate::TimelineActions>();
    match action {
        ZoomAction::Set(value) => actions.invoke_set_zoom(value),
        ZoomAction::In => actions.invoke_zoom_in(),
        ZoomAction::Out => actions.invoke_zoom_out(),
        ZoomAction::Fit => actions.invoke_zoom_to_fit(),
    }
    Ok(json!({
        "action": action.key(),
        "zoom": app.global::<crate::TimelineStore>().get_zoom()
    }))
}

fn execute_selection(app: &crate::AppWindow, action: SelectionAction) -> UiResponse {
    ensure_timeline_control_available(app)?;
    let timeline = app.global::<crate::TimelineStore>();
    let (clip_id, toggle) = match action {
        SelectionAction::Select(clip_id) => (clip_id, false),
        SelectionAction::Toggle(clip_id) => (clip_id, true),
        SelectionAction::Clear => {
            timeline.set_selected_track_id(SharedString::new());
            timeline.set_selected_clip_id(SharedString::new());
            timeline.set_selected_ids(Default::default());
            return Ok(selection_value(&timeline));
        }
    };
    let sequence = app.global::<crate::EditorStore>().get_project().sequence;
    let (track_id, locked) = find_clip_track(&sequence, &clip_id)
        .ok_or_else(|| format!("unknown timeline clip id '{clip_id}'"))?;
    if locked {
        return Err(format!(
            "clip '{clip_id}' is on locked track '{track_id}'; unlock the track first"
        ));
    }

    let update = if toggle {
        crate::selection::toggle_clip(
            &sequence,
            &timeline.get_selected_ids(),
            &track_id,
            &clip_id,
            timeline.get_link_enabled(),
        )
    } else {
        crate::selection::select_clip(&sequence, &track_id, &clip_id, timeline.get_link_enabled())
    };
    timeline.invoke_apply_selection(update);
    Ok(selection_value(&timeline))
}

fn selection_value(timeline: &crate::TimelineStore<'_>) -> Value {
    json!({
        "selected_track_id": timeline.get_selected_track_id().as_str(),
        "selected_clip_id": timeline.get_selected_clip_id().as_str(),
        "selected_clip_ids": model_strings(&timeline.get_selected_ids())
    })
}

fn find_clip_track(sequence: &crate::Sequence, clip_id: &str) -> Option<(String, bool)> {
    for track_row in 0..sequence.tracks.row_count() {
        let Some(track) = sequence.tracks.row_data(track_row) else {
            continue;
        };
        for clip_row in 0..track.clips.row_count() {
            if track
                .clips
                .row_data(clip_row)
                .is_some_and(|clip| clip.id == clip_id)
            {
                return Some((track.id.to_string(), track.locked));
            }
        }
    }
    None
}

fn execute_panel(app: &crate::AppWindow, panel: Panel, action: VisibilityAction) -> Value {
    let state = app.global::<crate::AppState>();
    let current = match panel {
        Panel::Library => state.get_library_panel_open(),
        Panel::Inspector => state.get_inspector_panel_open(),
        Panel::Assistant => state.get_agent_panel_open(),
        Panel::Timeline => state.get_timeline_panel_open(),
        Panel::Preview => state.get_preview_maximized(),
        Panel::Settings => state.get_settings_open(),
        Panel::Export => state.get_export_dialog_open(),
        Panel::Canvas => state.get_canvas_dialog_open(),
    };
    let visible = action.apply(current);
    match panel {
        Panel::Library => state.set_library_panel_open(visible),
        Panel::Inspector => state.set_inspector_panel_open(visible),
        Panel::Assistant => state.set_agent_panel_open(visible),
        Panel::Timeline => state.set_timeline_panel_open(visible),
        Panel::Preview => state.set_preview_maximized(visible),
        Panel::Settings => state.set_settings_open(visible),
        Panel::Export => state.set_export_dialog_open(visible),
        Panel::Canvas => state.set_canvas_dialog_open(visible),
    }
    json!({ "panel": panel.key(), "visible": visible })
}

fn execute_window(app: &crate::AppWindow, action: WindowAction) -> Value {
    match action {
        WindowAction::Minimize => app.global::<crate::WindowBackend>().invoke_minimize(),
        WindowAction::Restore => {
            app.window().set_minimized(false);
            app.global::<crate::AppState>().set_preview_maximized(false);
            app.global::<crate::WindowBackend>()
                .invoke_set_maximized(false);
        }
        WindowAction::Maximize => app
            .global::<crate::WindowBackend>()
            .invoke_set_maximized(true),
        // AppWindow's full-screen property is bound to this existing state;
        // changing the state preserves that binding and the preview layout.
        WindowAction::Fullscreen => app.global::<crate::AppState>().set_preview_maximized(true),
        WindowAction::ExitFullscreen => {
            app.global::<crate::AppState>().set_preview_maximized(false)
        }
    }
    json!({ "status": "requested", "action": action.key() })
}

fn ensure_timeline_control_available(app: &crate::AppWindow) -> Result<(), String> {
    let state = app.global::<crate::AppState>();
    if state.get_launch_visible() {
        return Err(
            "timeline control is unavailable on the launch screen; open a project first".into(),
        );
    }
    if state.get_settings_open()
        || state.get_export_dialog_open()
        || state.get_canvas_dialog_open()
        || state.get_relink_dialog_open()
        || state.get_confirm_delete_media_open()
        || !app
            .global::<crate::EditorStore>()
            .get_session_error()
            .is_empty()
    {
        return Err(
            "timeline control is blocked by an open dialog; close the dialog and retry".into(),
        );
    }
    Ok(())
}

fn sequence_end_tick(app: &crate::AppWindow, sequence: crate::Sequence) -> i32 {
    app.global::<crate::TimelineLib>()
        .invoke_sequence_duration(sequence)
        .value
        .max(0)
}

fn seconds_to_ticks(seconds: f64, fps: &crate::Rational) -> i32 {
    let ticks = seconds * f64::from(fps.num) / f64::from(fps.den);
    if ticks >= f64::from(i32::MAX) {
        i32::MAX
    } else {
        ticks.round().max(0.0) as i32
    }
}

fn ticks_to_seconds(ticks: i32, fps: &crate::Rational) -> f64 {
    if fps.num <= 0 || fps.den <= 0 {
        return 0.0;
    }
    f64::from(ticks) * f64::from(fps.den) / f64::from(fps.num)
}

fn loop_range_ticks(
    start_seconds: f64,
    end_seconds: f64,
    fps: &crate::Rational,
    duration_tick: i32,
) -> Result<(i32, i32), String> {
    let start_tick = seconds_to_ticks(start_seconds, fps);
    let end_tick = seconds_to_ticks(end_seconds, fps);
    if start_tick >= end_tick {
        return Err("loop range endpoints collapse to the same frame; choose a wider range".into());
    }
    if end_tick > duration_tick {
        return Err(format!(
            "loop range ends at {end_seconds:.3}s, beyond the {:.3}s sequence",
            ticks_to_seconds(duration_tick, fps)
        ));
    }
    Ok((start_tick, end_tick))
}

fn model_strings(model: &slint::ModelRc<SharedString>) -> Vec<String> {
    (0..model.row_count())
        .filter_map(|row| model.row_data(row))
        .map(|value| value.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;

    use super::*;

    #[test]
    fn registry_has_exact_unique_names_tiers_and_object_schemas() {
        let registry = specs();
        let names: Vec<_> = registry.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, TOOL_NAMES);
        assert_eq!(
            names.iter().copied().collect::<BTreeSet<_>>().len(),
            TOOL_NAMES.len()
        );

        let expected_tiers = BTreeMap::from([
            (APP_STATE, ToolTier::ReadOnly),
            (APP_PLAYBACK, ToolTier::Workspace),
            (APP_SEEK, ToolTier::Workspace),
            (APP_LOOP_RANGE, ToolTier::Workspace),
            (APP_TIMELINE_ZOOM, ToolTier::Workspace),
            (APP_SELECT_CLIP, ToolTier::Workspace),
            (APP_PANEL, ToolTier::Workspace),
            (APP_THEME, ToolTier::Workspace),
            (APP_WINDOW, ToolTier::Workspace),
            (APP_WINDOW_MOVE, ToolTier::Workspace),
            (APP_WINDOW_RESIZE, ToolTier::Workspace),
            (APP_CLOSE, ToolTier::System),
        ]);
        let expected_fields = BTreeMap::from([
            (APP_STATE, BTreeSet::from([])),
            (APP_PLAYBACK, BTreeSet::from(["action"])),
            (APP_SEEK, BTreeSet::from(["seconds"])),
            (
                APP_LOOP_RANGE,
                BTreeSet::from(["action", "end_seconds", "start_seconds"]),
            ),
            (APP_TIMELINE_ZOOM, BTreeSet::from(["action", "value"])),
            (APP_SELECT_CLIP, BTreeSet::from(["action", "clip_id"])),
            (APP_PANEL, BTreeSet::from(["action", "panel"])),
            (APP_THEME, BTreeSet::from(["theme"])),
            (APP_WINDOW, BTreeSet::from(["action"])),
            (APP_WINDOW_MOVE, BTreeSet::from(["x", "y"])),
            (APP_WINDOW_RESIZE, BTreeSet::from(["height", "width"])),
            (APP_CLOSE, BTreeSet::from([])),
        ]);
        let expected_required = BTreeMap::from([
            (APP_STATE, BTreeSet::from([])),
            (APP_PLAYBACK, BTreeSet::from(["action"])),
            (APP_SEEK, BTreeSet::from(["seconds"])),
            (APP_LOOP_RANGE, BTreeSet::from(["action"])),
            (APP_TIMELINE_ZOOM, BTreeSet::from(["action"])),
            (APP_SELECT_CLIP, BTreeSet::from(["action"])),
            (APP_PANEL, BTreeSet::from(["action", "panel"])),
            (APP_THEME, BTreeSet::from(["theme"])),
            (APP_WINDOW, BTreeSet::from(["action"])),
            (APP_WINDOW_MOVE, BTreeSet::from(["x", "y"])),
            (APP_WINDOW_RESIZE, BTreeSet::from(["height", "width"])),
            (APP_CLOSE, BTreeSet::from([])),
        ]);

        for entry in registry {
            assert_eq!(entry.tier, expected_tiers[entry.name.as_str()]);
            assert!(!entry.description.trim().is_empty());
            assert_eq!(entry.parameters["type"], "object");
            assert_eq!(entry.parameters["additionalProperties"], false);
            let fields = entry.parameters["properties"]
                .as_object()
                .expect("properties object")
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            assert_eq!(fields, expected_fields[entry.name.as_str()]);
            let required = entry.parameters["required"]
                .as_array()
                .expect("required array")
                .iter()
                .map(|value| value.as_str().expect("required field name"))
                .collect::<BTreeSet<_>>();
            assert_eq!(required, expected_required[entry.name.as_str()]);
        }
    }

    #[test]
    fn registry_enum_values_and_numeric_bounds_are_exact() {
        let registry = specs();
        let schema = |name: &str| {
            &registry
                .iter()
                .find(|entry| entry.name == name)
                .expect("registered tool")
                .parameters
        };
        assert_eq!(
            schema(APP_PLAYBACK)["properties"]["action"]["enum"],
            json!(["play", "pause", "toggle", "stop"])
        );
        assert_eq!(
            schema(APP_TIMELINE_ZOOM)["properties"]["action"]["enum"],
            json!(["set", "in", "out", "fit"])
        );
        assert_eq!(
            schema(APP_LOOP_RANGE)["properties"]["action"]["enum"],
            json!(["set", "enable", "disable", "clear"])
        );
        assert_eq!(
            schema(APP_SELECT_CLIP)["properties"]["action"]["enum"],
            json!(["select", "toggle", "clear"])
        );
        assert_eq!(
            schema(APP_PANEL)["properties"]["panel"]["enum"],
            json!([
                "library",
                "inspector",
                "assistant",
                "timeline",
                "preview",
                "settings",
                "export",
                "canvas"
            ])
        );
        assert_eq!(
            schema(APP_WINDOW)["properties"]["action"]["enum"],
            json!([
                "minimize",
                "restore",
                "maximize",
                "fullscreen",
                "exit-fullscreen"
            ])
        );
        assert_eq!(
            schema(APP_WINDOW_MOVE)["properties"]["x"]["minimum"],
            MIN_WINDOW_COORD
        );
        assert_eq!(
            schema(APP_WINDOW_RESIZE)["properties"]["width"]["maximum"],
            MAX_WINDOW_WIDTH
        );
    }

    #[test]
    fn parser_accepts_every_tool_and_action_branch() {
        assert_eq!(parse_request(APP_STATE, &json!({})), Ok(Request::State));
        for (key, action) in [
            ("play", PlaybackAction::Play),
            ("pause", PlaybackAction::Pause),
            ("toggle", PlaybackAction::Toggle),
            ("stop", PlaybackAction::Stop),
        ] {
            assert_eq!(
                parse_request(APP_PLAYBACK, &json!({ "action": key })),
                Ok(Request::Playback(action))
            );
        }
        assert_eq!(
            parse_request(APP_SEEK, &json!({ "seconds": 1.25 })),
            Ok(Request::Seek(1.25))
        );
        assert_eq!(
            parse_request(
                APP_LOOP_RANGE,
                &json!({
                    "action": "set",
                    "start_seconds": 1.0,
                    "end_seconds": 2.5
                })
            ),
            Ok(Request::LoopRange(LoopRangeAction::Set {
                start_seconds: 1.0,
                end_seconds: 2.5
            }))
        );
        for (key, action) in [
            ("enable", LoopRangeAction::Enable),
            ("disable", LoopRangeAction::Disable),
            ("clear", LoopRangeAction::Clear),
        ] {
            assert_eq!(
                parse_request(APP_LOOP_RANGE, &json!({ "action": key })),
                Ok(Request::LoopRange(action))
            );
        }
        assert_eq!(
            parse_request(APP_TIMELINE_ZOOM, &json!({ "action": "set", "value": 2.5 })),
            Ok(Request::TimelineZoom(ZoomAction::Set(2.5)))
        );
        for (key, action) in [
            ("in", ZoomAction::In),
            ("out", ZoomAction::Out),
            ("fit", ZoomAction::Fit),
        ] {
            assert_eq!(
                parse_request(APP_TIMELINE_ZOOM, &json!({ "action": key })),
                Ok(Request::TimelineZoom(action))
            );
        }
        assert_eq!(
            parse_request(
                APP_SELECT_CLIP,
                &json!({ "action": "select", "clip_id": "42" })
            ),
            Ok(Request::SelectClip(SelectionAction::Select("42".into())))
        );
        assert_eq!(
            parse_request(
                APP_SELECT_CLIP,
                &json!({ "action": "select", "clip_id": 42 })
            ),
            Ok(Request::SelectClip(SelectionAction::Select("42".into())))
        );
        assert_eq!(
            parse_request(
                APP_SELECT_CLIP,
                &json!({ "action": "toggle", "clip_id": "42" })
            ),
            Ok(Request::SelectClip(SelectionAction::Toggle("42".into())))
        );
        assert_eq!(
            parse_request(APP_SELECT_CLIP, &json!({ "action": "clear" })),
            Ok(Request::SelectClip(SelectionAction::Clear))
        );
        for (panel_key, panel) in [
            ("library", Panel::Library),
            ("inspector", Panel::Inspector),
            ("assistant", Panel::Assistant),
            ("timeline", Panel::Timeline),
            ("preview", Panel::Preview),
            ("settings", Panel::Settings),
            ("export", Panel::Export),
            ("canvas", Panel::Canvas),
        ] {
            for (action_key, action) in [
                ("show", VisibilityAction::Show),
                ("hide", VisibilityAction::Hide),
                ("toggle", VisibilityAction::Toggle),
            ] {
                assert_eq!(
                    parse_request(
                        APP_PANEL,
                        &json!({ "panel": panel_key, "action": action_key })
                    ),
                    Ok(Request::Panel { panel, action })
                );
            }
        }
        for (key, theme) in [
            ("default", cutlass_settings::ThemeChoice::Default),
            ("ember", cutlass_settings::ThemeChoice::Ember),
            ("dark-blue", cutlass_settings::ThemeChoice::DarkBlue),
        ] {
            assert_eq!(
                parse_request(APP_THEME, &json!({ "theme": key })),
                Ok(Request::Theme(theme))
            );
        }
        for (key, action) in [
            ("minimize", WindowAction::Minimize),
            ("restore", WindowAction::Restore),
            ("maximize", WindowAction::Maximize),
            ("fullscreen", WindowAction::Fullscreen),
            ("exit-fullscreen", WindowAction::ExitFullscreen),
        ] {
            assert_eq!(
                parse_request(APP_WINDOW, &json!({ "action": key })),
                Ok(Request::Window(action))
            );
        }
        assert_eq!(
            parse_request(APP_WINDOW_MOVE, &json!({ "x": -50, "y": 90 })),
            Ok(Request::WindowMove { x: -50, y: 90 })
        );
        assert_eq!(
            parse_request(APP_WINDOW_RESIZE, &json!({ "width": 1280, "height": 720 })),
            Ok(Request::WindowResize {
                width: 1280,
                height: 720
            })
        );
        assert_eq!(parse_request(APP_CLOSE, &json!({})), Ok(Request::Close));
    }

    #[test]
    fn parser_requires_objects_known_fields_and_required_fields() {
        assert!(
            parse_request("app_missing", &json!({}))
                .unwrap_err()
                .contains("unknown")
        );
        assert!(
            parse_request(APP_STATE, &Value::Null)
                .unwrap_err()
                .contains("must be an object")
        );
        assert!(
            parse_request(APP_STATE, &json!({ "extra": true }))
                .unwrap_err()
                .contains("unknown argument 'extra'")
        );
        assert!(
            parse_request(APP_PLAYBACK, &json!({}))
                .unwrap_err()
                .contains("missing required argument 'action'")
        );
        assert!(
            parse_request(APP_PLAYBACK, &json!({ "action": 1 }))
                .unwrap_err()
                .contains("must be a string")
        );
        assert!(
            parse_request(APP_PLAYBACK, &json!({ "action": "rewind" }))
                .unwrap_err()
                .contains("unknown value 'rewind'")
        );
        assert!(parse_request(APP_CLOSE, &json!({ "now": true })).is_err());
    }

    #[test]
    fn parser_checks_seek_zoom_and_selection_dependencies() {
        assert_eq!(
            parse_request(APP_SEEK, &json!({ "seconds": 0 })),
            Ok(Request::Seek(0.0))
        );
        assert!(parse_request(APP_SEEK, &json!({ "seconds": -0.01 })).is_err());
        assert!(parse_request(APP_SEEK, &json!({ "seconds": "1" })).is_err());

        assert!(
            parse_request(
                APP_LOOP_RANGE,
                &json!({ "action": "set", "start_seconds": 1.0 })
            )
            .is_err()
        );
        for (start, end) in [(-1.0, 2.0), (2.0, 2.0), (3.0, 2.0)] {
            assert!(
                parse_request(
                    APP_LOOP_RANGE,
                    &json!({
                        "action": "set",
                        "start_seconds": start,
                        "end_seconds": end
                    })
                )
                .is_err()
            );
        }
        assert!(
            parse_request(
                APP_LOOP_RANGE,
                &json!({ "action": "enable", "start_seconds": 1.0 })
            )
            .is_err()
        );
        assert!(parse_request(APP_LOOP_RANGE, &json!({ "action": "repeat" })).is_err());

        for value in [MIN_ZOOM, MAX_ZOOM] {
            assert!(
                parse_request(
                    APP_TIMELINE_ZOOM,
                    &json!({ "action": "set", "value": value })
                )
                .is_ok()
            );
        }
        for value in [MIN_ZOOM - 0.001, MAX_ZOOM + 0.001, f64::MAX] {
            assert!(
                parse_request(
                    APP_TIMELINE_ZOOM,
                    &json!({ "action": "set", "value": value })
                )
                .is_err()
            );
        }
        assert!(parse_request(APP_TIMELINE_ZOOM, &json!({ "action": "set" })).is_err());
        assert!(parse_request(APP_TIMELINE_ZOOM, &json!({ "action": "fit", "value": 1 })).is_err());

        assert!(parse_request(APP_SELECT_CLIP, &json!({ "action": "select" })).is_err());
        assert!(
            parse_request(
                APP_SELECT_CLIP,
                &json!({ "action": "toggle", "clip_id": "" })
            )
            .is_err()
        );
        assert!(
            parse_request(
                APP_SELECT_CLIP,
                &json!({ "action": "select", "clip_id": -1 })
            )
            .is_err()
        );
        assert!(
            parse_request(
                APP_SELECT_CLIP,
                &json!({ "action": "clear", "clip_id": "42" })
            )
            .is_err()
        );
    }

    #[test]
    fn parser_checks_integer_types_and_window_edge_bounds() {
        for coordinate in [MIN_WINDOW_COORD, MAX_WINDOW_COORD] {
            assert!(
                parse_request(
                    APP_WINDOW_MOVE,
                    &json!({ "x": coordinate, "y": coordinate })
                )
                .is_ok()
            );
        }
        for coordinate in [MIN_WINDOW_COORD - 1, MAX_WINDOW_COORD + 1] {
            assert!(parse_request(APP_WINDOW_MOVE, &json!({ "x": coordinate, "y": 0 })).is_err());
        }
        assert!(parse_request(APP_WINDOW_MOVE, &json!({ "x": 1.5, "y": 0 })).is_err());

        for (width, height) in [
            (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT),
            (MAX_WINDOW_WIDTH, MAX_WINDOW_HEIGHT),
        ] {
            assert!(
                parse_request(
                    APP_WINDOW_RESIZE,
                    &json!({ "width": width, "height": height })
                )
                .is_ok()
            );
        }
        for (width, height) in [
            (MIN_WINDOW_WIDTH - 1, MIN_WINDOW_HEIGHT),
            (MAX_WINDOW_WIDTH + 1, MIN_WINDOW_HEIGHT),
            (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT - 1),
            (MIN_WINDOW_WIDTH, MAX_WINDOW_HEIGHT + 1),
        ] {
            assert!(
                parse_request(
                    APP_WINDOW_RESIZE,
                    &json!({ "width": width, "height": height })
                )
                .is_err()
            );
        }
    }

    #[test]
    fn theme_keys_map_exactly_and_do_not_accept_aliases() {
        assert_eq!(
            parse_theme_choice("default"),
            Ok(cutlass_settings::ThemeChoice::Default)
        );
        assert_eq!(
            parse_theme_choice("ember"),
            Ok(cutlass_settings::ThemeChoice::Ember)
        );
        assert_eq!(
            parse_theme_choice("dark-blue"),
            Ok(cutlass_settings::ThemeChoice::DarkBlue)
        );
        assert!(parse_theme_choice("dark_blue").is_err());
        assert_eq!(theme_key_from_index(0), "default");
        assert_eq!(theme_key_from_index(1), "ember");
        assert_eq!(theme_key_from_index(2), "dark-blue");
        assert_eq!(theme_key_from_index(99), "dark-blue");
    }

    #[test]
    fn theme_persistence_refuses_to_overwrite_malformed_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let malformed = b"[appearance\ntheme = \"ember\"\n";
        fs::write(&path, malformed).expect("write malformed config");

        let error = persist_theme_at(&path, cutlass_settings::ThemeChoice::Default).unwrap_err();
        assert!(!error.contains(path.to_string_lossy().as_ref()));
        assert_eq!(fs::read(&path).expect("read config"), malformed);
    }

    #[test]
    fn theme_persistence_updates_only_owned_setting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "# keep me\n[appearance]\ntheme = \"default\"\n\n[future]\nflag = true\n",
        )
        .expect("write config");

        persist_theme_at(&path, cutlass_settings::ThemeChoice::Ember).expect("save theme");
        let saved = fs::read_to_string(&path).expect("read config");
        assert!(saved.contains("# keep me"));
        assert!(saved.contains("[future]"));
        assert!(saved.contains("flag = true"));
        assert_eq!(
            cutlass_settings::load(&path)
                .expect("load")
                .appearance
                .theme,
            cutlass_settings::ThemeChoice::Ember
        );
    }

    #[test]
    fn visibility_actions_cover_all_transitions() {
        assert!(VisibilityAction::Show.apply(false));
        assert!(VisibilityAction::Show.apply(true));
        assert!(!VisibilityAction::Hide.apply(false));
        assert!(!VisibilityAction::Hide.apply(true));
        assert!(VisibilityAction::Toggle.apply(false));
        assert!(!VisibilityAction::Toggle.apply(true));
    }

    #[test]
    fn mutating_response_envelope_always_carries_result_and_state() {
        let value = state_echo(
            json!({ "status": "requested" }),
            json!({ "transport": { "playing": false } }),
        );
        assert_eq!(value["result"]["status"], "requested");
        assert_eq!(value["state"]["transport"]["playing"], false);
        assert_eq!(value.as_object().expect("echo object").len(), 2);
    }

    #[test]
    fn clip_lookup_reports_track_and_lock_state() {
        let unlocked_clip = crate::Clip {
            id: "unlocked-clip".into(),
            ..Default::default()
        };
        let locked_clip = crate::Clip {
            id: "locked-clip".into(),
            ..Default::default()
        };
        let sequence = crate::Sequence {
            tracks: slint::ModelRc::new(slint::VecModel::from(vec![
                crate::Track {
                    id: "track-a".into(),
                    clips: slint::ModelRc::new(slint::VecModel::from(vec![unlocked_clip])),
                    ..Default::default()
                },
                crate::Track {
                    id: "track-b".into(),
                    clips: slint::ModelRc::new(slint::VecModel::from(vec![locked_clip])),
                    locked: true,
                    ..Default::default()
                },
            ])),
            ..Default::default()
        };

        assert_eq!(
            find_clip_track(&sequence, "unlocked-clip"),
            Some(("track-a".into(), false))
        );
        assert_eq!(
            find_clip_track(&sequence, "locked-clip"),
            Some(("track-b".into(), true))
        );
        assert_eq!(find_clip_track(&sequence, "missing"), None);
    }

    #[test]
    fn time_conversion_rounds_and_saturates() {
        let fps = crate::Rational { num: 24, den: 1 };
        assert_eq!(seconds_to_ticks(0.0, &fps), 0);
        assert_eq!(seconds_to_ticks(1.49 / 24.0, &fps), 1);
        assert_eq!(seconds_to_ticks(1.51 / 24.0, &fps), 2);
        assert_eq!(seconds_to_ticks(f64::MAX, &fps), i32::MAX);
        assert!((ticks_to_seconds(36, &fps) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn loop_range_conversion_rejects_collapsed_and_out_of_sequence_ranges() {
        let fps = crate::Rational { num: 24, den: 1 };
        assert_eq!(loop_range_ticks(1.0, 2.5, &fps, 120), Ok((24, 60)));
        assert!(loop_range_ticks(1.0, 1.01, &fps, 120).is_err());
        let error = loop_range_ticks(4.0, 6.0, &fps, 120).unwrap_err();
        assert!(error.contains("beyond the 5.000s sequence"), "{error}");
    }

    #[test]
    fn response_wait_returns_a_queued_result() {
        let (sender, receiver) = bounded(1);
        sender.send(Ok(json!({ "ok": true }))).expect("send");
        let cancel = AtomicBool::new(false);
        let abandoned = AtomicBool::new(false);
        let result = wait_for_ui_response(
            &receiver,
            &cancel,
            &abandoned,
            Duration::from_millis(50),
            Duration::from_millis(2),
        )
        .expect("wait")
        .expect("UI result");
        assert_eq!(result, json!({ "ok": true }));
        assert!(!abandoned.load(Ordering::Acquire));
    }

    #[test]
    fn call_honors_cancellation_before_parsing_or_dispatch() {
        let cancel = AtomicBool::new(true);
        let error = call(slint::Weak::default(), APP_PLAYBACK, &Value::Null, &cancel).unwrap_err();
        assert!(error.contains("cancelled"));
    }

    #[test]
    fn response_wait_is_cancellation_aware() {
        let (_sender, receiver) = bounded::<UiResponse>(1);
        let cancel = AtomicBool::new(true);
        let abandoned = AtomicBool::new(false);
        let started = Instant::now();
        let error = wait_for_ui_response(
            &receiver,
            &cancel,
            &abandoned,
            Duration::from_secs(1),
            Duration::from_millis(5),
        )
        .unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(abandoned.load(Ordering::Acquire));
    }

    #[test]
    fn response_wait_times_out_and_detects_disconnect() {
        let (_sender, receiver) = bounded::<UiResponse>(1);
        let cancel = AtomicBool::new(false);
        let abandoned = AtomicBool::new(false);
        let started = Instant::now();
        let error = wait_for_ui_response(
            &receiver,
            &cancel,
            &abandoned,
            Duration::from_millis(15),
            Duration::from_millis(3),
        )
        .unwrap_err();
        assert!(error.contains("did not respond"));
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(abandoned.load(Ordering::Acquire));

        let (sender, receiver) = bounded::<UiResponse>(1);
        drop(sender);
        let abandoned = AtomicBool::new(false);
        let error = wait_for_ui_response(
            &receiver,
            &cancel,
            &abandoned,
            Duration::from_secs(1),
            Duration::from_millis(5),
        )
        .unwrap_err();
        assert!(error.contains("channel closed"));
        assert!(abandoned.load(Ordering::Acquire));
    }
}
