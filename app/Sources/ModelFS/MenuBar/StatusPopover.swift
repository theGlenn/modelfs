import AppKit
import ModelFSKit
import SwiftUI

/// The menu bar window: what ModelFS saves, and the one button that matters.
struct StatusPopover: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("ModelFS").font(.headline)
                Spacer()
                Button {
                    model.refresh()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("Refresh")
            }

            if model.needsFirstScan {
                Text("ModelFS has not looked at your models yet.")
                    .foregroundStyle(.secondary)
            } else {
                SpaceFigures(space: model.totals)
            }

            ServiceLine(daemon: model.daemon, compact: true)
                .font(.callout)
            ActivityLine()
                .font(.callout)
            ProblemLine(problem: model.problem)
            if model.lastPassCancelled {
                Text("Cancelled; nothing changed.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Divider()

            HStack {
                primaryAction
                Spacer()
                Button("Open ModelFS") { openMainWindow() }
            }
            HStack {
                SettingsLink { Text("Settings…") }
                    .buttonStyle(.borderless)
                Spacer()
                Button("Quit ModelFS") { NSApp.terminate(nil) }
                    .buttonStyle(.borderless)
            }
            .font(.callout)
        }
        .padding(16)
        .frame(width: 330)
    }

    @ViewBuilder
    private var primaryAction: some View {
        if model.needsFirstScan {
            Button("Scan models") { model.runPass(dryRun: true) }
                .buttonStyle(.borderedProminent)
                .disabled(model.activity.isBusy)
        } else {
            Button("Deduplicate now") { model.runPass(dryRun: false) }
                .buttonStyle(.borderedProminent)
                .disabled(model.activity.isBusy || model.totals.pending == 0)
                .help(
                    model.totals.pending == 0
                        ? "No separate copies to share."
                        : "Turn separate copies into clones, freeing about \(model.totals.pending.bytes)."
                )
        }
    }

    private func openMainWindow() {
        openWindow(id: WindowID.main)
        NSApp.activate()
    }
}
