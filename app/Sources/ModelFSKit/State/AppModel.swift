import Foundation
import Observation

/// Everything the app shows, and the one change it may be making.
///
/// Reads (inventory, folders, service status) run in parallel and are
/// refreshed whenever the store's watched files change. Changes (a pass,
/// installing or removing the service) run one at a time and are owned
/// here, not by a view, so closing a window never interrupts one.
@MainActor
@Observable
public final class AppModel {
    public private(set) var inventory: Inventory?
    public private(set) var doctor: DoctorReport?
    public private(set) var daemon: DaemonStatus?
    public private(set) var environment: EngineEnvironment?
    public private(set) var activity: Activity = .idle
    /// The last pass this app ran, with what it did.
    public private(set) var lastPass: PassReport?
    /// The last thing that went wrong, worded for people.
    public private(set) var problem: String?
    /// The last pass was cancelled while it waited for the store lock.
    public private(set) var lastPassCancelled = false

    private let home: URL
    private let locateEngine: @MainActor () -> URL?
    private let resolveEnvironment: @Sendable (URL) async -> EngineEnvironment
    private var client: EngineClient?
    private var watcher: StoreWatcher?
    private var refresher: Refresher?
    private var mutation: Task<Void, Never>?

    public init(
        home: URL = FileManager.default.homeDirectoryForCurrentUser,
        locateEngine: @escaping @MainActor () -> URL? = { EngineLocator.locate() },
        resolveEnvironment: @escaping @Sendable (URL) async -> EngineEnvironment = { home in
            await EngineEnvironment.resolve(home: home)
        }
    ) {
        self.home = home
        self.locateEngine = locateEngine
        self.resolveEnvironment = resolveEnvironment
    }

    /// The store directory the engine uses.
    public var storeDirectory: URL {
        home.appending(path: ".modeld")
    }

    /// Disk usage across every model.
    public var totals: Space {
        inventory?.totals ?? .zero
    }

    /// Nothing is stored yet and no service will fill the store.
    public var needsFirstScan: Bool {
        guard let inventory, let daemon else { return false }
        return inventory.models.isEmpty && !daemon.isInstalled
    }

    // MARK: Reading

    /// Finds the engine, resolves its environment, and starts watching.
    public func start() async {
        guard client == nil, problem == nil else { return }
        guard let executable = locateEngine() else {
            problem = EngineError.notFound.localizedDescription
            return
        }
        let environment = await resolveEnvironment(home)
        self.environment = environment
        client = EngineClient(executable: executable, environment: environment)
        let refresher = Refresher { [weak self] in
            await self?.reload()
        }
        self.refresher = refresher
        watcher = StoreWatcher(directory: storeDirectory) {
            Task { await refresher.request() }
        }
        await reload()
    }

    /// Asks for a refresh; requests close together run once.
    public func refresh() {
        guard let refresher else { return }
        Task { await refresher.request() }
    }

    /// Reads the inventory, folders, and service status in parallel.
    func reload() async {
        guard let client else { return }
        async let inventory = Self.attempt { () async throws(EngineError) in try await client.inventory() }
        async let doctor = Self.attempt { () async throws(EngineError) in try await client.doctor() }
        async let daemon = Self.attempt { () async throws(EngineError) in try await client.daemonStatus() }
        let (inventoryResult, doctorResult, daemonResult) = await (inventory, doctor, daemon)
        var failures: [EngineError] = []
        record(inventoryResult, into: \.inventory, failures: &failures)
        record(doctorResult, into: \.doctor, failures: &failures)
        record(daemonResult, into: \.daemon, failures: &failures)
        if let failure = failures.first {
            problem = failure.localizedDescription
        } else if activity == .idle {
            problem = nil
        }
    }

    private func record<Value>(
        _ result: Result<Value, EngineError>,
        into keyPath: ReferenceWritableKeyPath<AppModel, Value?>,
        failures: inout [EngineError]
    ) {
        switch result {
        case .success(let value):
            self[keyPath: keyPath] = value
        case .failure(let error):
            failures.append(error)
        }
    }

    // MARK: Changing

    /// Runs one pass; with `dryRun`, only the store changes.
    public func runPass(dryRun: Bool) {
        guard mutation == nil, let client else { return }
        activity = .pass(PassProgress(dryRun: dryRun))
        lastPassCancelled = false
        let (events, forward) = AsyncStream.makeStream(of: PassEvent.self)
        mutation = Task {
            let progress = Task { @MainActor in
                for await event in events {
                    self.advance(with: event)
                }
            }
            let result = await Self.attempt { () async throws(EngineError) in
                try await client.pass(dryRun: dryRun) { event in
                    forward.yield(event)
                }
            }
            forward.finish()
            await progress.value
            await finishPass(result)
        }
    }

    /// Stops a pass that is still waiting for the store lock.
    ///
    /// Does nothing once the pass holds the lock: it then runs to the end.
    public func cancelPass() {
        guard case .pass(let progress) = activity, progress.canCancel else { return }
        mutation?.cancel()
    }

    /// Installs or updates the background service with this app's engine.
    public func installService() {
        changeService(.installingService) { client throws(EngineError) in
            _ = try await client.installDaemon()
        }
    }

    /// Stops and removes the background service.
    public func removeService() {
        changeService(.removingService) { client throws(EngineError) in
            _ = try await client.uninstallDaemon()
        }
    }

    private func advance(with event: PassEvent) {
        guard case .pass(var progress) = activity else { return }
        progress.apply(event)
        activity = .pass(progress)
    }

    private func finishPass(_ result: Result<PassReport, EngineError>) async {
        switch result {
        case .success(let report):
            lastPass = report
            problem = nil
        case .failure(.failed(let failure)) where failure.code == .cancelled:
            lastPassCancelled = true
        case .failure(let error):
            problem = error.localizedDescription
        }
        activity = .idle
        mutation = nil
        await reload()
    }

    private func changeService(
        _ activity: Activity,
        _ change: @escaping @Sendable (EngineClient) async throws(EngineError) -> Void
    ) {
        guard mutation == nil, let client else { return }
        self.activity = activity
        mutation = Task {
            let result = await Self.attempt { () async throws(EngineError) in try await change(client) }
            if case .failure(let error) = result {
                problem = error.localizedDescription
            }
            // The service's plist is now the source of the folder variables
            // (or no longer is), so resolve them again.
            let environment = await resolveEnvironment(home)
            self.environment = environment
            self.client = EngineClient(executable: client.executable, environment: environment)
            self.activity = .idle
            mutation = nil
            await reload()
        }
    }

    private nonisolated static func attempt<Value: Sendable>(
        _ body: @Sendable () async throws(EngineError) -> Value
    ) async -> Result<Value, EngineError> {
        do {
            return .success(try await body())
        } catch {
            return .failure(error)
        }
    }
}
