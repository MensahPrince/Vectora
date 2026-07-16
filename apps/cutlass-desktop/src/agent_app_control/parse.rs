use super::*;

pub(super) const MIN_ZOOM: f64 = 0.05;
pub(super) const MAX_ZOOM: f64 = 40.0;
pub(super) const MIN_WINDOW_WIDTH: i64 = 640;
pub(super) const MAX_WINDOW_WIDTH: i64 = 7_680;
pub(super) const MIN_WINDOW_HEIGHT: i64 = 480;
pub(super) const MAX_WINDOW_HEIGHT: i64 = 4_320;
pub(super) const MIN_WINDOW_COORD: i64 = -100_000;
pub(super) const MAX_WINDOW_COORD: i64 = 100_000;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Request {
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
pub(super) enum PlaybackAction {
    Play,
    Pause,
    Toggle,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum LoopRangeAction {
    Set {
        start_seconds: f64,
        end_seconds: f64,
    },
    Enable,
    Disable,
    Clear,
}

impl LoopRangeAction {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Set { .. } => "set",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Clear => "clear",
        }
    }
}

impl PlaybackAction {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Toggle => "toggle",
            Self::Stop => "stop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum ZoomAction {
    Set(f32),
    In,
    Out,
    Fit,
}

impl ZoomAction {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Set(_) => "set",
            Self::In => "in",
            Self::Out => "out",
            Self::Fit => "fit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SelectionAction {
    Select(String),
    Toggle(String),
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
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
    pub(super) fn key(self) -> &'static str {
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
pub(super) enum VisibilityAction {
    Show,
    Hide,
    Toggle,
}

impl VisibilityAction {
    pub(super) fn apply(self, current: bool) -> bool {
        match self {
            Self::Show => true,
            Self::Hide => false,
            Self::Toggle => !current,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowAction {
    Minimize,
    Restore,
    Maximize,
    Fullscreen,
    ExitFullscreen,
}

impl WindowAction {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Restore => "restore",
            Self::Maximize => "maximize",
            Self::Fullscreen => "fullscreen",
            Self::ExitFullscreen => "exit-fullscreen",
        }
    }
}

pub(super) fn parse_request(name: &str, arguments: &Value) -> Result<Request, String> {
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

pub(super) fn validate_fields(
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

pub(super) fn string_argument<'a>(
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

pub(super) fn clip_id_argument(tool: &str, object: &Map<String, Value>) -> Result<String, String> {
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

pub(super) fn finite_number_argument(
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

pub(super) fn integer_argument(
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

pub(super) fn reject_present(
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

pub(super) fn reject_range_arguments(
    tool: &str,
    object: &Map<String, Value>,
    context: &str,
) -> Result<(), String> {
    reject_present(tool, object, "start_seconds", context)?;
    reject_present(tool, object, "end_seconds", context)
}

pub(super) fn enum_error(tool: &str, field: &str, value: &str, allowed: &[&str]) -> String {
    format!(
        "{tool} argument '{field}' has unknown value '{value}'; expected one of {}",
        allowed.join(", ")
    )
}

pub(super) fn parse_theme_choice(value: &str) -> Result<cutlass_settings::ThemeChoice, String> {
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

pub(super) fn theme_key_from_index(index: i32) -> &'static str {
    cutlass_settings::ThemeChoice::from_index(index).key()
}

pub(super) fn persist_theme_at(
    path: &Path,
    theme: cutlass_settings::ThemeChoice,
) -> Result<(), String> {
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
