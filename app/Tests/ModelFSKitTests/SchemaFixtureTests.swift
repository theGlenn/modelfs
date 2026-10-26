import Foundation
import Testing
@testable import ModelFSKit

/// The app must read every example the engine's golden tests pin down.
@Suite struct SchemaFixtureTests {
    @Test func everyFixtureFileHasADecodingTest() throws {
        let names = try FileManager.default.contentsOfDirectory(atPath: schemaDirectory.path).sorted()

        #expect(
            names == [
                "daemon-install.json", "daemon-status.json", "daemon-uninstall.json",
                "doctor.json", "error.json", "ls.json", "pass.jsonl",
                "rm-plan.json", "rm-removed.json", "watch.json",
            ]
        )
    }

    @Test func inventoryDecodesWithStatesAndTotals() throws {
        let message = try decodeLine(try fixture("ls.json"), as: Inventory.self)

        guard case .result(let inventory) = message else {
            Issue.record("expected a result, got \(message)")
            return
        }
        #expect(inventory.models.count == 2)
        let qwen = try #require(inventory.models.first)
        #expect(qwen.kind == .model)
        #expect(qwen.references.map(\.state) == [.clone, .clone, .copy])
        #expect(qwen.references.first?.provider == .lmstudio)
        #expect(qwen.status == .canDeduplicate)
        let minilm = try #require(inventory.models.last)
        #expect(minilm.references.first?.tags == ["all-minilm:22m", "all-minilm:latest"])
        #expect(minilm.status == .storeOnly)
        #expect(inventory.totals.saved == 532_517_120)
        #expect(inventory.totals.storeOnly == 45_949_216)
    }

    @Test func doctorDecodes() throws {
        let message = try decodeLine(try fixture("doctor.json"), as: DoctorReport.self)

        guard case .result(let doctor) = message else {
            Issue.record("expected a result, got \(message)")
            return
        }
        #expect(doctor.roots.map(\.provider) == [.ollama, .huggingface, .manual])
        #expect(doctor.roots[1].excluded.count == 1)
    }

    @Test func watchDecodesTheCanonicalFolderAndConfigPath() throws {
        let message = try decodeLine(try fixture("watch.json"), as: WatchResult.self)

        guard case .result(let result) = message else {
            Issue.record("expected a result, got \(message)")
            return
        }
        #expect(result.path == "/Users/me/models")
        #expect(result.added)
        #expect(result.config == "/Users/me/.modeld/config.toml")
    }

    @Test func daemonStatusKeepsEnvironmentNamesIntact() throws {
        let message = try decodeLine(try fixture("daemon-status.json"), as: DaemonStatus.self)

        guard case .result(let status) = message else {
            Issue.record("expected a result, got \(message)")
            return
        }
        #expect(status.state == .running)
        #expect(status.environment["HF_HOME"] == "/Volumes/Models/hf")
        #expect(status.binaryIsCurrent == true)
        #expect(status.lastPass?.date != nil)
    }

    @Test func daemonInstallAndUninstallDecode() throws {
        let install = try decodeLine(try fixture("daemon-install.json"), as: DaemonInstall.self)
        let uninstall = try decodeLine(try fixture("daemon-uninstall.json"), as: DaemonUninstall.self)

        guard case .result(let installed) = install, case .result(let removed) = uninstall else {
            Issue.record("expected results, got \(install) and \(uninstall)")
            return
        }
        #expect(installed.status.state == .running)
        #expect(removed.wasInstalled)
    }

    @Test func passLinesAreEventsThenAResult() throws {
        let lines = String(decoding: try fixture("pass.jsonl"), as: UTF8.self)
            .split(separator: "\n")
            .map { Data($0.utf8) }

        let messages = try lines.map { try decodeLine($0, as: PassReport.self) }

        var events: [PassEvent] = []
        var report: PassReport?
        for message in messages {
            switch message {
            case .event(let event): events.append(event)
            case .result(let result): report = result
            default: Issue.record("unexpected \(message)")
            }
        }
        #expect(events.first == .waitingForLock)
        #expect(events.dropFirst().first == .locked)
        #expect(report?.cloned.count == 1)
        #expect(report?.deferred.first?.settlesInSecs == 289)
    }

    @Test func errorLineCarriesItsCode() throws {
        let message = try decodeLine(try fixture("error.json"), as: Inventory.self)

        guard case .failure(let failure) = message else {
            Issue.record("expected a failure, got \(message)")
            return
        }
        #expect(failure.code == .storeUnavailable)
    }

    @Test func valuesANewerEngineAddsDecodeAsUnknown() throws {
        let line = Data(
            #"{"type":"result","schema":1,"result":{"models":[{"digest":"sha256:00","name":"x","kind":"lora","format":null,"quant":null,"params":null,"architecture":null,"size":1,"stored":{"path":"/s","present":true},"references":[{"path":"/p","provider":"jan","label":null,"source":null,"tags":[],"state":"teleported","new_field":1}],"space":{"on_disk":1,"saved":0,"pending":0,"store_only":0,"logical":1}}],"totals":{"on_disk":1,"saved":0,"pending":0,"store_only":0,"logical":1}}}"#.utf8
        )

        let message = try decodeLine(line, as: Inventory.self)

        guard case .result(let inventory) = message else {
            Issue.record("expected a result, got \(message)")
            return
        }
        let model = try #require(inventory.models.first)
        #expect(model.kind == .unknown)
        #expect(model.references.first?.provider == .unknown)
        #expect(model.references.first?.state == .unknown)
    }

    @Test func newLineTypesAndEventKindsAreIgnorable() throws {
        let notice = Data(#"{"type":"notice","schema":1,"notice":{}}"#.utf8)
        let event = Data(#"{"type":"event","schema":1,"event":{"kind":"verifying","path":"/x"}}"#.utf8)

        let decodedNotice = try decodeLine(notice, as: Inventory.self)
        let decodedEvent = try decodeLine(event, as: Inventory.self)

        guard case .unknown = decodedNotice else {
            Issue.record("expected unknown, got \(decodedNotice)")
            return
        }
        guard case .event(.unknown(let kind)) = decodedEvent else {
            Issue.record("expected an unknown event, got \(decodedEvent)")
            return
        }
        #expect(kind == "verifying")
    }

    @Test func anotherSchemaVersionIsRefused() {
        let line = Data(#"{"type":"result","schema":2,"result":{}}"#.utf8)

        #expect(throws: DecodingError.self) {
            try decodeLine(line, as: Inventory.self)
        }
    }
}
