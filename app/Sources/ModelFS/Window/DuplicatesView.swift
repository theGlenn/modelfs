import ModelFSKit
import SwiftUI

/// What deduplicating would free, and what sharing already saves.
struct DuplicatesView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let inventory = model.inventory ?? .empty
        List {
            SwiftUI.Section {
                if inventory.deduplicable.isEmpty {
                    Text("No separate copies left. Every duplicate already shares its disk space.")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(inventory.deduplicable) { row in
                        DuplicateRow(model: row, figure: row.space.pending, verb: "frees")
                    }
                }
            } header: {
                HStack {
                    Text("Can be deduplicated (\(model.totals.pending.bytes))")
                    Spacer()
                    Button("Deduplicate now") { model.runPass(dryRun: false) }
                        .disabled(model.activity.isBusy || inventory.deduplicable.isEmpty)
                }
            }

            SwiftUI.Section("Already sharing (saving \(model.totals.saved.bytes))") {
                if inventory.sharing.isEmpty {
                    Text("No model is shared between apps yet.")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(inventory.sharing) { row in
                        DuplicateRow(model: row, figure: row.space.saved, verb: "saves")
                    }
                }
            }
        }
        .navigationTitle("Duplicates")
    }
}

/// A model with copies in several places.
private struct DuplicateRow: View {
    let model: Model
    let figure: UInt64
    let verb: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(model.name).fontWeight(.medium)
                Text(model.size.bytes)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                Spacer()
                Text("\(verb) \(figure.bytes)")
                    .monospacedDigit()
            }
            ForEach(model.presentReferences) { reference in
                HStack(spacing: 8) {
                    Image(systemName: reference.provider.symbol)
                        .frame(width: 16)
                        .foregroundStyle(.secondary)
                    Text(reference.source ?? reference.provider.displayName)
                        .lineLimit(1)
                    Text(reference.path)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 8)
                    Badge(text: reference.state.label, tint: reference.state.tint)
                        .help(reference.state.help)
                }
                .font(.callout)
            }
        }
        .padding(.vertical, 4)
    }
}
