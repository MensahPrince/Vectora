import CutlassMobile
import SwiftUI

#if os(iOS)
import Photos
#endif

/// Export flow riding the engine's background export job: settings
/// (resolution / frame rate) -> live progress ring with cancel -> saved
/// confirmation. The engine snapshots the project when the job starts, so
/// the editor stays fully interactive behind the sheet.
///
/// The finished mp4 goes to Photos on iOS; macOS offers a save panel (the
/// job writes to a temp file either way).
struct ExportSheet: View {
    var state: EditorState

    private enum Phase: Equatable {
        case settings
        case exporting
        /// Export finished; macOS only — waiting for the user to pick where
        /// the movie goes.
        case finished
        case saved
        case failed(String)
    }

    private struct Resolution: Hashable {
        var label: String
        var detail: String
        /// Output short side in pixels (1080 ⇒ "1080p"); aspect follows the
        /// project canvas.
        var shortSide: Int
        /// Rough H.264 bitrate in megabits per second at 30 fps.
        var mbps: Double
    }

    private static let resolutions: [Resolution] = [
        Resolution(label: "720p", detail: "HD", shortSide: 720, mbps: 7),
        Resolution(label: "1080p", detail: "Full HD", shortSide: 1080, mbps: 14),
        Resolution(label: "4K", detail: "Ultra HD", shortSide: 2160, mbps: 45),
    ]
    private static let frameRates: [Int] = [24, 30, 60]

    @Environment(\.dismiss) private var dismiss
    @State private var phase: Phase = .settings
    @State private var resolution = Self.resolutions[1]
    @State private var frameRate = 30
    @State private var progress: Double = 0
    @State private var job: ExportJob?
    @State private var exportTask: Task<Void, Never>?
    @State private var savePanelPresented = false

    var body: some View {
        VStack(spacing: 0) {
            Capsule()
                .fill(Theme.textTertiary.opacity(0.5))
                .frame(width: 36, height: 4)
                .padding(.top, 10)

            switch phase {
            case .settings:
                settings
            case .exporting:
                exporting
            case .finished:
                finished
            case .saved:
                saved
            case .failed(let message):
                failed(message)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.surface)
        .presentationDetents([.medium])
        .interactiveDismissDisabled(phase == .exporting)
        .onDisappear {
            exportTask?.cancel()
            job?.cancel()
        }
    }

    // MARK: Settings

    private var settings: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Export")
                .font(.title3.bold())
                .foregroundStyle(.white)
                .frame(maxWidth: .infinity)
                .padding(.top, 14)

            VStack(alignment: .leading, spacing: 10) {
                Text("Resolution")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Theme.textSecondary)
                HStack(spacing: 10) {
                    ForEach(Self.resolutions, id: \.self) { option in
                        selectablePill(
                            title: option.label,
                            subtitle: option.detail,
                            isOn: option == resolution
                        ) {
                            resolution = option
                        }
                        .accessibilityIdentifier("exportResolution-\(option.label)")
                    }
                }
            }

