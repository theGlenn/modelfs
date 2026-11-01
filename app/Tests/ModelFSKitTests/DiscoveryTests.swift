import Foundation
import Testing
@testable import ModelFSKit

@MainActor
@Suite struct DiscoveryTests {
    @Test func clientReportsProgressAndDecodesTheSuggestions() async throws {
        let engine = try FakeEngine()
        let client = EngineClient(executable: engine.script, environment: environment(engine))
        let heard = EventLog()

        let report = try await client.discover { heard.record(.discovering(folders: $0)) }

        #expect(engine.arguments == ["discover", "--json"])
        #expect(heard.all == [.discovering(folders: 4096)])
        #expect(report.suggestions.count == 2)
    }

    @Test func watchingAPatternSuggestionSavesThePatternAndDropsIt() async throws {
        let engine = try FakeEngine()
        let model = model(engine)
        await model.start()

        model.discoverFolders()
        let report = try await finishedReport(model)
        let pattern = try #require(report.suggestions.first)
        model.watchSuggestion(pattern)
        try await waitUntil { await MainActor.run { model.activity == .idle } }

        let watch = try #require(engine.arguments.firstIndex(of: "watch"))
        #expect(engine.arguments[watch...].prefix(4) == ["watch", "--pattern", "~/games/*/Assets", "--json"])
        #expect(model.lastWatchedFolder != nil)
        guard case .finished(let remaining) = model.discovery else {
            Issue.record("expected suggestions, got \(model.discovery)")
            return
        }
        #expect(remaining.suggestions.map(\.id) == ["/Users/me/models/whisper"])
    }

    @Test func stoppingASearchSendsOneInterruptAndKeepsNothing() async throws {
        let engine = try FakeEngine()
        let model = model(engine, mode: "discover-wait")
        await model.start()

        model.discoverFolders()
        try await waitUntil { await MainActor.run { model.discovery == .running(foldersRead: 7) } }
        model.cancelDiscovery()
        try await waitUntil { await MainActor.run { model.discovery == .idle } }

        #expect(engine.signals == "interrupted\n")
        #expect(model.problem == nil)
    }

    @Test func searchingIsNotAChangeSoAFolderCanBeAddedMeanwhile() async throws {
        let engine = try FakeEngine()
        let model = model(engine, mode: "discover-wait")
        await model.start()

        model.discoverFolders()
        model.watchFolder(URL(fileURLWithPath: "/custom/folder"))
        try await waitUntil { await MainActor.run { model.activity == .idle } }

        #expect(model.discovery.isRunning)
        #expect(model.lastWatchedFolder?.path == "/custom/folder")
        model.cancelDiscovery()
        try await waitUntil { await MainActor.run { model.discovery == .idle } }
    }

    private func finishedReport(_ model: AppModel) async throws -> DiscoveryReport {
        try await waitUntil {
            await MainActor.run {
                if case .finished = model.discovery { true } else { false }
            }
        }
        guard case .finished(let report) = model.discovery else {
            throw WaitTimedOut()
        }
        return report
    }

    private func environment(_ engine: FakeEngine, mode: String = "result") -> EngineEnvironment {
        EngineEnvironment(home: engine.directory.url.path, variables: engine.environment(mode), source: .loginShell)
    }

    private func model(_ engine: FakeEngine, mode: String = "result") -> AppModel {
        let executable = engine.script
        let environment = environment(engine, mode: mode)
        return AppModel(
            home: engine.directory.url,
            locateEngine: { executable },
            resolveEnvironment: { _ in environment }
        )
    }
}
