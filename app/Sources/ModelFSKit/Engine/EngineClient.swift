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
