import Foundation

/// `modeld daemon status --json`: the background service and its last pass.
public struct DaemonStatus: Decodable, Sendable, Equatable {
    public var state: AgentState
    public var pid: Int?
    /// The service's own copy of modeld, `~/.modeld/bin/modeld`.
    public var binary: String
    /// Whether that copy matches the engine that answered; `nil` when it
    /// could not be read. `false` means the service needs an update.
    public var binaryIsCurrent: Bool?
    public var plist: String
    public var log: String
    /// The service only logs the clones it would make.
    public var dryRun: Bool
    /// Variables the installed service runs with, such as `HF_HOME`.
    public var environment: [String: String]
    public var lastPass: LastPass?
    public var recentLog: [String]

    private enum CodingKeys: String, CodingKey {
        case state, pid, binary
        case binaryIsCurrent = "binary_is_current"
        case plist, log
        case dryRun = "dry_run"
        case environment
        case lastPass = "last_pass"
        case recentLog = "recent_log"
    }

    /// Whether launchd will run the service (now or at the next login).
    public var isInstalled: Bool {
        state != .notInstalled
    }
}

/// The background service's lifecycle state.
public enum AgentState: String, OpenEnum {
    case running
    /// Loaded into launchd, but no process right now.
    case loaded
    /// Installed, not loaded; it starts at the next login.
    case installed
    case notInstalled = "not_installed"
    case unknown
}

/// The last pass the background service logged.
public struct LastPass: Decodable, Sendable, Equatable {
    /// RFC 3339 in UTC, as logged.
    public var at: String
    public var summary: String
    public var failed: Bool

    /// The timestamp as a date, if it parses.
    public var date: Date? {
        try? Date(at, strategy: .iso8601)
    }
}

/// `modeld daemon install --json`.
public struct DaemonInstall: Decodable, Sendable, Equatable {
    public var binary: String
    public var plist: String
    public var log: String
    public var status: DaemonStatus
}

/// `modeld daemon uninstall --json`.
public struct DaemonUninstall: Decodable, Sendable, Equatable {
    public var wasInstalled: Bool

    private enum CodingKeys: String, CodingKey {
        case wasInstalled = "was_installed"
    }
}
