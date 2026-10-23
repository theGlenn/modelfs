import Foundation
import Synchronization
import Testing
@testable import ModelFSKit

/// A shell script standing in for modeld; `FAKE_MODE` picks its behavior.
struct FakeEngine {
    let directory: TemporaryDirectory
    let script: URL
    let log: URL

    init() throws {
        directory = try TemporaryDirectory()
        script = directory.url.appending(path: "modeld")
        log = directory.url.appending(path: "signals.log")
        let body = #"""
        #!/bin/bash
        locked='{"type":"event","schema":1,"event":{"kind":"locked"}}'
        waiting='{"type":"event","schema":1,"event":{"kind":"waiting_for_lock"}}'
        cancelled='{"type":"error","schema":1,"error":{"code":"cancelled","message":"stopped while waiting"}}'
        result=$(tail -n 1 "$FAKE_FIXTURE")
        case "$FAKE_MODE" in
          result) echo "$locked"; echo "$result" ;;
          error) echo '{"type":"error","schema":1,"error":{"code":"store_unavailable","message":"cannot open store"}}'; exit 1 ;;
          crash) echo "thread 'main' panicked" >&2; exit 101 ;;
          garbage) echo "not json"; exit 0 ;;
          wait)
            trap 'echo interrupted >> "$FAKE_LOG"; echo "$cancelled"; exit 1' INT
            echo "$waiting"
            while true; do sleep 0.02; done ;;
          slow)
            trap 'echo interrupted >> "$FAKE_LOG"' INT
            echo "$locked"
            for i in $(seq 1 50); do sleep 0.02; done
            echo "$result" ;;
          flood)
            echo "$locked"
            for i in $(seq 1 3000); do
              echo "{\"type\":\"event\",\"schema\":1,\"event\":{\"kind\":\"hashing\",\"path\":\"/m/model-$i.gguf\",\"size\":$i}}"
            done
            echo "$result" ;;
        esac
        """#
        try body.write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
    }

    func invocation(_ mode: String) -> EngineInvocation {
        EngineInvocation(
            executable: script,
            arguments: ["pass", "--json"],
            environment: [
                "PATH": "/usr/bin:/bin",
                "FAKE_MODE": mode,
                "FAKE_FIXTURE": schemaDirectory.appending(path: "pass.jsonl").path,
                "FAKE_LOG": log.path,
            ]
        )
    }

    var signals: String {
        (try? String(contentsOf: log, encoding: .utf8)) ?? ""
    }
}

/// Collects events across tasks, in arrival order.
final class EventLog: Sendable {
    private let events = Mutex<[PassEvent]>([])

    func record(_ event: PassEvent) {
        events.withLock { $0.append(event) }
    }

    var all: [PassEvent] {
        events.withLock { $0 }
    }
}

@Suite struct EngineProcessTests {
    @Test func resultLineBecomesTheReturnValue() async throws {
        let engine = try FakeEngine()
        let heard = EventLog()

        let report = try await EngineProcess.run(
            engine.invocation("result"),
            as: PassReport.self,
            onEvent: heard.record
        )

        #expect(report.cloned.count == 1)
        #expect(heard.all == [.locked])
    }

    @Test func errorLineIsThrownWithItsCode() async throws {
        let engine = try FakeEngine()

        await #expect(throws: EngineError.failed(EngineFailure(code: .storeUnavailable, message: "cannot open store"))) {
            try await EngineProcess.run(engine.invocation("error"), as: Inventory.self)
        }
    }

    @Test func crashWithoutAResultReportsStderr() async throws {
        let engine = try FakeEngine()

        do {
            _ = try await EngineProcess.run(engine.invocation("crash"), as: Inventory.self)
            Issue.record("expected a failure")
        } catch {
            guard case .noResult(let status, let stderr) = error else {
                Issue.record("unexpected \(error)")
                return
            }
            #expect(status == 101)
            #expect(stderr.contains("panicked"))
        }
    }

    @Test func outputThatIsNotJSONIsReported() async throws {
        let engine = try FakeEngine()

        do {
            _ = try await EngineProcess.run(engine.invocation("garbage"), as: Inventory.self)
            Issue.record("expected a failure")
        } catch {
            guard case .unreadable(let line, _) = error else {
                Issue.record("unexpected \(error)")
                return
            }
            #expect(line == "not json")
        }
    }

    @Test func cancellingWhileWaitingForTheLockSendsOneInterrupt() async throws {
        let engine = try FakeEngine()
        let (waiting, signal) = AsyncStream.makeStream(of: Void.self)
        let invocation = engine.invocation("wait")
        let run = Task {
            try await EngineProcess.run(
                invocation,
                as: PassReport.self,
                interruptible: true
            ) { event in
                if event == .waitingForLock {
                    signal.yield()
                }
            }
        }
        for await _ in waiting {
            break
        }

        run.cancel()
        let outcome = await run.result

        guard case .failure(let error as EngineError) = outcome, case .failed(let failure) = error else {
            Issue.record("expected the cancelled failure, got \(outcome)")
            return
        }
        #expect(failure.code == .cancelled)
        #expect(engine.signals == "interrupted\n")
    }

    @Test func cancellingAfterLockedNeverInterruptsAndStillReturnsTheResult() async throws {
        let engine = try FakeEngine()
        let (locked, signal) = AsyncStream.makeStream(of: Void.self)
        let invocation = engine.invocation("slow")
        let run = Task {
            try await EngineProcess.run(
                invocation,
                as: PassReport.self,
                interruptible: true
            ) { event in
                if event == .locked {
                    signal.yield()
                }
            }
        }
        for await _ in locked {
            break
        }

        run.cancel()
        let report = try await run.value

        #expect(report.cloned.count == 1)
        #expect(engine.signals.isEmpty)
    }

    @Test func uninterruptibleRunIgnoresCancellation() async throws {
        let engine = try FakeEngine()
        let invocation = engine.invocation("slow")
        let run = Task {
            try await EngineProcess.run(invocation, as: PassReport.self)
        }

        run.cancel()
        let report = try await run.value

        #expect(report.cloned.count == 1)
        #expect(engine.signals.isEmpty)
    }

    @Test func thousandsOfEventsNeverStallTheEngine() async throws {
        let engine = try FakeEngine()
        let heard = EventLog()

        let report = try await EngineProcess.run(
            engine.invocation("flood"),
            as: PassReport.self,
            onEvent: heard.record
        )

        #expect(report.cloned.count == 1)
        #expect(heard.all.count == 3001)
        #expect(heard.all.last == .hashing(path: "/m/model-3000.gguf", size: 3000))
    }

    @Test func missingExecutableFailsToLaunch() async {
        let invocation = EngineInvocation(
            executable: URL(fileURLWithPath: "/nonexistent/modeld"),
            arguments: [],
            environment: [:]
        )

        do {
            _ = try await EngineProcess.run(invocation, as: Inventory.self)
            Issue.record("expected a failure")
        } catch {
            guard case .launchFailed = error else {
                Issue.record("unexpected \(error)")
                return
            }
        }
    }
}
