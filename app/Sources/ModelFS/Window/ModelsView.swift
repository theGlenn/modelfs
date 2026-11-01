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

private enum ModelLayout: String, CaseIterable, Identifiable {
    case grouped, flat

    var id: Self { self }
    var title: String { self == .grouped ? "Grouped" : "Flat" }
}

/// Models a removal sheet was opened for.
private struct RemovalBatch: Identifiable {
    let id = UUID()
    let models: [Model]
}

/// Every stored model, searchable, filterable, and sortable.
///
/// Several rows can be selected; a group row stands for every variant in
/// it. Searching, then Select all, gathers a family of models (say, every
/// Kitten model) for one removal.
struct ModelsView: View {
    @Environment(AppModel.self) private var model
    @AppStorage("models.layout") private var layout = ModelLayout.grouped
    @State private var search: String
    @State private var filter = ModelFilter.all
    @State private var sortOrder = [KeyPathComparator(\ModelTableRow.size, order: .reverse)]
    @State private var expanded: Set<String> = []
    @State private var selection: Set<ModelTableRow.ID>
    @State private var removing: RemovalBatch?
    private let selectsAllMatches: Bool

    /// `search` and `selectsAllMatches` open the view mid-task, for review snapshots.
    init(selection: Model.ID? = nil, search: String = "", selectsAllMatches: Bool = false) {
        _selection = State(initialValue: selection.map { [$0] } ?? [])
        _search = State(initialValue: search)
        self.selectsAllMatches = selectsAllMatches
    }

