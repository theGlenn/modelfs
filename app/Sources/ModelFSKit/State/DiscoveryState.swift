import Foundation

/// Where the search for folders worth watching stands.
public enum DiscoveryState: Sendable, Equatable {
    case idle
    /// Looking; `foldersRead` grows as the engine reports progress.
    case running(foldersRead: Int)
    case finished(DiscoveryReport)
    /// Why the search failed, worded for people.
    case failed(String)

    public var isRunning: Bool {
        if case .running = self { true } else { false }
    }
}
