import Foundation

/// `modeld pass --json`: what one pass did.
public struct PassReport: Decodable, Sendable, Equatable {
    public var dryRun: Bool
    /// Models newly copied into the store.
    public var imported: Int
    /// Files whose references the pass recorded.
    public var synced: Int
    /// Stale references dropped; `nil` when the pass was incomplete.
    public var pruned: Int?
    public var complete: Bool
    /// Duplicates replaced, or that would be on a dry run.
    public var planned: [PlannedClone]
    public var cloned: [String]
    /// Bytes the clones made free.
    public var freed: UInt64
    public var refused: [Problem]
    public var deferred: [DeferredFile]
    public var skipped: [Problem]
    public var warnings: [String]
    /// The daemon's one-line summary, such as `pass: nothing new`.
    public var summary: String

    private enum CodingKeys: String, CodingKey {
        case dryRun = "dry_run"
        case imported, synced, pruned, complete, planned, cloned, freed
        case refused, deferred, skipped, warnings, summary
    }
}

/// A duplicate a pass replaces with a clone.
public struct PlannedClone: Decodable, Sendable, Equatable {
    public var path: String
    public var digest: String
    public var size: UInt64
}

/// A file left for a later pass because it was written moments ago.
public struct DeferredFile: Decodable, Sendable, Equatable {
    public var path: String
    public var settlesInSecs: UInt64

    private enum CodingKeys: String, CodingKey {
        case path
        case settlesInSecs = "settles_in_secs"
    }
}
