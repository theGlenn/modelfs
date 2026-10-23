import Foundation

extension Frees {
    /// One sentence on the space a removal frees, always "about".
    ///
    /// `bytes` formats a byte count; the app passes Finder's style.
    public func summary(bytes: (UInt64) -> String) -> String {
        if now == 0, afterTrash == 0 {
            return pinned > 0
                ? "Frees about \(bytes(0)): other apps share these bytes."
                : "Frees about \(bytes(0))."
        }
        var sentence = "Frees about "
        if now > 0 {
            sentence += "\(bytes(now)) now"
        }
        if afterTrash > 0 {
            sentence += now > 0 ? " and \(bytes(afterTrash))" : bytes(afterTrash)
            sentence += " once you empty the Trash"
        }
        if pinned > 0 {
            sentence += "; \(bytes(pinned)) stays in use by other copies"
        }
        return sentence + "."
    }
}

/// What a plan does to one app's files, in a line people read.
public struct AppChange: Sendable, Equatable, Identifiable {
    /// The app, or `nil` for the ModelFS store.
    public var app: Provider?
    public var summary: String
    /// Every path the steps touch, in order.
    public var paths: [String]

    public var id: String { app?.rawValue ?? "store" }
}

extension RemovalPlan {
    /// The steps grouped by app, apps in a fixed order, the store last.
    public var changes: [AppChange] {
        var grouped: [Provider?: [RemovalStep]] = [:]
        for step in steps {
            grouped[step.app, default: []].append(step)
        }
        let apps: [Provider?] = Provider.allCases.map(Optional.some) + [nil]
        return apps.compactMap { app in
            guard let steps = grouped[app] else { return nil }
            return AppChange(app: app, summary: Self.summary(of: steps), paths: steps.map(\.path))
        }
    }

    private static func summary(of steps: [RemovalStep]) -> String {
        let actions = Set(steps.map(\.action))
        let tags = steps.compactMap(\.tag)
        if !tags.isEmpty {
            return tags.count == 1 ? "Ollama deletes the tag \(tags[0])" : "Ollama deletes the tags \(tags.joined(separator: ", "))"
        }
        if actions.contains(.removeStoredCopy) {
            return "Delete the stored copy"
        }
        if actions.contains(.deleteFolder) {
            return "Delete the cached download"
        }
        if actions.contains(.trashFolder) {
            return "Move the model folder to the Trash"
        }
        let trashed = steps.filter { $0.action == .trash }.count
        if trashed > 0 {
            return trashed == 1 ? "Move the file to the Trash" : "Move \(trashed) files to the Trash"
        }
        let deleted = steps.filter { $0.action == .delete }.count
        return deleted == 1 ? "Delete the file" : "Delete \(deleted) files"
    }
}

extension RemovalRefusal {
    /// What to do about it, in a line people read.
    public var advice: String {
        switch code {
        case .inUse:
            let who = processes.isEmpty ? "another app" : processes.joined(separator: ", ")
            return "Open in \(who). Quit it or unload the model, then check again."
        case .ollamaNotRunning:
            return "Ollama removes its own models, so it has to be running."
        case .ollamaNotLoopback:
            return "OLLAMA_HOST points at another machine; ModelFS only talks to Ollama on this Mac."
        case .ollamaStoreMismatch:
            return "The running Ollama uses another models folder than the one ModelFS scanned."
        case .downloadInProgress:
            return "It is still downloading. Check again once the download finishes."
        case .contentChanged:
            return "The file changed since ModelFS looked. Refresh, then check again."
        default:
            return message
        }
    }
}
