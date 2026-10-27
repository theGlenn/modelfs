import ModelFSKit
import SwiftUI

/// Each duplicate and its copies stay together in a card.
struct DuplicatesView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let inventory = model.inventory ?? .empty
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 24) {
                VStack(alignment: .leading, spacing: 12) {
                    HStack(alignment: .firstTextBaseline) {
                        sectionHeading("Can deduplicate", count: inventory.deduplicable.count)
                        Spacer()
                        Button("Deduplicate now") { model.runPass(dryRun: false) }
                            .disabled(model.activity.isBusy || inventory.deduplicable.isEmpty)
                    }
                    if inventory.deduplicable.isEmpty {
                        empty("No separate copies to deduplicate.", symbol: "checkmark.circle")
                    } else {
                        ForEach(inventory.deduplicable) { row in
                            DuplicateCard(model: row, figure: row.space.pending, verb: "Can free", tint: .orange)
                        }
                    }
                }

                VStack(alignment: .leading, spacing: 12) {
                    sectionHeading("Already sharing", count: inventory.sharing.count)
                    if inventory.sharing.isEmpty {
                        empty("No copies share their disk space yet.", symbol: "square.on.square")
                    } else {
                        ForEach(inventory.sharing) { row in
                            DuplicateCard(model: row, figure: row.space.saved, verb: "Saving", tint: .green)
                        }
                    }
                }
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .navigationTitle("Duplicates")
    }

    private func sectionHeading(_ title: String, count: Int) -> some View {
        HStack(spacing: 8) {
            Text(title).font(.headline)
            Text(count.formatted())
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    private func empty(_ message: String, symbol: String) -> some View {
        Label(message, systemImage: symbol)
            .foregroundStyle(.secondary)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct DuplicateCard: View {
    let model: Model
    let figure: UInt64
    let verb: String
    let tint: Color

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .top, spacing: 16) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text(model.name)
                            .font(.headline)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(metadata)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                    VStack(alignment: .trailing, spacing: 3) {
                        Text(verb).font(.caption).foregroundStyle(.secondary)
                        Text(figure.bytes)
                            .font(.headline)
                            .monospacedDigit()
                            .foregroundStyle(tint)
                    }
                    .fixedSize()
                }
                Divider()
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(model.presentReferences) { reference in
                        copy(reference)
                    }
                }
            }
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var metadata: String {
        [model.format, model.quant, model.size.bytes, "\(model.presentReferences.count) copies"]
            .compactMap(\.self)
            .joined(separator: " · ")
    }

    private func copy(_ reference: Reference) -> some View {
        HStack(alignment: .top, spacing: 10) {
            ProviderIcon(provider: reference.provider)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(reference.provider.displayName).fontWeight(.medium)
                    if let source = reference.source, source != reference.provider.displayName {
                        Text(source)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .help(source)
                    }
                }
                Text(reference.path)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .help(reference.path)
            }
            Spacer(minLength: 4)
            Badge(text: reference.state.label, tint: reference.state.tint)
                .help(reference.state.help)
                .fixedSize()
            Button {
                revealInFinder(reference.path)
            } label: {
                Image(systemName: "arrow.up.right.square")
            }
            .buttonStyle(.borderless)
            .help("Reveal copy in Finder")
            .accessibilityLabel("Reveal \(reference.provider.displayName) copy in Finder")
        }
        .font(.callout)
    }
}
