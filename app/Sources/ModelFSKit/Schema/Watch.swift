import Foundation

/// `modeld watch --json`: a folder or pattern saved in the scan configuration.
public struct WatchResult: Decodable, Sendable, Equatable {
    /// The canonical absolute folder path, or the pattern as saved.
    public var path: String
    /// False when the folder was already watched.
    public var added: Bool
    /// The configuration file that contains the folder.
    public var config: String
}

extension WatchResult {
    /// Whether a glob pattern was saved rather than one folder.
    public var isPattern: Bool {
        !path.hasPrefix("/")
    }
}

/// What `modeld watch` adds to the scan configuration.
public enum WatchTarget: Sendable, Hashable {
    /// One existing folder.
    case folder(URL)
    /// A glob pattern over folders, such as `~/games/*/Assets`.
    case pattern(String)

    var arguments: [String] {
        switch self {
        case .folder(let folder): ["watch", folder.path]
        case .pattern(let pattern): ["watch", "--pattern", pattern]
        }
    }
}
