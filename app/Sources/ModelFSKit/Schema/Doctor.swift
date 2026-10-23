import Foundation

/// `modeld doctor --json`: the folders modeld scans.
public struct DoctorReport: Decodable, Sendable, Equatable {
    /// The store directory, normally `~/.modeld`.
    public var store: String
    public var roots: [ScanRoot]
    /// Sources of folders that could not be read, such as a broken config.
    public var unreadable: [Problem]
}

/// One folder modeld scans for an app.
public struct ScanRoot: Decodable, Sendable, Equatable, Identifiable {
    public var provider: Provider
    public var path: String
    public var excluded: [String]

    public var id: String { path }
}

/// A path paired with why it was left out.
public struct Problem: Decodable, Sendable, Equatable, Identifiable {
    public var path: String
    public var reason: String

    public var id: String { path + "\u{0}" + reason }
}
