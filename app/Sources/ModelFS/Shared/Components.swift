import ModelFSKit
import SwiftUI

/// The three numbers that matter: saved, can be freed, kept only by the store.
struct SpaceFigures: View {
    let space: Space

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
            figure("Saved", space.saved, tint: .green, help: "Disk space sharing saves right now.")
            figure(
                "Can free",
                space.pending,
                tint: .yellow,
                help: "Separate copies Deduplicate would turn into clones."
            )
            figure(
                "Stored only",
                space.storeOnly,
                tint: .purple,
                help: "Models no app uses anymore; only ModelFS keeps a copy."
            )
        }
    }

    private func figure(_ title: String, _ bytes: UInt64, tint: Color, help: String) -> some View {
        GridRow {
            Label(title, systemImage: "circle.fill")
                .labelStyle(DotLabelStyle(tint: tint))
            Text(bytes.bytes)
                .monospacedDigit()
                .fontWeight(.semibold)
                .gridColumnAlignment(.trailing)
        }
        .help(help)
    }
}

private struct DotLabelStyle: LabelStyle {
    let tint: Color

    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            Circle().fill(tint).frame(width: 8, height: 8)
            configuration.title
        }
    }
}

/// One line on what a running pass is doing, with Cancel while it can.
struct PassProgressLine: View {
    @Environment(AppModel.self) private var model
    let progress: PassProgress

    var body: some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(description)
                .lineLimit(1)
                .truncationMode(.middle)
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
            if progress.canCancel {
                Button("Cancel") { model.cancelPass() }
                    .controlSize(.small)
            }
        }
    }

    private var description: String {
        switch progress.step {
        case .starting:
            "Starting…"
        case .waitingForLock:
            "Waiting for the background service to finish…"
        case .running:
            progress.dryRun ? "Scanning model folders…" : "Reading model folders…"
        case .hashing(let path):
            "Checking \(fileName(path))"
        case .cloning(let path):
            "Sharing \(fileName(path))"
        }
    }
}

/// What the app is busy with, if anything, as one line.
struct ActivityLine: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        switch model.activity {
        case .idle:
            EmptyView()
        case .pass(let progress):
            PassProgressLine(progress: progress)
        case .installingService:
            busy("Turning on the background service…")
        case .removingService:
            busy("Turning off the background service…")
        }
    }

    private func busy(_ text: String) -> some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(text).foregroundStyle(.secondary)
        }
    }
}

/// "Running · last pass 3 min ago", or why the service is not running.
///
/// `compact` puts the last pass on a second line, for narrow places.
struct ServiceLine: View {
    let daemon: DaemonStatus?
    var compact = false

    var body: some View {
        let layout = compact ? AnyLayout(VStackLayout(alignment: .leading, spacing: 2)) : AnyLayout(HStackLayout(spacing: 6))
        layout {
            HStack(spacing: 6) {
                Circle()
                    .fill(tint)
                    .frame(width: 8, height: 8)
                Text(compact ? shortStateText : stateText)
            }
            if let lastPass = daemon?.lastPass, let date = lastPass.date {
                Text("\(compact ? "Last" : "· last") pass \(date, format: .relative(presentation: .named))")
                    .foregroundStyle(.secondary)
                    .padding(.leading, compact ? 14 : 0)
            }
        }
        .lineLimit(1)
    }

    private var needsUpdate: Bool {
        daemon.map { $0.isInstalled && $0.binaryIsCurrent == false } ?? false
    }

    private var stateText: String {
        guard let daemon else { return "Background service: checking…" }
        if needsUpdate {
            return "Background service needs an update"
        }
        return "Background service: \(daemon.state.label)"
    }

    private var shortStateText: String {
        guard let daemon else { return "Service: checking…" }
        return needsUpdate ? "Service needs an update" : "Service: \(daemon.state.label)"
    }

    private var tint: Color {
        switch daemon?.state {
        case .running: daemon?.binaryIsCurrent == false ? .orange : .green
        case .loaded, .installed: .orange
        default: .secondary
        }
    }
}

/// A warning line for the last problem, if any.
struct ProblemLine: View {
    let problem: String?

    var body: some View {
        if let problem {
            Label(problem, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
                .font(.callout)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}
