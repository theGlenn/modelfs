import Foundation

/// `modeld ls --json`: every stored model and where its copies stand.
public struct Inventory: Decodable, Sendable, Equatable {
    /// Largest first.
    public var models: [Model]
    public var totals: Space

    public init(models: [Model], totals: Space) {
        self.models = models
        self.totals = totals
    }

    public static let empty = Inventory(models: [], totals: .zero)
}

/// One stored model: a digest, its stored copy, and every app's copy.
public struct Model: Decodable, Sendable, Equatable, Identifiable {
    /// `sha256:<hex>`, the model's identity.
    public var digest: String
    /// Display name: descriptive header, or filename/label/digest fallback.
    public var name: String
    /// `nil` until the engine has read the file's header.
    public var kind: Kind?
    public var format: String?
    public var quant: String?
    public var params: String?
    public var architecture: String?
    public var size: UInt64
    public var stored: StoredCopy
    public var references: [Reference]
    public var space: Space

    public var id: String { digest }

    /// Weights, or a model-adjacent asset such as a tokenizer.
    public enum Kind: String, OpenEnum {
        case model, asset, unknown
    }
}

/// The store's own copy of a model, under `~/.modeld/blobs`.
public struct StoredCopy: Decodable, Sendable, Equatable {
    public var path: String
    public var present: Bool
}

/// One app's copy of a model.
public struct Reference: Decodable, Sendable, Equatable, Identifiable {
    public var path: String
    public var provider: Provider
    public var label: String?
    /// Where it came from in the app's terms: the Hugging Face repo, the
    /// LM Studio `publisher/repo`, the Ollama tags, or your folder's root.
    public var source: String?
    /// Every Ollama tag using this file; empty for other apps.
    public var tags: [String]
    public var state: ReferenceState

    public var id: String { path }
}

/// Which app a copy belongs to.
public enum Provider: String, OpenEnum, CaseIterable, Comparable {
    case ollama
    case huggingface
    case lmstudio
    case xybrid
    /// Folders listed in `~/.modeld/config.toml`.
    case manual
    case unknown

    public static func < (lhs: Self, rhs: Self) -> Bool {
        lhs.order < rhs.order
    }

    private var order: Int {
        Self.allCases.firstIndex(of: self) ?? 0
    }
}

/// Where one app's copy stands against the stored copy.
public enum ReferenceState: String, OpenEnum, CaseIterable {
    /// The file is gone; the next complete pass forgets it.
    case missing
    /// The path could not be checked, as when macOS denies access to its
    /// folder. It counts as a present copy.
    case unreadable
    /// The file shares the stored copy's blocks.
    case clone
    /// Other hard links share the file; ModelFS never replaces it.
    case hardlinked
    /// The file changed since ModelFS hashed it.
    case changed
    /// A copy on another volume than the store; clones cannot cross volumes.
    case otherVolume = "other_volume"
    /// An independent copy a pass can turn into a clone.
    case copy
    case unknown
}

/// Disk usage in bytes, estimated from each copy's state.
public struct Space: Decodable, Sendable, Equatable {
    /// What model copies and stored copies occupy.
    public var onDisk: UInt64
    /// What sharing saves today.
    public var saved: UInt64
    /// What a pass could still free.
    public var pending: UInt64
    /// Stored copies of models no app uses anymore.
    public var storeOnly: UInt64
    /// What the apps' copies would occupy without sharing.
    public var logical: UInt64

    public init(onDisk: UInt64, saved: UInt64, pending: UInt64, storeOnly: UInt64, logical: UInt64) {
        self.onDisk = onDisk
        self.saved = saved
        self.pending = pending
        self.storeOnly = storeOnly
        self.logical = logical
    }

    public static let zero = Space(onDisk: 0, saved: 0, pending: 0, storeOnly: 0, logical: 0)

    private enum CodingKeys: String, CodingKey {
        case onDisk = "on_disk"
        case saved
        case pending
        case storeOnly = "store_only"
        case logical
    }
}
