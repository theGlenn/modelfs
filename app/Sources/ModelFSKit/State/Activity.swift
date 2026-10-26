import Foundation

/// What the app is changing right now; there is never more than one.
public enum Activity: Sendable, Equatable {
    case idle
    case pass(PassProgress)
    case installingService
    case removingService
    case addingFolder
    /// Removing a model; it cannot be cancelled once started.
    case removing(name: String)

    public var isBusy: Bool {
        self != .idle
    }
}

/// Where a running pass is, from the events the engine reported.
public struct PassProgress: Sendable, Equatable {
    public enum Step: Sendable, Equatable {
        /// Started; no event yet.
        case starting
        /// Another modeld process, usually the background service, is busy.
        case waitingForLock
        /// Holds the lock; reading folders.
        case running
        case hashing(path: String)
        case cloning(path: String)
    }

    /// The pass fills the store without changing any app's files.
    public var dryRun: Bool
    public var step: Step

    public init(dryRun: Bool, step: Step = .starting) {
        self.dryRun = dryRun
        self.step = step
    }

    /// The engine stops a pass only before it holds the store lock.
    public var canCancel: Bool {
        step == .starting || step == .waitingForLock
    }

    /// Moves forward with `event`. Once the pass holds the lock, it never
    /// reads as cancellable again.
    public mutating func apply(_ event: PassEvent) {
        switch event {
        case .waitingForLock where canCancel:
            step = .waitingForLock
        case .waitingForLock, .unknown, .removing:
            break
        case .locked:
            step = .running
        case .hashing(let path, _):
            step = .hashing(path: path)
        case .cloning(let path, _):
            step = .cloning(path: path)
        }
    }
}
