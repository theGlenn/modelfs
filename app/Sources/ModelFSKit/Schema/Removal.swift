import Foundation

/// `modeld rm --json`: the plan, and what running it did.
public struct RemovalResult: Decodable, Sendable, Equatable {
    public var status: RemovalStatus
    public var plan: RemovalPlan
    /// What ran; `nil` when nothing did.
    public var outcome: RemovalOutcome?
}

/// How a removal ended.
public enum RemovalStatus: String, OpenEnum {
    /// A dry run: the plan, nothing changed.
    case planned
    /// The plan made under the store lock differs; nothing changed.
    case planChanged = "plan_changed"
    /// Something blocks the removal; nothing changed.
    case refused
    case removed
    /// A step failed; the steps before it ran.
    case failed
    case stopped
    case unknown
}

/// Everything a removal would do, and what blocks it.
public struct RemovalPlan: Decodable, Sendable, Equatable {
    /// Pass back to run exactly this plan.
    public var planID: String
    public var models: [RemovalModel]
    /// In the order they run.
    public var steps: [RemovalStep]
    /// Anything here blocks the whole removal.
    public var refusals: [RemovalRefusal]
    public var notes: [RemovalNote]
    public var collateral: [RemovalCollateral]
    public var frees: Frees

    private enum CodingKeys: String, CodingKey {
        case planID = "plan_id"
        case models, steps, refusals, notes, collateral, frees
    }

    /// Every copy goes, and the stored copy with them.
    public var removesEverything: Bool {
        models.allSatisfy(\.everywhere)
    }
}

/// A model the plan removes copies of.
public struct RemovalModel: Decodable, Sendable, Equatable {
    public var digest: String
    public var name: String
    public var size: UInt64
    public var everywhere: Bool
}

/// One planned change.
public struct RemovalStep: Decodable, Sendable, Equatable {
    public var action: StepAction
    /// The app the path belongs to (`ollama`, ...), or `store`.
    public var provider: String
    public var path: String
    /// The Ollama tag, for `ollamaDelete`.
    public var tag: String?
    public var digest: String

    /// The app, or `nil` for the store's own copy.
    public var app: Provider? {
        provider == "store" ? nil : Provider(rawValue: provider) ?? .unknown
    }
}

/// How a step removes its path.
public enum StepAction: String, OpenEnum {
    case ollamaDelete = "ollama_delete"
    case unlink
    case delete
    case deleteFolder = "delete_folder"
    case trash
    case trashFolder = "trash_folder"
    case removeStoredCopy = "remove_stored_copy"
    case unknown
}

/// A path that blocks the removal.
public struct RemovalRefusal: Decodable, Sendable, Equatable, Identifiable {
    public var code: RefusalCode
    public var path: String
    public var message: String
    /// For `inUse`: the processes holding the file open.
    public var processes: [String]

    public var id: String { path + "\u{0}" + code.rawValue }
}

/// Why a path blocks the removal.
public enum RefusalCode: String, OpenEnum {
    case unknownModel = "unknown_model"
    case notReferenced = "not_referenced"
    case nothingSelected = "nothing_selected"
    case outsideRoots = "outside_roots"
    case notRegularFile = "not_regular_file"
    case unreadable
    case contentChanged = "content_changed"
    case inUse = "in_use"
    case downloadInProgress = "download_in_progress"
    case ollamaNotRunning = "ollama_not_running"
    case ollamaNotLoopback = "ollama_not_loopback"
    case ollamaStoreMismatch = "ollama_store_mismatch"
    case unknown
}

/// Something worth knowing before confirming.
public struct RemovalNote: Decodable, Sendable, Equatable, Identifiable {
    public var code: NoteCode
    public var path: String
    public var message: String
    public var paths: [String]

    public var id: String { path + "\u{0}" + code.rawValue }
}

/// What a note is about.
public enum NoteCode: String, OpenEnum {
    case lastApp = "last_app"
    case alreadyGone = "already_gone"
    case verifyByHashing = "verify_by_hashing"
    case gitRepository = "git_repository"
    case symlinks
    case unknown
}

/// A file that goes along with the model.
public struct RemovalCollateral: Decodable, Sendable, Equatable {
    public var path: String
    public var size: UInt64
    public var reason: String
}

/// About how many bytes a removal frees.
public struct Frees: Decodable, Sendable, Equatable {
    public var now: UInt64
    /// Once the Trash is emptied.
    public var afterTrash: UInt64
    /// Bytes of removed files that other files keep using.
    public var pinned: UInt64

    private enum CodingKeys: String, CodingKey {
        case now
        case afterTrash = "after_trash"
        case pinned
    }
}

/// What running a plan did.
public struct RemovalOutcome: Decodable, Sendable, Equatable {
    /// Indexes into `plan.steps` that ran to completion.
    public var completed: [Int]
    public var failure: StepFailure?
    public var stopped: Bool
    public var warnings: [String]
}

/// A step that failed, and why.
public struct StepFailure: Decodable, Sendable, Equatable {
    public var step: Int
    public var reason: String
}
