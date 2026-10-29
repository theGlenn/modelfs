import Foundation

/// The line format version this app reads; see `schema/README.md`.
public let engineSchemaVersion = 1

/// One line of `modeld --json` output, decoded for a command whose result
/// is `Result`.
public enum EngineMessage<Result: Decodable & Sendable>: Sendable {
    /// Progress while the command runs.
    case event(PassEvent)
    /// The command's outcome; the last line of a successful run.
    case result(Result)
    /// Why the command failed.
    case failure(EngineFailure)
    /// A line type a newer engine added; safe to ignore.
    case unknown
}

extension EngineMessage: Decodable {
    private enum CodingKeys: String, CodingKey {
        case type, schema, event, result, error
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let schema = try container.decode(Int.self, forKey: .schema)
        guard schema == engineSchemaVersion else {
            throw DecodingError.dataCorruptedError(
                forKey: .schema,
                in: container,
                debugDescription: "modeld speaks schema \(schema); this app reads \(engineSchemaVersion)"
            )
        }
        switch try container.decode(String.self, forKey: .type) {
        case "event":
            self = .event(try container.decode(PassEvent.self, forKey: .event))
        case "result":
            self = .result(try container.decode(Result.self, forKey: .result))
        case "error":
            self = .failure(try container.decode(EngineFailure.self, forKey: .error))
        default:
            self = .unknown
        }
    }
}

/// The payload of an error line.
public struct EngineFailure: Decodable, Sendable, Equatable {
    public var code: Code
    /// Human-readable detail; not part of the contract.
    public var message: String

    public init(code: Code, message: String) {
        self.code = code
        self.message = message
    }

    /// Why the engine failed, for the app to branch on.
    public enum Code: String, OpenEnum {
        case storeUnavailable = "store_unavailable"
        case registryUnreadable = "registry_unreadable"
        case passFailed = "pass_failed"
        case cancelled
        case installFailed = "install_failed"
        case uninstallFailed = "uninstall_failed"
        case unsupported
        case confirmationRequired = "confirmation_required"
        case removeFailed = "remove_failed"
        case watchFailed = "watch_failed"
        case discoverFailed = "discover_failed"
        case unknown
    }
}

/// Progress of a pass, a removal, or a discovery.
public enum PassEvent: Sendable, Equatable {
    /// Another modeld process, often the daemon, holds the store lock.
    case waitingForLock
    /// The pass holds the lock. It can no longer be cancelled.
    case locked
    case hashing(path: String, size: UInt64)
    case cloning(path: String, size: UInt64)
    /// `rm`: a step is about to run on this path.
    case removing(path: String)
    /// `discover`: how many folders it has looked through so far.
    case discovering(folders: Int)
    /// An event kind a newer engine added.
    case unknown(kind: String)
}

extension PassEvent: Decodable {
    private enum CodingKeys: String, CodingKey {
        case kind, path, size, folders
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "waiting_for_lock":
            self = .waitingForLock
        case "locked":
            self = .locked
        case "hashing":
            self = .hashing(
                path: try container.decode(String.self, forKey: .path),
                size: try container.decode(UInt64.self, forKey: .size)
            )
        case "cloning":
            self = .cloning(
                path: try container.decode(String.self, forKey: .path),
                size: try container.decode(UInt64.self, forKey: .size)
            )
        case "removing":
            self = .removing(path: try container.decode(String.self, forKey: .path))
        case "discovering":
            self = .discovering(folders: try container.decode(Int.self, forKey: .folders))
        default:
            self = .unknown(kind: kind)
        }
    }
}