    var body: some View {
        Table(of: ModelTableRow.self, selection: $selection, sortOrder: $sortOrder) {
            TableColumn("Name", value: \.name) { row in
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(row.name)
                            .fontWeight(row.children == nil ? .regular : .medium)
                            .lineLimit(1)
                        if let children = row.children {
                            Text("\(children.count) variants")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    if let detail = row.detail {
                        Text(detail)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                .help(row.model?.digest ?? row.name)
            }
            .width(min: 200, ideal: 300)
            TableColumn("Format", value: \.formatLabel)
                .width(min: 60, ideal: 90)
            TableColumn("Quant", value: \.quantLabel)
                .width(min: 50, ideal: 80)
            TableColumn("Size", value: \.size) { row in
                Text(row.size.bytes).monospacedDigit()
            }
            .width(min: 70, ideal: 90)
            TableColumn("Apps") { row in
                HStack(spacing: 6) {
                    if row.providers.isEmpty {
                        Text("—").foregroundStyle(.secondary)
                    } else {
                        ForEach(row.providers, id: \.self) { provider in
                            ProviderLabel(provider: provider, compact: true)
                        }
                    }
                }
                .lineLimit(1)
                .help(row.providers.map(\.displayName).joined(separator: ", "))
            }
            .width(min: 100, ideal: 180)
            TableColumn("State", value: \.status) { row in
                Badge(text: row.statusLabel, tint: row.status.tint)
            }
            .width(min: 90, ideal: 120)
        } rows: {
            ForEach(tableRows) { row in
                if let children = row.children {
                    DisclosureTableRow(row, isExpanded: expansion(for: row.id)) {
                        ForEach(children) { child in
                            TableRow(child)
                        }
                    }
                } else {
                    TableRow(row)
                }
            }
        }
        .contextMenu(forSelectionType: ModelTableRow.ID.self) { ids in
            let models = models(in: ids)
            if !models.isEmpty {
                Button(models.count == 1 ? "Remove…" : "Remove \(models.count) models…", role: .destructive) {
                    removing = RemovalBatch(models: models)
                }
                .disabled(model.activity.isBusy)
            }
        }
        .sheet(item: $removing) { batch in
            RemoveSheet(targets: batch.models)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if showsSelectionBar {
                selectionBar
            }
        }
        .searchable(text: $search, prompt: "Name, repo, tag, or path")
        .toolbar {
            ToolbarItem {
                Picker("Layout", selection: $layout) {
                    ForEach(ModelLayout.allCases) { layout in
                        Text(layout.title).tag(layout)
                    }
                }
                .pickerStyle(.segmented)
                .frame(width: 150)
                .help("Group variants by model, or show every variant in a flat list.")
            }
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
            if matchingModels.isEmpty {
                ContentUnavailableView.search(text: search)
            }
        }
        .onAppear {
            expandSelectedGroup()
            expandSearchResults()
            if selectsAllMatches {
                selectAllMatches()
            }
        }
        .onChange(of: layout) { expandSelectedGroup() }
        .onChange(of: search) { expandSearchResults() }
        .onChange(of: filter) { expandSearchResults() }
        .onChange(of: model.inventory) { forgetRemovedSelection() }
        .navigationTitle("Models")
    }

    private var isSearching: Bool {
        !search.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private var showsSelectionBar: Bool {
        isSearching || selectedModels.count > 1
    }

    /// While searching: how many match and Select all. With a selection:
    /// what it holds and Remove.
    private var selectionBar: some View {
        let selected = selectedModels
        let matches = matchingModels
        return VStack(spacing: 0) {
            Divider()
            HStack(spacing: 12) {
                if isSearching {
                    Text("\(matches.count) \(matches.count == 1 ? "model matches" : "models match")")
                        .foregroundStyle(.secondary)
                    Button("Select all") { selectAllMatches() }
                        .disabled(matches.isEmpty || Set(matches.map(\.id)) == Set(selected.map(\.id)))
                }
                Spacer()
                if !selected.isEmpty {
                    Text("\(selected.count) selected · \(selected.reduce(0) { $0 + $1.size }.bytes)")
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                    Button(selected.count == 1 ? "Remove…" : "Remove \(selected.count) models…", role: .destructive) {
                        removing = RemovalBatch(models: selected)
                    }
                    .disabled(model.activity.isBusy)
                    .help("Plan removing the selected models from every app and the store; nothing changes until you confirm.")
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background(.bar)
        }
    }

    /// The models selected rows stand for, in table order, each once.
    private var selectedModels: [Model] {
        models(in: selection)
    }

    private func models(in ids: Set<ModelTableRow.ID>) -> [Model] {
        var seen = Set<Model.ID>()
        var models: [Model] = []
        func add(_ model: Model) {
            if seen.insert(model.id).inserted {
                models.append(model)
            }
        }
        for row in tableRows {
            if ids.contains(row.id) {
                row.model.map(add)
                row.children?.compactMap(\.model).forEach(add)
            } else {
                row.children?.filter { ids.contains($0.id) }.compactMap(\.model).forEach(add)
            }
        }
        return models
    }

    private func selectAllMatches() {
        expandSearchResults()
        selection = Set(matchingModels.map(\.id))
    }

    /// Drops removed models from the selection once the inventory refreshes.
    private func forgetRemovedSelection() {
        let known = Set((model.inventory?.models ?? []).map(\.id))
        selection = selection.filter { $0.hasPrefix("group:") || known.contains($0) }
    }

    private var matchingModels: [Model] {
        (model.inventory?.models ?? [])
            .filter { filter.includes($0) && $0.matches(search) }
    }

    private var tableRows: [ModelTableRow] {
        let rows: [ModelTableRow]
        if layout == .grouped {
            rows = ModelGroup.grouping(matchingModels).map { group in
                if group.models.count == 1, let model = group.models.first {
                    return ModelTableRow(model: model)
                }
                return ModelTableRow(group: group, sortOrder: sortOrder)
            }
        } else {
            rows = matchingModels.map { ModelTableRow(model: $0) }
        }
        return rows.sorted(using: sortOrder)
    }

    private func expansion(for id: String) -> Binding<Bool> {
        Binding(
            get: { expanded.contains(id) },
            set: { isExpanded in
                if isExpanded {
                    expanded.insert(id)
                } else {
                    expanded.remove(id)
                }
            }
        )
    }

    private func expandSearchResults() {
        guard !search.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        expanded.formUnion(ModelGroup.grouping(matchingModels).map(\.id))
    }

    private func expandSelectedGroup() {
        for group in ModelGroup.grouping(matchingModels)
        where group.models.contains(where: { selection.contains($0.id) }) {
            expanded.insert(group.id)
        }
    }

    /// The one model the inspector shows: only when one variant row is selected.
    private var selected: Model? {
        guard selection.count == 1, let id = selection.first else { return nil }
        return model.inventory?.models.first { $0.id == id }
    }

    private var inspectorShown: Binding<Bool> {
        Binding(
            get: { selected != nil },
            set: { shown in
                if !shown {
                    selection = []
                }
            }
        )
    }
}

/// Both group summaries and leaves share the table's columns and sort controls.
private struct ModelTableRow: Identifiable {
    let id: String
    let name: String
    let formatLabel: String
    let quantLabel: String
    let size: UInt64
    let providers: [Provider]
    let status: ModelStatus
    let detail: String?
    let model: Model?
    let children: [Self]?

    var statusLabel: String {
        guard let children else { return status.label }
        if status == .canDeduplicate || status == .changed {
            return status.label
        }
        if Set(children.map(\.status)).count > 1 {
            return "Mixed states"
        }
        return status == .single ? "One copy each" : status.label
    }

    init(model: Model, isVariant: Bool = false) {
        id = model.id
        name = model.name
        formatLabel = model.formatLabel
        quantLabel = model.quantLabel
        size = model.size
        providers = model.providers
        status = model.status
        self.model = model
        children = nil
        if isVariant {
            let reference = model.presentReferences.first
            let location = reference.map { reference in
                let filename = URL(fileURLWithPath: reference.path).lastPathComponent
                return [reference.source, filename].compactMap { $0 }.joined(separator: " · ")
            }
            let digest = String(model.digest.replacingOccurrences(of: "sha256:", with: "").prefix(10))
            detail = [location, digest].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
        } else {
            detail = nil
        }
    }

    init(group: ModelGroup, sortOrder: [KeyPathComparator<Self>]) {
        id = group.id
        name = group.name
        formatLabel = Self.summary(group.models.compactMap(\.format))
        quantLabel = Self.summary(group.models.compactMap(\.quant))
        size = group.size
        providers = group.providers
        status = group.models.map(\.status).min() ?? .single
        detail = nil
        model = nil
        children = group.models.map { Self(model: $0, isVariant: true) }.sorted(using: sortOrder)
    }

    private static func summary(_ values: [String]) -> String {
        let distinct = Set(values)
        return distinct.count > 1 ? "Mixed" : distinct.first ?? "—"
    }
}