            VStack(alignment: .leading, spacing: 10) {
                Text("Frame rate")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Theme.textSecondary)
                HStack(spacing: 10) {
                    ForEach(Self.frameRates, id: \.self) { fps in
                        selectablePill(
                            title: "\(fps)",
                            subtitle: "fps",
                            isOn: fps == frameRate
                        ) {
                            frameRate = fps
                        }
                        .accessibilityIdentifier("exportFps-\(fps)")
                    }
                }
            }

            HStack {
                Label(state.duration.timecode, systemImage: "clock")
                Spacer()
                Label(estimatedSize, systemImage: "internaldrive")
            }
            .font(.footnote)
            .foregroundStyle(Theme.textSecondary)

            Button {
                startExport()
            } label: {
                Text("Export")
                    .font(.headline)
                    .foregroundStyle(.black)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 13)
                    .background(.white, in: Capsule())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("exportStartButton")
            .disabled(state.isEmpty)
            .opacity(state.isEmpty ? 0.4 : 1)
        }
        .padding(.horizontal, 22)
    }

    private var estimatedSize: String {
        let mbps = resolution.mbps * (Double(frameRate) / 30)
        let megabytes = mbps * max(state.duration, 0) / 8
        if megabytes >= 1000 {
            return String(format: "~%.1f GB", megabytes / 1000)
        }
        return String(format: "~%.0f MB", max(megabytes, 1))
    }

    // MARK: Exporting

    private func startExport() {
        withAnimation(.snappy(duration: 0.2)) { phase = .exporting }
        progress = 0
        let shortSide = resolution.shortSide
        let fps = frameRate
        exportTask = Task {
            guard let job = await state.startExport(shortSide: shortSide, fps: fps) else {
                setPhase(.failed("The export could not start."))
                return
            }
            // The sheet may have disappeared while the job was starting; its
            // onDisappear saw job == nil, so this task owns the cancel.
            if Task.isCancelled {
                job.cancel()
            }
            self.job = job

            while !job.isFinished, !Task.isCancelled {
                withAnimation(.linear(duration: 0.1)) { progress = job.progress }
                try? await Task.sleep(for: .milliseconds(100))
            }
            do {
                _ = try await job.wait()
                withAnimation(.linear(duration: 0.1)) { progress = 1 }
                await deliver(URL(fileURLWithPath: job.outputPath))
            } catch let error as CutlassError where error.kind == "cancelled" {
                setPhase(.settings)
            } catch {
                setPhase(.failed(String(describing: error)))
            }
            self.job = nil
        }
    }

    /// Hand the finished movie off: straight into the photo library on iOS,
    /// a save panel on macOS.
    private func deliver(_ movie: URL) async {
        #if os(iOS)
        let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
        guard status == .authorized || status == .limited else {
            try? FileManager.default.removeItem(at: movie)
            setPhase(.failed("Allow photo library access in Settings to save exports."))
            return
        }
        do {
            try await PHPhotoLibrary.shared().performChanges {
                PHAssetChangeRequest.creationRequestForAssetFromVideo(atFileURL: movie)
            }
            try? FileManager.default.removeItem(at: movie)
            setPhase(.saved)
        } catch {
            try? FileManager.default.removeItem(at: movie)
            setPhase(.failed("Saving to Photos failed: \(error.localizedDescription)"))
        }
        #else
        exportedMovie = movie
        setPhase(.finished)
        #endif
    }

    private func setPhase(_ new: Phase) {
        withAnimation(.snappy(duration: 0.25)) { phase = new }
    }

    private var exporting: some View {
        VStack(spacing: 22) {
            ZStack {
                Circle()
                    .stroke(Theme.surfaceElevated, lineWidth: 7)
                Circle()
                    .trim(from: 0, to: progress)
                    .stroke(Theme.accent, style: StrokeStyle(lineWidth: 7, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                Text("\(Int(progress * 100))%")
                    .font(.title2.bold().monospacedDigit())
                    .foregroundStyle(.white)
                    .accessibilityIdentifier("exportProgressLabel")
            }
            .frame(width: 116, height: 116)
            .padding(.top, 36)

            VStack(spacing: 5) {
                Text("Exporting...")
                    .font(.headline)
                    .foregroundStyle(.white)
                Text("\(resolution.label) · \(frameRate) fps")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }

            Button("Cancel") {
                job?.cancel()
            }
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Theme.textSecondary)
            .buttonStyle(.plain)
            .accessibilityIdentifier("exportCancelButton")
        }
    }

    // MARK: Finished (macOS save panel)

    /// Where the export job left the movie, until the user places it.
    @State private var exportedMovie: URL?

    private var finished: some View {
        VStack(spacing: 18) {
            Image(systemName: "checkmark.seal.fill")
                .font(.system(size: 54))
                .foregroundStyle(Theme.accent)
                .padding(.top, 40)

            VStack(spacing: 5) {
                Text("Export complete")
                    .font(.headline)
                    .foregroundStyle(.white)
                Text("\(resolution.label) · \(frameRate) fps")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }

            Button {
                savePanelPresented = true
            } label: {
                Text("Save…")
                    .font(.headline)
                    .foregroundStyle(.black)
                    .padding(.horizontal, 52)
                    .padding(.vertical, 12)
                    .background(.white, in: Capsule())
            }
            .buttonStyle(.plain)
            .padding(.top, 8)
            .fileMover(isPresented: $savePanelPresented, file: exportedMovie) { result in
                if case .success = result {
                    exportedMovie = nil
                    setPhase(.saved)
                }
            }
        }
        .onDisappear {
            // Sheet dismissed without placing the file: don't litter tmp.
            if let leftover = exportedMovie {
                try? FileManager.default.removeItem(at: leftover)
            }
        }
    }

    // MARK: Saved

    private var saved: some View {
        VStack(spacing: 18) {
            Image(systemName: "checkmark.seal.fill")
                .font(.system(size: 54))
                .foregroundStyle(Theme.accent)
                .padding(.top, 40)

            VStack(spacing: 5) {
                #if os(iOS)
                Text("Saved to Photos")
                    .font(.headline)
                    .foregroundStyle(.white)
                #else
                Text("Saved")
                    .font(.headline)
                    .foregroundStyle(.white)
                #endif
                Text("\(resolution.label) · \(frameRate) fps")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }

            Button {
                dismiss()
            } label: {
                Text("Done")
                    .font(.headline)
                    .foregroundStyle(.black)
                    .padding(.horizontal, 52)
                    .padding(.vertical, 12)
                    .background(.white, in: Capsule())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("exportDoneButton")
            .padding(.top, 8)
        }
    }

    // MARK: Failed

    private func failed(_ message: String) -> some View {
        VStack(spacing: 18) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 54))
                .foregroundStyle(.yellow)
                .padding(.top, 40)

            VStack(spacing: 5) {
                Text("Export failed")
                    .font(.headline)
                    .foregroundStyle(.white)
                Text(message)
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
                    .lineLimit(3)
            }
            .padding(.horizontal, 24)

            Button("Back") {
                setPhase(.settings)
            }
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Theme.textSecondary)
            .buttonStyle(.plain)
            .padding(.top, 8)
        }
    }

    // MARK: Pieces

    private func selectablePill(
        title: String,
        subtitle: String,
        isOn: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            VStack(spacing: 2) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(isOn ? .black : .white)
                Text(subtitle)
                    .font(.caption2)
                    .foregroundStyle(isOn ? .black.opacity(0.6) : Theme.textTertiary)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 9)
            .background(
                isOn ? AnyShapeStyle(.white) : AnyShapeStyle(Theme.surfaceElevated),
                in: RoundedRectangle(cornerRadius: 11, style: .continuous)
            )
        }
        .buttonStyle(.plain)
    }
}

#Preview {
    Color.black.sheet(isPresented: .constant(true)) {
        ExportSheet(state: EditorState())
    }
}
