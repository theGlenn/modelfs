import ModelFSKit
import SwiftUI

/// Folders ModelFS does not watch yet that hold large model files.
///
/// Searching changes nothing; watching a suggestion only updates the scan
/// configuration, like Add folder.
struct SuggestedFoldersSection: View {
    @Environment(AppModel.self) private var model
    /// Opens the first suggestion's details, for review snapshots.
    var expandsFirst = false

    var body: some View {
        SwiftUI.Section {
            switch model.discovery {
            case .idle:
                intro
            case .running(let foldersRead):
                HStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text(
                        foldersRead == 0
                            ? "Looking through your home folder…"
                            : "Looked through \(foldersRead.formatted()) folders…"
                    )
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                    Spacer()
                    Button("Stop") { model.cancelDiscovery() }
                }
            case .finished(let report):
                summary(report)
                ForEach(report.suggestions) { suggestion in
                    SuggestionRow(
                        suggestion: suggestion,
                        expanded: expandsFirst && suggestion.id == report.suggestions.first?.id
                    )
                }
                if !report.loose.isEmpty {
                    LooseFilesRow(files: report.loose)
                }
            case .failed(let reason):
                HStack {
                    Label(reason, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.orange)
                        .textSelection(.enabled)
                    Spacer()
                    Button("Try again") { model.discoverFolders() }
                }
            }
        } header: {
            Text("Suggested folders")
        }
    }

    private var intro: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Find folders with models")
                    .fontWeight(.medium)
                Text(
                    """
                    ModelFS can look through your home folder for model files of 100 MB or more \
                    that it does not watch yet, such as game assets and test fixtures. It takes a \
                    few minutes and changes nothing.
                    """
                )
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            Button("Find folders") { model.discoverFolders() }
                .buttonStyle(.borderedProminent)
        }
    }

    private func summary(_ report: DiscoveryReport) -> some View {
        let reclaimable = report.suggestions.reduce(0) { $0 + $1.reclaimable }
        return HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                if report.suggestions.isEmpty {
                    Text("Every folder with model files of \(report.minSize.bytes) or more is watched.")
                } else {
                    let count = report.suggestions.count
                    Text(
                        reclaimable > 0
                            ? "\(count) \(count == 1 ? "folder" : "folders") to watch, freeing about \(reclaimable.bytes)"
                            : "\(count) \(count == 1 ? "folder" : "folders") to watch; their models are not copies"
                    )
                    .fontWeight(.medium)
                }
                Text("Looked through \(report.foldersRead.formatted()) folders.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            Button("Search again") { model.discoverFolders() }
        }
    }
}

/// One suggestion: what it holds, what watching it frees, and the button.
private struct SuggestionRow: View {
    @Environment(AppModel.self) private var model
    let suggestion: FolderSuggestion
    @State var expanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 8) {
                if suggestion.pattern != nil {
                    Text("Matches these folders now, and the same folder in projects you add later:")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    ForEach(suggestion.folders, id: \.self) { folder in
                        Text(abbreviated(folder))
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                    }
                }
                Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 4) {
                    ForEach(suggestion.files) { file in
                        GridRow {
                            Text(relativePath(file.path))
                                .font(.caption.monospaced())
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .help(file.path)
                            Text(file.size.bytes)
                                .font(.caption)
                                .monospacedDigit()
                                .gridColumnAlignment(.trailing)
                            CopyBadge(file: file)
                        }
                    }
                }
                Text(
                    """
                    A likely copy matches the size and sampled bytes of a stored model or another \
                    file here; one already shared is a clone and frees nothing. A pass compares full \
                    contents before it changes anything.
                    """
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                Button("Reveal in Finder") { revealInFinder(suggestion.folders.first ?? "") }
                    .controlSize(.small)
            }
            .padding(.top, 8)
            .padding(.bottom, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            HStack(spacing: 10) {
                Image(systemName: suggestion.pattern == nil ? "folder" : "folder.badge.plus")
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).fontWeight(.medium)
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .help(subtitle)
                }
                Spacer(minLength: 8)
                VStack(alignment: .trailing, spacing: 3) {
                    if suggestion.reclaimable > 0 {
                        Text("Can free \(suggestion.reclaimable.bytes)")
                            .foregroundStyle(.yellow)
                    }
                    Text("\(suggestion.size.bytes) in \(suggestion.files.count) \(suggestion.files.count == 1 ? "file" : "files")")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .monospacedDigit()
                .fixedSize()
                Button("Watch") { model.watchSuggestion(suggestion) }
                    .disabled(model.activity.isBusy || !model.canChangeFiles)
                    .help(
                        suggestion.pattern == nil
                            ? "Add this folder to the folders ModelFS watches."
                            : "Add this pattern to the folders ModelFS watches."
                    )
            }
            .padding(.vertical, 4)
        }
    }

    private var title: String {
        suggestion.pattern ?? fileName(suggestion.folders.first ?? "")
    }

    private var subtitle: String {
        if suggestion.pattern != nil {
            let count = suggestion.folders.count
            return "\(count) \(count == 1 ? "folder" : "folders") now, and new projects later"
        }
        return abbreviated(suggestion.folders.first ?? "")
    }

    /// `path` below the folder holding every suggested folder.
    private func relativePath(_ path: String) -> String {
        let base = commonFolder(suggestion.folders)
        guard !base.isEmpty, path.hasPrefix(base + "/") else { return abbreviated(path) }
        return String(path.dropFirst(base.count + 1))
    }
}

/// Files no folder short of home or a project root holds.
private struct LooseFilesRow: View {
    let files: [DiscoveredFile]

    var body: some View {
        DisclosureGroup {
            VStack(alignment: .leading, spacing: 4) {
                Text(
                    """
                    These sit directly in your home folder or a project folder, which is too broad \
                    to watch. Move them into a folder of their own to add them.
                    """
                )
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                ForEach(files) { file in
                    HStack {
                        Text(abbreviated(file.path))
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                        Spacer()
                        Text(file.size.bytes).font(.caption).monospacedDigit()
                    }
                }
            }
            .padding(.top, 6)
        } label: {
            Text("\(files.count) \(files.count == 1 ? "file" : "files") not suggested")
                .foregroundStyle(.secondary)
        }
    }
}

private struct CopyBadge: View {
    let file: DiscoveredFile

    var body: some View {
        if file.likelyCopy {
            Text(file.reclaimable > 0 ? "Likely copy" : "Already shared")
                .font(.caption)
                .foregroundStyle(file.reclaimable > 0 ? .yellow : .secondary)
                .help(
                    file.reclaimable > 0
                        ? "Watching this folder would let ModelFS turn it into a clone."
                        : "Already a clone: it shares its bytes and frees nothing."
                )
        } else {
            Text("")
        }
    }
}

/// `path` with the home folder written as `~`.
private func abbreviated(_ path: String) -> String {
    (path as NSString).abbreviatingWithTildeInPath
}

/// The deepest folder holding every one of `folders`.
private func commonFolder(_ folders: [String]) -> String {
    guard var common = folders.first?.split(separator: "/", omittingEmptySubsequences: false) else {
        return ""
    }
    for folder in folders.dropFirst() {
        let parts = folder.split(separator: "/", omittingEmptySubsequences: false)
        common = Array(zip(common, parts).prefix { $0 == $1 }.map(\.0))
    }
    return common.joined(separator: "/")
}
