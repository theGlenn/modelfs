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
                ForEach([Provider.ollama, .huggingface, .lmstudio, .xybrid], id: \.self) { provider in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            ProviderLabel(provider: provider)
                                .fontWeight(.medium)
                            Spacer()
                            if appRoots(for: provider).isEmpty {
                                Text("Not found").foregroundStyle(.secondary)
                            }
                        }
                        ForEach(appRoots(for: provider)) { root in
                            folderPath(root.path)
                        }
                    }
                    .padding(.vertical, 3)
                }
            } header: {
                Text("Apps")
            } footer: {
                Text("Model folders are found automatically when these apps have downloaded models.")
            }

            SwiftUI.Section {
                HStack {
                    Text("\(folderRoots.count) watched \(folderRoots.count == 1 ? "folder" : "folders")")
                        .foregroundStyle(.secondary)
                    Spacer()
                    AddFolderButton()
                }
                if folderRoots.isEmpty {
                    Text("Add a folder to find models outside your apps. Subfolders are included.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text("Folders")
            }

            ForEach(folderRoots) { root in
                SwiftUI.Section {
                    WatchedFolderCard(root: root, indexedFiles: indexedFiles(in: root.path))
                }
            }

            if let problems = model.doctor?.unreadable, !problems.isEmpty {
                SwiftUI.Section("Folders that could not be read") {
                    ForEach(problems) { problem in
                        Label("\(problem.path): \(problem.reason)", systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.orange)
                            .textSelection(.enabled)
                    }
                }
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

    private var folderRoots: [ScanRoot] {
        (model.doctor?.roots ?? []).filter { $0.provider == .manual }
            .sorted { $0.path.localizedStandardCompare($1.path) == .orderedAscending }
    }

    private func appRoots(for provider: Provider) -> [ScanRoot] {
        (model.doctor?.roots ?? []).filter { $0.provider == provider }
    }

    private func indexedFiles(in path: String) -> Int {
        let prefix = path.hasSuffix("/") ? path : path + "/"
        let paths = (model.inventory?.models ?? []).flatMap(\.presentReferences)
            .filter { $0.path.hasPrefix(prefix) }
            .map(\.path)
        return Set(paths).count
    }

    private func folderPath(_ path: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(path)
                .font(.callout.monospaced())
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
            Button { revealInFinder(path) } label: {
                Image(systemName: "arrow.up.right.square")
            }
            .buttonStyle(.borderless)
            .help("Reveal app folder in Finder")
            .accessibilityLabel("Reveal app folder in Finder")
        }
        .padding(.leading, 26)
    }

}

/// A folder's details stay folded until they are needed.
private struct WatchedFolderCard: View {
    let root: ScanRoot
    let indexedFiles: Int
    @State private var expanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 10) {
                Text(root.path)
                    .font(.callout.monospaced())
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                Text("Subfolders are included. New downloads are indexed after they finish settling.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                if !root.excluded.isEmpty {
                    Text("Excluded: \(root.excluded.joined(separator: ", "))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
                Button("Reveal in Finder") { revealInFinder(root.path) }
                    .controlSize(.small)
            }
            .padding(.top, 8)
            .padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            HStack(spacing: 10) {
                Image(systemName: "folder")
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 3) {
                    Text(fileName(root.path)).fontWeight(.medium)
                    Text(root.path)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .help(root.path)
                }
                Spacer(minLength: 8)
                Text("\(indexedFiles) indexed \(indexedFiles == 1 ? "file" : "files")")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize()
            }
            .padding(.vertical, 4)
        }
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
