import Foundation

/// `modeld watch <folder> --json`: a folder saved in the scan configuration.
public struct WatchResult: Decodable, Sendable, Equatable {
    /// The canonical absolute folder path.
    public var path: String
    /// False when the folder was already watched.
    public var added: Bool
    /// The configuration file that contains the folder.
    public var config: String
}
