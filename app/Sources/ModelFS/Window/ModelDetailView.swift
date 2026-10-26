import AppKit
import ModelFSKit
import SwiftUI

/// One model: what it is, where every copy is, and how much they share.
struct ModelDetailView: View {
    @Environment(AppModel.self) private var app
    @State private var removing = false
    let model: Model

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                header
                space
                copies
                stored
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .safeAreaInset(edge: .bottom) {
            HStack {
                Spacer()
                Button("Remove…", role: .destructive) { removing = true }
                    .disabled(app.activity.isBusy)
            }
            .padding(12)
            .background(.bar)
        }
        .sheet(isPresented: $removing) {
            RemoveSheet(target: model)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(model.name)
                .font(.title2.weight(.semibold))
                .textSelection(.enabled)
            if !model.facts.isEmpty {
                Text(model.facts).foregroundStyle(.secondary)
            }
            HStack {
                Text(model.size.bytes).monospacedDigit()
                if model.kind == .asset {
                    Badge(text: "Asset", tint: .secondary)
                }
                Badge(text: model.status.label, tint: model.status.tint)
            }
            HStack(spacing: 6) {
                Text(model.digest)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Button {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(model.digest, forType: .string)
                } label: {
                    Image(systemName: "doc.on.doc")
                }
                .buttonStyle(.borderless)
                .help("Copy digest")
            }
        }
    }

    private var space: some View {
        GroupBox("Disk space") {
            Grid(alignment: .leading, verticalSpacing: 4) {
                row("On disk", model.space.onDisk)
                row("Saved by sharing", model.space.saved)
                row("Can free", model.space.pending)
                if model.space.storeOnly > 0 {
                    row("Kept only by ModelFS", model.space.storeOnly)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func row(_ title: String, _ bytes: UInt64) -> some View {
        GridRow {
            Text(title).foregroundStyle(.secondary)
            Text(bytes.bytes).monospacedDigit().gridColumnAlignment(.trailing)
        }
    }

    private var copies: some View {
        GroupBox("Copies in apps") {
            if model.references.isEmpty {
                Text("No app uses this model anymore.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(model.references) { reference in
                        CopyRow(reference: reference)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private var stored: some View {
        GroupBox("Stored copy") {
            VStack(alignment: .leading, spacing: 6) {
                Text(
                    model.stored.present
                        ? "ModelFS keeps one copy so apps can share it. It costs no extra space while an app's copy is a clone of it."
                        : "The stored copy is missing; the next scan restores it."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                PathLine(path: model.stored.path, exists: model.stored.present)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// One app's copy: which app, where from, where on disk, and its state.
private struct CopyRow: View {
    let reference: Reference

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                ProviderLabel(provider: reference.provider)
                    .fontWeight(.medium)
                Spacer()
                Badge(text: reference.state.label, tint: reference.state.tint)
                    .help(reference.state.help)
            }
            if let source = reference.source {
                Text(source)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            PathLine(path: reference.path, exists: reference.state != .missing)
        }
    }
}

/// A path with a Reveal in Finder button.
private struct PathLine: View {
    let path: String
    let exists: Bool

    var body: some View {
        HStack(spacing: 6) {
            Text(path)
                .font(.caption.monospaced())
                .lineLimit(2)
                .truncationMode(.middle)
                .textSelection(.enabled)
            Spacer(minLength: 0)
            Button {
                revealInFinder(path)
            } label: {
                Image(systemName: "magnifyingglass.circle")
            }
            .buttonStyle(.borderless)
            .disabled(!exists)
            .help("Reveal in Finder")
        }
    }
}
