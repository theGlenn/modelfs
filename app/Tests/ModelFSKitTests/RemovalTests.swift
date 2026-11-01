import Foundation
import Testing
@testable import ModelFSKit

@Suite struct RemovalTests {
    func bytes(_ value: UInt64) -> String {
        value == 0 ? "0 KB" : "\(value) B"
    }

    @Test func sharedCopiesFreeAboutNothingAndSaySo() {
        let frees = Frees(now: 0, afterTrash: 0, pinned: 500)

        #expect(frees.summary(bytes: bytes) == "Frees about 0 KB: other apps share these bytes.")
    }

    @Test func trashedBytesWaitForTheTrash() {
        let frees = Frees(now: 100, afterTrash: 200, pinned: 0)

        #expect(frees.summary(bytes: bytes) == "Frees about 100 B now and 200 B once you empty the Trash.")
        #expect(
            Frees(now: 0, afterTrash: 200, pinned: 0).summary(bytes: bytes)
                == "Frees about 200 B once you empty the Trash."
        )
    }

    @Test func planFixturesDecodeAndGroupByApp() throws {
        let planned = try decodeLine(try fixture("rm-plan.json"), as: RemovalResult.self)
        let removed = try decodeLine(try fixture("rm-removed.json"), as: RemovalResult.self)

        guard case .result(let plan) = planned, case .result(let done) = removed else {
            Issue.record("expected results, got \(planned) and \(removed)")
            return
        }
        #expect(plan.status == .planned)
        #expect(plan.plan.refusals.first?.code == .inUse)
        #expect(plan.plan.refusals.first?.advice.contains("LM Studio") == true)
        #expect(plan.plan.removesEverything)
        #expect(
            plan.plan.changes.map(\.summary) == [
                "Ollama deletes the tag qwen3.5:0.8b",
                "Move the model folder to the Trash",
                "Move the file to the Trash",
                "Delete the stored copy",
            ]
        )
        #expect(plan.plan.changes.map(\.app) == [.ollama, .lmstudio, .manual, nil])
        #expect(done.status == .removed)
        #expect(done.outcome?.completed == [0, 1, 2, 3])
    }

    @Test func notesOfOneKindMergeIntoOneLine() throws {
        guard case .result(let planned) = try decodeLine(try fixture("rm-plan.json"), as: RemovalResult.self) else {
            Issue.record("expected a plan")
            return
        }
        var plan = planned.plan
        plan.notes = [
            RemovalNote(code: .symlinks, path: "/m/a.gguf", message: "1 symlink points here", paths: ["/p/a"]),
            RemovalNote(code: .gitRepository, path: "/r/c.gguf", message: "Inside the git repository /r", paths: ["/r"]),
            RemovalNote(code: .symlinks, path: "/m/b.gguf", message: "1 symlink points here", paths: ["/p/b"]),
        ]

        let summaries = plan.noteSummaries

        #expect(
            summaries.map(\.text) == [
                "2 files have symlinks pointing at them; those links will stop working.",
                "Inside the git repository /r",
            ]
        )
        #expect(summaries.first?.paths == ["/m/a.gguf", "/m/b.gguf", "/p/a", "/p/b"])
    }

    @Test func aRefusalConcernsTheModelWhoseCopyItNames() throws {
        guard case .result(let inventory) = try decodeLine(try fixture("ls.json"), as: Inventory.self) else {
            Issue.record("expected an inventory")
            return
        }
        let qwen = try #require(inventory.models.first)
        let minilm = try #require(inventory.models.last)
        let path = try #require(qwen.references.first?.path)
        let refusal = RemovalRefusal(code: .inUse, path: path, message: "open", processes: [])
        let unknown = RemovalRefusal(code: .unknownModel, path: minilm.digest, message: "unknown", processes: [])

        #expect(refusal.concerns(qwen))
        #expect(!refusal.concerns(minilm))
        #expect(unknown.concerns(minilm))
    }

    @Test func aChangedPlanIsReturnedNotThrown() async throws {
        let engine = try FakeEngine()

        let result = try await EngineProcess.run(
            engine.invocation("rm-changed"),
            as: RemovalResult.self,
            acceptedStatuses: [0, 1, 3]
        )

        #expect(result.status == .planChanged)
        #expect(result.outcome == nil)
    }
}
