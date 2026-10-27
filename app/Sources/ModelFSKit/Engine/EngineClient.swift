import Foundation

/// Typed calls to the engine, one process per call.
///
/// Every call passes `--json` and the resolved ``EngineEnvironment``, so
/// the engine scans the same folders as the background service.
public struct EngineClient: Sendable {
    public var executable: URL
    public var environment: EngineEnvironment

    public init(executable: URL, environment: EngineEnvironment) {
        self.executable = executable
        self.environment = environment
    }

    /// The folders modeld scans.
    public func doctor() async throws(EngineError) -> DoctorReport {
        try await run(["doctor"])
    }

    /// Adds a folder to the persistent scan configuration; model files do not change.
    public func watchFolder(_ folder: URL) async throws(EngineError) -> WatchResult {
        try await run(["watch", folder.path])
    }

    /// Every stored model and where its copies stand.
    public func inventory() async throws(EngineError) -> Inventory {
        try await run(["ls"])
    }

    /// The background service's state and last pass.
    public func daemonStatus() async throws(EngineError) -> DaemonStatus {
        try await run(["daemon", "status"])
    }

    /// Installs or updates the background service with this app's engine.
    ///
    /// Waits for a running service to finish its pass, up to two minutes.
    public func installDaemon() async throws(EngineError) -> DaemonInstall {
        try await run(["daemon", "install"])
    }

    /// Stops and removes the background service.
    public func uninstallDaemon() async throws(EngineError) -> DaemonUninstall {
        try await run(["daemon", "uninstall"])
    }

    /// Runs one pass: sync every folder, then clone duplicates.
    ///
    /// With `dryRun`, the store fills up but no app's file changes.
    /// Cancelling the calling task stops the pass only while it waits for
    /// the store lock; see ``EngineProcess``.
    public func pass(
        dryRun: Bool,
        onEvent: @escaping @Sendable (PassEvent) -> Void
    ) async throws(EngineError) -> PassReport {
        let arguments = dryRun ? ["pass", "--dry-run"] : ["pass"]
        return try await EngineProcess.run(
            invocation(arguments),
            interruptible: true,
            onEvent: onEvent
        )
    }

    /// Plans removing `scope`'s copies of a model; changes nothing.
    public func planRemoval(digest: String, scope: RemovalScope) async throws(EngineError) -> RemovalResult {
        try await EngineProcess.run(invocation(["rm", digest] + scope.arguments + ["--dry-run"]))
    }

    /// Runs the removal planned as `planID`, if the plan is still the same.
    ///
    /// Returns `.planChanged` with the new plan when anything moved since
    /// the dry run. Not cancellable: once asked, the engine runs every step
    /// it can, in order.
    public func remove(
        digest: String,
        scope: RemovalScope,
        expecting planID: String,
        onEvent: @escaping @Sendable (PassEvent) -> Void = { _ in }
    ) async throws(EngineError) -> RemovalResult {
        try await EngineProcess.run(
            invocation(["rm", digest] + scope.arguments + ["--expect-plan", planID]),
            acceptedStatuses: [0, 1, 3],
            onEvent: onEvent
        )
    }

    /// The invocation for `arguments`, with `--json` and the environment.
    public func invocation(_ arguments: [String]) -> EngineInvocation {
        EngineInvocation(
            executable: executable,
            arguments: arguments + ["--json"],
            environment: environment.processEnvironment
        )
    }

    private func run<Result: Decodable & Sendable>(_ arguments: [String]) async throws(EngineError) -> Result {
        try await EngineProcess.run(invocation(arguments))
    }
}

/// Which copies of a model a removal takes.
public enum RemovalScope: Sendable, Hashable {
    /// Every copy one app holds.
    case app(Provider)
    /// Every copy, and the stored copy.
    case everywhere

    var arguments: [String] {
        switch self {
        case .app(let provider): ["--provider", provider.rawValue]
        case .everywhere: ["--everywhere"]
        }
    }
}
