import Foundation
import Testing
@testable import ModelFSKit

/// AppModel against the fake engine, which answers ls, doctor, and status
/// from the schema fixtures.
@MainActor
@Suite struct AppModelTests {
    func model(engine: FakeEngine, mode: String, source: EngineEnvironment.Source) -> AppModel {
        let executable = engine.script
        let environment = EngineEnvironment(home: "/nonexistent", variables: engine.environment(mode), source: source)
        return AppModel(
            home: engine.directory.url,
            locateEngine: { executable },
            resolveEnvironment: { _ in environment }
        )
    }

    @Test func passFailureStaysVisibleAfterTheRefreshThatFollows() async throws {
        let engine = try FakeEngine()
        let model = model(engine: engine, mode: "pass-error", source: .loginShell)
        await model.start()
        #expect(model.problem == nil)

        model.runPass(dryRun: false)
        try await waitUntilIdle(model)

        #expect(model.problem == "pass failed: disk full")
        #expect(model.inventory?.models.count == 2)
    }

    @Test func nothingChangesWhileTheFoldersAreUnknown() async throws {
        let engine = try FakeEngine()
        let model = model(engine: engine, mode: "result", source: .unavailable)
        await model.start()

        model.runPass(dryRun: false)
        model.installService()

        #expect(model.activity == .idle)
        #expect(model.problem?.contains("login shell") == true)
        #expect(model.inventory != nil, "reading still works")
    }

    private func waitUntilIdle(_ model: AppModel) async throws {
        let deadline = ContinuousClock.now + .seconds(10)
        repeat {
            try await Task.sleep(for: .milliseconds(20))
        } while model.activity != .idle && ContinuousClock.now < deadline
        try #require(model.activity == .idle)
    }
}
