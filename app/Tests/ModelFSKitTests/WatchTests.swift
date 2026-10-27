import Foundation
import Testing
@testable import ModelFSKit

@MainActor
@Suite struct WatchTests {
    @Test func clientPassesTheFolderAsOneArgumentAndDecodesTheResult() async throws {
        let engine = try FakeEngine()
        let client = EngineClient(executable: engine.script, environment: environment(engine))
        let folder = URL(fileURLWithPath: "/custom/a folder [with] $characters")

        let result = try await client.watchFolder(folder)

        #expect(engine.arguments == ["watch", folder.path, "--json"])
        #expect(result.path == "/custom/folder")
        #expect(result.added)
        #expect(result.config == "/home/.modeld/config.toml")
    }

    @Test func addingAFolderReloadsItsRootWithoutRunningAPass() async throws {
        let engine = try FakeEngine()
        let model = model(engine)
        await model.start()

        model.watchFolder(URL(fileURLWithPath: "/custom/folder"))
        #expect(model.activity == .addingFolder)
        try await waitUntilIdle(model)

        #expect(model.lastWatchedFolder?.path == "/custom/folder")
        #expect(model.doctor?.roots.map(\.path) == ["/custom/folder"])
        #expect(model.problem == nil)
        #expect(!engine.arguments.contains("pass"))
    }

    @Test func aFailedAdditionRemainsVisibleAfterRefresh() async throws {
        let engine = try FakeEngine()
        let model = model(engine, mode: "watch-error")
        await model.start()

        model.watchFolder(URL(fileURLWithPath: "/unreadable"))
        try await waitUntilIdle(model)
        await model.reload()

        #expect(model.lastWatchedFolder == nil)
        #expect(model.problem == "folder cannot be read")
        #expect(model.inventory != nil)
    }

    @Test func anotherMutationCannotStartWhileAddingAFolder() async throws {
        let engine = try FakeEngine()
        let model = model(engine)
        await model.start()

        model.watchFolder(URL(fileURLWithPath: "/custom/folder"))
        model.runPass(dryRun: false)
        model.watchFolder(URL(fileURLWithPath: "/another/folder"))
        try await waitUntilIdle(model)

        #expect(engine.arguments.filter { $0 == "watch" }.count == 1)
        #expect(!engine.arguments.contains("pass"))
    }

    @Test func unknownFoldersPreventAddingAWatchRoot() async throws {
        let engine = try FakeEngine()
        let model = model(engine, source: .unavailable)
        await model.start()

        model.watchFolder(URL(fileURLWithPath: "/custom/folder"))

        #expect(model.activity == .idle)
        #expect(model.problem?.contains("login shell") == true)
        #expect(!engine.arguments.contains("watch"))
    }

    @Test func anExistingFolderReportsSuccessAndScanningClearsItsNotice() async throws {
        let engine = try FakeEngine()
        let model = model(engine, mode: "watch-existing")
        await model.start()

        model.watchFolder(URL(fileURLWithPath: "/custom/folder"))
        try await waitUntilIdle(model)
        #expect(model.lastWatchedFolder?.added == false)
        #expect(model.problem == nil)

        model.runPass(dryRun: true)
        #expect(model.lastWatchedFolder == nil)
        try await waitUntilIdle(model)
    }

    private func environment(_ engine: FakeEngine, mode: String = "result", source: EngineEnvironment.Source = .loginShell) -> EngineEnvironment {
        EngineEnvironment(home: engine.directory.url.path, variables: engine.environment(mode), source: source)
    }

    private func model(_ engine: FakeEngine, mode: String = "result", source: EngineEnvironment.Source = .loginShell) -> AppModel {
        let executable = engine.script
        let environment = environment(engine, mode: mode, source: source)
        return AppModel(
            home: engine.directory.url,
            locateEngine: { executable },
            resolveEnvironment: { _ in environment }
        )
    }

    private func waitUntilIdle(_ model: AppModel) async throws {
        let deadline = ContinuousClock.now + .seconds(10)
        while model.activity != .idle && ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(20))
        }
        try #require(model.activity == .idle)
    }
}
