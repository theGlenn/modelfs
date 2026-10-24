import ModelFSKit
import SwiftUI

/// Which models the table shows.
enum ModelFilter: String, CaseIterable, Identifiable {
    case all
    case canDeduplicate
    case sharing
    case storeOnly
    case assets

    var id: Self { self }

    var title: String {
        switch self {
        case .all: "All models"
        case .canDeduplicate: "Can deduplicate"
        case .sharing: "Sharing"
        case .storeOnly: "Not in any app"
        case .assets: "Assets"
        }
    }

    func includes(_ model: Model) -> Bool {
        switch self {
        case .all: model.kind != .asset
        case .canDeduplicate: model.status == .canDeduplicate
        case .sharing: model.space.saved > 0
        case .storeOnly: model.status == .storeOnly
        case .assets: model.kind == .asset
        }
    }
}

/// Every stored model, searchable, filterable, and sortable.
struct ModelsView: View {
    @Environment(AppModel.self) private var model
    @State private var search = ""
    @State private var filter = ModelFilter.all
    @State private var sortOrder = [KeyPathComparator(\Model.size, order: .reverse)]
    @State private var selection: Model.ID?
    @State private var removing: Model?

    init(selection: Model.ID? = nil) {
        _selection = State(initialValue: selection)
    }

    var body: some View {
        Table(rows, selection: $selection, sortOrder: $sortOrder) {
            TableColumn("Name", value: \.name) { row in
                Text(row.name)
                    .help(row.digest)
            }
            .width(min: 160, ideal: 240)
            TableColumn("Format", value: \.formatLabel)
                .width(min: 60, ideal: 90)
            TableColumn("Quant", value: \.quantLabel)
                .width(min: 50, ideal: 80)
            TableColumn("Size", value: \.size) { row in
                Text(row.size.bytes).monospacedDigit()
            }
            .width(min: 70, ideal: 90)
            TableColumn("Apps") { row in
                Text(row.providers.isEmpty ? "—" : row.providers.map(\.shortName).joined(separator: ", "))
                    .foregroundStyle(row.providers.isEmpty ? .secondary : .primary)
                    .help(row.providers.map(\.displayName).joined(separator: ", "))
            }
            .width(min: 100, ideal: 180)
            TableColumn("State", value: \.status) { row in
                Badge(text: row.status.label, tint: row.status.tint)
            }
            .width(min: 90, ideal: 120)
        }
        .contextMenu(forSelectionType: Model.ID.self) { ids in
            if let id = ids.first, let row = model.inventory?.models.first(where: { $0.id == id }) {
                Button("Remove…", role: .destructive) { removing = row }
                    .disabled(model.activity.isBusy)
            }
        }
        .sheet(item: $removing) { row in
            RemoveSheet(target: row)
        }
        .searchable(text: $search, prompt: "Name, repo, tag, or path")
        .toolbar {
            ToolbarItem {
                Picker("Show", selection: $filter) {
                    ForEach(ModelFilter.allCases) { filter in
                        Text(filter.title).tag(filter)
                    }
                }
                .pickerStyle(.menu)
            }
        }
        .inspector(isPresented: inspectorShown) {
            if let selected {
                ModelDetailView(model: selected)
                    .inspectorColumnWidth(min: 320, ideal: 380, max: 520)
            }
        }
        .overlay {
            if rows.isEmpty {
                ContentUnavailableView.search(text: search)
            }
        }
        .navigationTitle("Models")
    }

    private var rows: [Model] {
        (model.inventory?.models ?? [])
            .filter { filter.includes($0) && $0.matches(search) }
            .sorted(using: sortOrder)
    }

    private var selected: Model? {
        model.inventory?.models.first { $0.id == selection }
    }

    private var inspectorShown: Binding<Bool> {
        Binding(
            get: { selection != nil },
            set: { shown in
                if !shown {
                    selection = nil
                }
            }
        )
    }
}
