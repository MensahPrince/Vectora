import SwiftUI

/// Home screen: purple header with quick actions, recent projects row,
/// template carousels, and a floating new-project button.
struct HomeView: View {
    var onNewProject: () -> Void
    var onBlankProject: () -> Void
    var onOpenProject: (ProjectStore.Entry) -> Void = { _ in }

    /// Template selection routed into the detail sheet, kept together so one
    /// `sheet(item:)` covers every carousel.
    private struct TemplatePick: Identifiable {
        var template: MockTemplate
        var sectionTitle: String
        var id: UUID { template.id }
    }

    @State private var projects: [ProjectStore.Entry] = []
    @State private var menuProject: ProjectStore.Entry?
    @State private var renameProject: ProjectStore.Entry?
    @State private var renameText = ""
    @State private var templatePick: TemplatePick?

    var body: some View {
        ZStack(alignment: .top) {
            // Ambient header wash; fades into the background before the fold.
            VStack(spacing: 0) {
                Theme.homeHeader
                    .frame(height: 430)
                Theme.background
            }
            .ignoresSafeArea()

            ScrollView(showsIndicators: false) {
                VStack(alignment: .leading, spacing: 28) {
                    topBar
                    QuickActionsGrid(
                        onNewProject: onNewProject,
                        onBlankProject: onBlankProject
                    )
                    if !projects.isEmpty {
                        projectsSection
                    }
                    ForEach(MockData.templateSections) { section in
                        TemplateSection(section: section) { template in
                            templatePick = TemplatePick(template: template, sectionTitle: section.title)
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 96)
            }
        }
        .onAppear { projects = ProjectStore.list() }
        .overlay(alignment: .bottomTrailing) {
            fab
        }
        .confirmationDialog(
            menuProject?.name ?? "Project",
            isPresented: Binding(
                get: { menuProject != nil },
                set: { if !$0 { menuProject = nil } }
            ),
            titleVisibility: .visible,
            presenting: menuProject
        ) { project in
            Button("Rename") { beginRename(project) }
            Button("Duplicate") { duplicate(project) }
            Button("Delete", role: .destructive) { delete(project) }
        }
        .alert("Rename project", isPresented: Binding(
            get: { renameProject != nil },
            set: { if !$0 { renameProject = nil } }
        )) {
            TextField("Project name", text: $renameText)
            Button("Cancel", role: .cancel) { renameProject = nil }
            Button("Rename") { commitRename() }
        }
        .sheet(item: $templatePick) { pick in
            TemplateDetailSheet(
                template: pick.template,
                sectionTitle: pick.sectionTitle,
                onUse: onNewProject
            )
            .presentationDetents([.large])
        }
    }

    // MARK: Project mutations (store-backed; the list re-reads after each)

    private func beginRename(_ project: ProjectStore.Entry) {
        renameText = project.name
        renameProject = project
    }

    private func commitRename() {
        guard let project = renameProject else { return }
        let trimmed = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty {
            ProjectStore.rename(id: project.id, to: trimmed)
            projects = ProjectStore.list()
        }
        renameProject = nil
    }

    private func duplicate(_ project: ProjectStore.Entry) {
        guard ProjectStore.duplicate(id: project.id) != nil else { return }
        withAnimation(.snappy(duration: 0.25)) {
            projects = ProjectStore.list()
        }
    }

    private func delete(_ project: ProjectStore.Entry) {
        ProjectStore.delete(id: project.id)
        withAnimation(.snappy(duration: 0.25)) {
            projects = ProjectStore.list()
        }
    }

    private var topBar: some View {
        HStack(spacing: 18) {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(Theme.accent)
                .frame(width: 34, height: 34)
                .overlay {
                    Text("Cu")
                        .font(.subheadline.bold())
                        .foregroundStyle(.white)
                }

            Spacer()

            Circle()
                .fill(Theme.premiumBadge)
                .frame(width: 26, height: 26)
                .overlay {
                    Image(systemName: "crown.fill")
                        .font(.system(size: 11))
                        .foregroundStyle(.white)
                }

            Image(systemName: "lightbulb")
                .font(.system(size: 17))
                .foregroundStyle(.white)

            Image(systemName: "ellipsis")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(.white)
        }
        .padding(.top, 6)
    }

    private var projectsSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Projects")
                .font(.title3.bold())
                .foregroundStyle(.white)

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(alignment: .top, spacing: 12) {
                    ForEach(projects) { project in
                        Button {
                            onOpenProject(project)
                        } label: {
                            ProjectCard(project: project, onMenu: { menuProject = project })
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("projectCard")
                        .contextMenu {
                            Button("Rename", systemImage: "pencil") { beginRename(project) }
                            Button("Duplicate", systemImage: "plus.square.on.square") { duplicate(project) }
                            Button("Delete", systemImage: "trash", role: .destructive) { delete(project) }
                        }
                    }
                }
            }
        }
    }

    private var fab: some View {
        Button(action: onNewProject) {
            Image(systemName: "plus")
                .font(.system(size: 22, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 56, height: 56)
                .background(Theme.accent, in: Circle())
                .shadow(color: .black.opacity(0.45), radius: 10, y: 4)
        }
        .buttonStyle(.plain)
        .padding(.trailing, 20)
        .padding(.bottom, 12)
    }
}

#Preview {
    HomeView(onNewProject: {}, onBlankProject: {})
}
