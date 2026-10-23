import ModelFSKit
import SwiftUI

/// The background service: on, off, or out of date, and what it scans.
struct BackgroundServiceView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Form {
            SwiftUI.Section {
                ServiceLine(daemon: model.daemon)
                if let lastPass = model.daemon?.lastPass {
                    LabeledContent("Last pass") {
                        Text(lastPass.summary.replacingOccurrences(of: "pass: ", with: ""))
                            .foregroundStyle(lastPass.failed ? .orange : .secondary)
                    }
                }
                if model.daemon?.dryRun == true {
                    Text("Preview mode: the service logs the clones it would make but makes none.")
                        .foregroundStyle(.secondary)
                }
                Text(
                    """
                    The service runs at login. It imports new downloads once they have not changed \
                    for 5 minutes, turns duplicates into clones, and checks every 15 minutes.
                    """
                )
                .font(.callout)
                .foregroundStyle(.secondary)
                actions
            } header: {
                Text("Background service")
            }

            SwiftUI.Section {
                if let doctor = model.doctor {
                    ForEach(doctor.roots) { root in
                        LabeledContent {
                            Text(root.path)
                                .font(.callout.monospaced())
                                .textSelection(.enabled)
                        } label: {
                            Label(root.provider.displayName, systemImage: root.provider.symbol)
                        }
                    }
                    ForEach(doctor.unreadable) { problem in
                        Label("\(problem.path): \(problem.reason)", systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.orange)
                    }
                    if doctor.roots.isEmpty {
                        Text("No model folders found.").foregroundStyle(.secondary)
                    }
                }
                Text(environmentNote)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Model folders")
            }

            if let log = model.daemon?.recentLog, !log.isEmpty {
                SwiftUI.Section("Recent log") {
                    Text(log.joined(separator: "\n"))
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Background service")
    }

    @ViewBuilder
    private var actions: some View {
        let busy = model.activity.isBusy
        HStack {
            if let daemon = model.daemon, daemon.isInstalled {
                if daemon.binaryIsCurrent == false {
                    Button("Update") { model.installService() }
                        .buttonStyle(.borderedProminent)
                        .disabled(busy)
                        .help("Run this app's version of modeld in the background service.")
                }
                Button("Turn off") { model.removeService() }
                    .disabled(busy)
            } else {
                Button("Turn on") { model.installService() }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || model.daemon == nil)
            }
        }
    }

    private var environmentNote: String {
        guard let environment = model.environment else { return "" }
        let variables = environment.variables
            .sorted { $0.key < $1.key }
            .map { "\($0.key)=\($0.value)" }
            .joined(separator: ", ")
        let source = switch environment.source {
        case .backgroundService: "the background service's settings"
        case .loginShell: "your login shell"
        case .defaults: "the defaults"
        }
        if variables.isEmpty {
            return "Folder locations come from \(source); no HF_HOME, HF_HUB_CACHE, or OLLAMA_MODELS is set."
        }
        return "Folder locations come from \(source): \(variables)."
    }
}

/// Settings: where the engine is and which folder variables it uses.
struct SettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Form {
            LabeledContent("Store") {
                Text(model.storeDirectory.path)
                    .font(.callout.monospaced())
                    .textSelection(.enabled)
            }
            LabeledContent("Background service") {
                Text(model.daemon?.state.label ?? "—")
            }
            if let environment = model.environment {
                ForEach(EngineEnvironment.modelVariables, id: \.self) { name in
                    LabeledContent(name) {
                        Text(environment.variables[name] ?? "not set")
                            .font(.callout.monospaced())
                            .foregroundStyle(environment.variables[name] == nil ? .secondary : .primary)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 520)
        .padding()
    }
}
