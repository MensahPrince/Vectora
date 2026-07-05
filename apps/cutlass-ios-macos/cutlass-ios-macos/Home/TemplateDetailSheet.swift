import SwiftUI

/// Template preview sheet: large art with the caption, mock stats, and a
/// "Use template" call to action that hands off to the media picker.
struct TemplateDetailSheet: View {
    var template: MockTemplate
    var sectionTitle: String
    var onUse: () -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 16) {
            ZStack(alignment: .topTrailing) {
                MockArtView(art: template.art, symbolSize: 64)
                    .aspectRatio(9 / 14, contentMode: .fit)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .overlay {
                        if let caption = template.caption {
                            Text(caption)
                                .font(.title3.weight(.bold))
                                .foregroundStyle(.white)
                                .multilineTextAlignment(.center)
                                .shadow(color: .black.opacity(0.6), radius: 4)
                                .padding(.horizontal, 24)
                        }
                    }
                    .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))

                Button {
                    dismiss()
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(width: 30, height: 30)
                        .background(.black.opacity(0.5), in: Circle())
                }
                .buttonStyle(.plain)
                .padding(10)
            }

            VStack(spacing: 4) {
                Text(template.caption ?? sectionTitle)
                    .font(.headline)
                    .foregroundStyle(.white)
                Text("\(sectionTitle) · 3 clips · 0:15")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }

            HStack(spacing: 22) {
                Label("12.4K", systemImage: "heart")
                Label("Trending", systemImage: "flame")
                Label("New", systemImage: "sparkles")
            }
            .font(.caption)
            .foregroundStyle(Theme.textSecondary)

            Button {
                dismiss()
                onUse()
            } label: {
                Text("Use template")
                    .font(.headline)
                    .foregroundStyle(.white)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 13)
                    .background(Theme.accent, in: Capsule())
            }
            .buttonStyle(.plain)
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.surface)
    }
}

#Preview {
    Color.black.sheet(isPresented: .constant(true)) {
        TemplateDetailSheet(
            template: MockData.templateSections[0].templates[0],
            sectionTitle: "Shorts templates",
            onUse: {}
        )
    }
}
