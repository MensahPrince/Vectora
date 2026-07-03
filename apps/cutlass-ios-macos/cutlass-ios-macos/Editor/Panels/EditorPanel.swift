import Foundation

/// Every bottom panel the editor can present. Toolbars and boundary buttons
/// route through `EditorView.activePanel`.
nonisolated enum EditorPanel: Hashable {
    // Root toolbar
    case aspect
    case background
    case text(editing: UUID?, tab: Int = 0)
    case stickers
    case effects
    case filters
    case adjust
    case audio
    case captions

    // Selected main clip
    case clipVolume
    case clipSpeed
    case clipAnimation
    case clipFilter
    case clipAdjust
    case clipOpacity
    case clipCrop
    case clipMask
    case clipChroma
    case clipStabilize

    // Selected lane clips
    case overlayVolume
    case audioVolume
    case audioFade

    // Clip boundary
    case transition(after: UUID)

    /// Panels that edit the selected clip close when the selection clears.
    var requiresSelection: Bool {
        switch self {
        case .aspect, .background, .stickers, .effects, .filters, .adjust, .audio, .captions, .transition:
            return false
        default:
            return true
        }
    }

    var title: String {
        switch self {
        case .aspect: return "Aspect ratio"
        case .background: return "Background"
        case .text(let editing, _): return editing == nil ? "Add text" : "Edit text"
        case .stickers: return "Stickers"
        case .effects: return "Effects"
        case .filters: return "Filters"
        case .adjust: return "Adjust"
        case .audio: return "Audio"
        case .captions: return "Auto captions"
        case .clipVolume, .overlayVolume, .audioVolume: return "Volume"
        case .clipSpeed: return "Speed"
        case .clipAnimation: return "Animation"
        case .clipFilter: return "Filters"
        case .clipAdjust: return "Adjust"
        case .clipOpacity: return "Opacity"
        case .clipCrop: return "Crop"
        case .clipMask: return "Mask"
        case .clipChroma: return "Chroma key"
        case .clipStabilize: return "Stabilize"
        case .audioFade: return "Fade"
        case .transition: return "Transition"
        }
    }
}
