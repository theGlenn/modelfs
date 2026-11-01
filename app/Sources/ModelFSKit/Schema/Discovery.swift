import Foundation

/// `modeld discover --json`: folders no scan covers that hold large model files.
public struct DiscoveryReport: Decodable, Sendable, Equatable {
    /// Most bytes to free first, then largest.
    public var suggestions: [FolderSuggestion]
    /// Files directly in the home folder or a project root: too broad to watch.
    public var loose: [DiscoveredFile]
    /// The smallest file size considered, in bytes.
    public var minSize: UInt64
    public var foldersRead: Int
    /// Entries that could not be listed or inspected.
    public var unreadable: Int
    /// Files and folders left alone because they are stored only in the cloud.
    public var cloudOnly: Int

    private enum CodingKeys: String, CodingKey {
        case suggestions, loose
        case minSize = "min_size"
        case foldersRead = "folders_read"
        case unreadable
        case cloudOnly = "cloud_only"
    }
}

/// One folder, or one glob pattern over sibling projects, worth watching.
public struct FolderSuggestion: Decodable, Sendable, Equatable, Identifiable {
    /// The glob pattern to watch, such as `~/games/*/Assets`; `nil` to watch
    /// the one folder in `folders`.
    public var pattern: String?
    /// The folders holding `files`.
    public var folders: [String]
    public var size: UInt64
    /// Bytes watching it would probably free.
    public var reclaimable: UInt64
    /// Largest first.
    public var files: [DiscoveredFile]

    public var id: String { pattern ?? folders.first ?? "" }

    /// What watching it adds to the scan configuration.
    public var target: WatchTarget {
        if let pattern {
            .pattern(pattern)
        } else {
            .folder(URL(fileURLWithPath: folders.first ?? "/"))
        }
    }
}

/// A large model file no scan covers.
public struct DiscoveredFile: Decodable, Sendable, Equatable, Identifiable {
    public var path: String
    public var size: UInt64
    /// Same size and sampled bytes as a stored model or another file found.
    public var likelyCopy: Bool
    /// Bytes no other file shares: what deduplicating it would free. Zero for
    /// a copy that is already a clone.
    public var reclaimable: UInt64

    public var id: String { path }

    private enum CodingKeys: String, CodingKey {
        case path, size
        case likelyCopy = "likely_copy"
        case reclaimable
    }
}
