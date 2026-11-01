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

/// Notes of one kind, merged so a plan for many models stays readable.
public struct NoteSummary: Sendable, Equatable, Identifiable {
    public var code: NoteCode
    public var text: String
    /// The paths the notes are about, then their related paths.
    public var paths: [String]

    public var id: String { code.rawValue }
}

extension RemovalPlan {
    /// One line per kind of note, in the order kinds first appear. A kind
    /// with one note keeps its own message.
    public var noteSummaries: [NoteSummary] {
        var order: [NoteCode] = []
        var byCode: [NoteCode: [RemovalNote]] = [:]
        for note in notes {
            if byCode[note.code] == nil {
                order.append(note.code)
            }
            byCode[note.code, default: []].append(note)
        }
        return order.map { code in
            let notes = byCode[code] ?? []
            let text = notes.count == 1
                ? notes[0].message
                : code.summary(count: notes.count) ?? "\(notes[0].message) (and \(notes.count - 1) more)"
            return NoteSummary(code: code, text: text, paths: notes.map(\.path) + notes.flatMap(\.paths))
        }
    }
}

extension NoteCode {
    /// What `count` notes of this kind say together.
    func summary(count: Int) -> String? {
        switch self {
        case .lastApp: "\(count) models leave their last app, so their stored copies go too."
        case .alreadyGone: "\(count) files are already gone; ModelFS only forgets them."
        case .verifyByHashing: "\(count) files changed since they were hashed, so they are hashed again first."
        case .gitRepository: "\(count) files are inside a git repository."
        case .symlinks: "\(count) files have symlinks pointing at them; those links will stop working."
        case .unknown: nil
        }
    }
}

extension RemovalRefusal {
    /// Whether this refusal is about `model`: its digest or one of its copies.
    public func concerns(_ model: Model) -> Bool {
        path == model.digest || model.references.contains { $0.path == path }
    }
}
