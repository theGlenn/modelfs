import Foundation
import Testing
@testable import ModelFSKit

@MainActor
@Suite struct BatchRemovalTests {
    @Test func severalModelsArePlannedTogetherEverywhere() async throws {
        let engine = try FakeEngine()
        let model = model(engine)
        await model.start()
        let models = try #require(model.inventory?.models)
        try #require(models.count == 2)

        let options = await model.removalOptions(for: models)

        #expect(options.map(\.scope) == [.everywhere])
        let rm = try #require(engine.arguments.firstIndex(of: "rm"))
        #expect(
            engine.arguments[rm...].prefix(6)
                == ["rm", models[0].digest, models[1].digest, "--everywhere", "--dry-run", "--json"]
        )
    }

    @Test func removingSeveralModelsRunsTheirPlanAsOne() async throws {
        let engine = try FakeEngine()
        let model = model(engine)
        await model.start()
        let models = try #require(model.inventory?.models)
        let options = await model.removalOptions(for: models)
        guard case .success(let planned) = options.first?.plan else {
            Issue.record("expected a plan, got \(options)")
            return
        }

        let result = await model.remove(models, scope: .everywhere, plan: planned.plan)

        #expect((try? result?.get())?.status == .removed)
        let run = try #require(engine.arguments.lastIndex(of: "rm"))
        #expect(
            engine.arguments[run...].prefix(7)
                == ["rm", models[0].digest, models[1].digest, "--everywhere", "--expect-plan", planned.plan.planID, "--json"]
        )
        #expect(model.activity == .idle)
    }

    @Test func oneModelHeldByManyAppsStillOffersEachApp() async throws {
        let engine = try FakeEngine()
        let model = model(engine)
        await model.start()
        let shared = try #require(model.inventory?.models.first { $0.providers.count > 1 })

        let options = await model.removalOptions(for: [shared])

        #expect(options.map(\.scope) == shared.providers.map(RemovalScope.app) + [.everywhere])
    }

    private func model(_ engine: FakeEngine) -> AppModel {
        let executable = engine.script
        let environment = EngineEnvironment(
            home: engine.directory.url.path,
            variables: engine.environment("result"),
            source: .loginShell
        )
        return AppModel(
            home: engine.directory.url,
            locateEngine: { executable },
            resolveEnvironment: { _ in environment }
        )
    }
}
