import AppKit
import ModelFSKit
import SwiftUI

extension UInt64 {
    /// Bytes the way Finder shows them, such as `2.6 GB`.
    var bytes: String {
        Int64(clamping: self).formatted(.byteCount(style: .file, spellsOutZero: false))
    }
}

extension Provider {
    var displayName: String {
        switch self {
        case .ollama: "Ollama"
        case .huggingface: "Hugging Face"
        case .lmstudio: "LM Studio"
        case .manual: "Your folders"
        case .unknown: "Other"
        }
    }

    /// For tight columns.
    var shortName: String {
        self == .manual ? "Folders" : displayName
    }

    var symbol: String {
        switch self {
        case .ollama: "cube"
        case .huggingface: "face.smiling"
        case .lmstudio: "macwindow"
        case .manual: "folder"
        case .unknown: "questionmark.folder"
        }
    }
}

extension ReferenceState {
    var label: String {
        switch self {
        case .missing: "Gone"
        case .unreadable: "Can't read"
        case .clone: "Shared"
        case .hardlinked: "Hard-linked"
        case .changed: "Changed"
        case .otherVolume: "Other disk"
        case .copy: "Separate copy"
        case .unknown: "Unknown"
        }
    }

    var help: String {
        switch self {
        case .missing: "The file is gone. ModelFS forgets it at the next scan."
        case .unreadable:
            "ModelFS could not check this file. If it is in a protected folder such as Downloads, allow ModelFS in System Settings > Privacy & Security."
        case .clone: "Shares its disk space with the stored copy."
        case .hardlinked: "Other names point at this file, so ModelFS leaves it alone."
        case .changed: "The file changed since ModelFS last checked it."
        case .otherVolume: "On another disk than ~/.modeld. Clones cannot cross disks, so it keeps its own space."
        case .copy: "Uses its own disk space. Deduplicating turns it into a clone."
        case .unknown: "A newer modeld reported a state this app does not know."
        }
    }

    var tint: Color {
        switch self {
        case .missing: .secondary
        case .unreadable: .red
        case .clone: .green
        case .hardlinked: .blue
        case .changed: .orange
        case .otherVolume: .blue
        case .copy: .yellow
        case .unknown: .secondary
        }
    }
}

extension ModelStatus {
    var label: String {
        switch self {
        case .canDeduplicate: "Can deduplicate"
        case .changed: "Changed"
        case .sharing: "Sharing"
        case .single: "One copy"
        case .storeOnly: "Not in any app"
        }
    }

    var tint: Color {
        switch self {
        case .canDeduplicate: .yellow
        case .changed: .orange
        case .sharing: .green
        case .single: .secondary
        case .storeOnly: .purple
        }
    }
}

extension AgentState {
    var label: String {
        switch self {
        case .running: "Running"
        case .loaded: "Loaded, not running"
        case .installed: "Starts at next login"
        case .notInstalled: "Off"
        case .unknown: "Unknown"
        }
    }
}

extension Model {
    var formatLabel: String { format ?? "—" }
    var quantLabel: String { quant ?? "—" }

    /// Format, quantization, size label, and architecture, as one line.
    var facts: String {
        [format, quant, params.map { "\($0) params" }, architecture]
            .compactMap(\.self)
            .joined(separator: " · ")
    }
}

/// A small colored capsule with a label.
struct Badge: View {
    let text: String
    let tint: Color

    var body: some View {
        Text(text)
            .font(.caption.weight(.medium))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(tint.opacity(0.18), in: Capsule())
            .foregroundStyle(tint == .secondary ? Color.secondary : Color.primary)
    }
}

/// Shows `path` selected in a Finder window.
@MainActor
func revealInFinder(_ path: String) {
    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
}

/// The last component of a path, for compact progress lines.
func fileName(_ path: String) -> String {
    URL(fileURLWithPath: path).lastPathComponent
}
