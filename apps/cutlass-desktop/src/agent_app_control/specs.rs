use super::*;

pub(super) const APP_STATE: &str = "app_state";
pub(super) const APP_PLAYBACK: &str = "app_playback";
pub(super) const APP_SEEK: &str = "app_seek";
pub(super) const APP_LOOP_RANGE: &str = "app_loop_range";
pub(super) const APP_TIMELINE_ZOOM: &str = "app_timeline_zoom";
pub(super) const APP_SELECT_CLIP: &str = "app_select_clip";
pub(super) const APP_PANEL: &str = "app_panel";
pub(super) const APP_THEME: &str = "app_theme";
pub(super) const APP_WINDOW: &str = "app_window";
pub(super) const APP_WINDOW_MOVE: &str = "app_window_move";
pub(super) const APP_WINDOW_RESIZE: &str = "app_window_resize";
pub(super) const APP_CLOSE: &str = "app_close";

pub(super) const TOOL_NAMES: [&str; 12] = [
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

pub(super) fn spec(
    name: &str,
    description: &str,
    parameters: Value,
    tier: ToolTier,
) -> HostToolSpec {
    HostToolSpec {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
        tier,
    }
}
