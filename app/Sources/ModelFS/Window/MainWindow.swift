import AppKit
import ModelFSKit
import SwiftUI

/// The sections of the main window's sidebar.
enum Section: String, CaseIterable, Identifiable {
    case models
    case duplicates
    case service

    var id: Self { self }

    var title: String {
        switch self {
        case .models: "Models"
        case .duplicates: "Duplicates"
        case .service: "Background service"
        }
    }

    var symbol: String {
        switch self {
        case .models: "square.stack.3d.up"
        case .duplicates: "square.on.square"
        case .service: "gearshape.2"
        }
    }
}

struct MainWindow: View {
    @Environment(AppModel.self) private var model
    @State private var section: Section?
    private let selection: Model.ID?

    init(section: Section = .models, selection: Model.ID? = nil) {
        _section = State(initialValue: section)
        self.selection = selection
    }

    var body: some View {
        NavigationSplitView {
            List(Section.allCases, selection: $section) { section in
                Label(section.title, systemImage: section.symbol)
                    .badge(badge(for: section))
            }
            .navigationSplitViewColumnWidth(min: 190, ideal: 210)
            .safeAreaInset(edge: .bottom) {
                VStack(alignment: .leading, spacing: 10) {
                    SpaceFigures(space: model.totals)
                    ServiceLine(daemon: model.daemon, compact: true)
                        .font(.caption)
                }
                .font(.callout)
                .padding(12)
            }
        } detail: {
            VStack(spacing: 0) {
                if model.activity.isBusy || model.problem != nil {
                    VStack(alignment: .leading, spacing: 6) {
                        ActivityLine()
                        ProblemLine(problem: model.problem)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.bar)
                    Divider()
                }
                if let watched = model.lastWatchedFolder, !model.activity.isBusy, model.problem == nil {
                    HStack(spacing: 10) {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(.green)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(watched.isPattern ? watched.path : fileName(watched.path)) is \(watched.added ? "now watched" : "already watched").")
                            Text(model.daemon?.isInstalled == true
                                 ? "Scan now to find its models, or let the background service pick them up."
                                 : "Scan now to find its models.")
                                .font(.callout)
                                .foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 8)
                        Button("Scan now") { model.runPass(dryRun: true) }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .background(.bar)
                    Divider()
                }
                detail
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .toolbar {
            ToolbarItemGroup {
                AddFolderButton()
                Button {
                    model.refresh()
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                Button {
                    model.runPass(dryRun: false)
                } label: {
                    Label("Deduplicate now", systemImage: "square.on.square.dashed")
                }
                .disabled(model.activity.isBusy || model.totals.pending == 0)
                .help("Turn separate copies into clones, freeing about \(model.totals.pending.bytes).")
            }
        }
        .onAppear {
            NSApp.setActivationPolicy(.regular)
            NSApp.activate()
        }
        .onDisappear {
            NSApp.setActivationPolicy(.accessory)
        }
    }

    @ViewBuilder
    private var detail: some View {
        if model.needsFirstScan && section != .service {
            FirstRunView()
        } else if model.inventory == nil {
            ProgressView("Reading the store…")
        } else {
            switch section ?? .models {
            case .models: ModelsView(selection: selection)
            case .duplicates: DuplicatesView()
            case .service: BackgroundServiceView()
            }
        }
    }

    private func badge(for section: Section) -> Int {
        section == .duplicates ? model.inventory?.deduplicable.count ?? 0 : 0
    }
}

/// Shown until the first scan: what scanning does, and the button.
struct FirstRunView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ContentUnavailableView {
            Label("Find your models", systemImage: "magnifyingglass")
        } description: {
            Text(
                """
                ModelFS looks through Ollama, Hugging Face, LM Studio, Xybrid, and the folders in \
                ~/.modeld/config.toml. Scanning adds each model to the ModelFS store as a clone, \
                which takes no extra disk space and changes no app's files.
                """
            )
            .frame(maxWidth: 460)
        } actions: {
            if case .pass(let progress) = model.activity {
                PassProgressLine(progress: progress)
                    .frame(maxWidth: 420)
            } else {
                Button("Scan models") { model.runPass(dryRun: true) }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .disabled(model.activity.isBusy)
            }
        }
    }
}
