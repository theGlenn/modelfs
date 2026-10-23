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
    /// The last pass was cancelled while it waited for the store lock.
    public private(set) var lastPassCancelled = false
    /// Why the last change (pass, service) failed; kept until the next one.
    private var actionProblem: String?
    /// Why the last refresh could not read everything.
    private var readProblem: String?
    /// The refresh running or queued last; refreshes run one after another,
    /// so an older one can never overwrite a newer one.
    private var lastReload: Task<Void, Never>?

    /// The last thing that went wrong, worded for people.
    public var problem: String? {
        actionProblem ?? readProblem
    }

    /// Changes are refused while the model folders are unknown.
    public var canChangeFiles: Bool {
        environment?.canChangeFiles ?? false
    }

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
        guard client == nil, actionProblem == nil else { return }
        guard let executable = locateEngine() else {
            actionProblem = EngineError.notFound.localizedDescription
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

    /// Reads the folder variables again, as after fixing a shell config.
    public func retryEnvironment() {
        guard mutation == nil, let client else { return }
        mutation = Task {
            let environment = await resolveEnvironment(home)
            self.environment = environment
            self.client = EngineClient(executable: client.executable, environment: environment)
            mutation = nil
            await reload()
        }
    }

    /// Reads the inventory, folders, and service status; returns once applied.
    ///
    /// Refreshes run in the order they were asked for, one at a time, each
    /// with the client current when it starts. Results from an old
    /// environment therefore never land after newer ones.
    func reload() async {
        let previous = lastReload
        let reload = Task {
            await previous?.value
            await readEverything()
        }
        lastReload = reload
        await reload.value
    }

    /// Reads the inventory, folders, and service status in parallel.
    private func readEverything() async {
        guard let client else { return }
        async let inventory = Self.attempt { () async throws(EngineError) in try await client.inventory() }
        async let doctor = Self.attempt { () async throws(EngineError) in try await client.doctor() }
        async let daemon = Self.attempt { () async throws(EngineError) in try await client.daemonStatus() }
        let (inventoryResult, doctorResult, daemonResult) = await (inventory, doctor, daemon)
        var failures: [EngineError] = []
        record(inventoryResult, into: \.inventory, failures: &failures)
        record(doctorResult, into: \.doctor, failures: &failures)
        record(daemonResult, into: \.daemon, failures: &failures)
        readProblem = failures.first?.localizedDescription
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
        guard refuseUnlessFoldersKnown() else { return }
        activity = .pass(PassProgress(dryRun: dryRun))
        actionProblem = nil
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
        guard refuseUnlessFoldersKnown() else { return }
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

    /// Sets the problem and returns false while the folders are unknown.
    private func refuseUnlessFoldersKnown() -> Bool {
        guard !canChangeFiles else { return true }
        actionProblem =
            "ModelFS could not read your login shell's settings (HF_HOME, HF_HUB_CACHE, "
            + "OLLAMA_MODELS), so it does not know which folders to scan and changes nothing. "
            + "Retry once your shell starts quickly."
        return false
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
        case .failure(.failed(let failure)) where failure.code == .cancelled:
            lastPassCancelled = true
        case .failure(let error):
            actionProblem = error.localizedDescription
        }
        // Idle only once the refreshed numbers are showing.
        await reload()
        activity = .idle
        mutation = nil
    }

    private func changeService(
        _ activity: Activity,
        _ change: @escaping @Sendable (EngineClient) async throws(EngineError) -> Void
    ) {
        guard mutation == nil, let client else { return }
        self.activity = activity
        actionProblem = nil
        mutation = Task {
            let result = await Self.attempt { () async throws(EngineError) in try await change(client) }
            if case .failure(let error) = result {
                actionProblem = error.localizedDescription
            }
            // The service's plist is now the source of the folder variables
            // (or no longer is), so resolve them again.
            let environment = await resolveEnvironment(home)
            self.environment = environment
            self.client = EngineClient(executable: client.executable, environment: environment)
            await reload()
            self.activity = .idle
            mutation = nil
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
