import SwiftUI

/// Tabbed audio browser: mock music and sound-effect lists plus a voiceover
/// recorder. Adding drops an audio clip at the playhead.
struct AudioPanel: View {
    var state: EditorState

    @State private var tab = 0

    var body: some View {
        VStack(spacing: 6) {
            PanelTabs(tabs: ["Music", "Sound FX", "Voiceover"], selection: $tab)

            switch tab {
            case 0: songList(MockData.songs, kind: .music)
            case 1: songList(MockData.soundEffects, kind: .soundFX)
            default: VoiceoverRecorder(state: state)
            }
        }
        .frame(height: 210, alignment: .top)
    }

    private func songList(_ songs: [MockSong], kind: MockAudioClip.Kind) -> some View {
        ScrollView(showsIndicators: false) {
            VStack(spacing: 2) {
                ForEach(songs) { song in
                    HStack(spacing: 12) {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(Theme.surfaceElevated)
                            .frame(width: 40, height: 40)
                            .overlay {
                                Image(systemName: kind == .music ? "music.note" : "waveform")
                                    .font(.system(size: 16))
                                    .foregroundStyle(Theme.textSecondary)
                            }

                        VStack(alignment: .leading, spacing: 2) {
                            Text(song.title)
                                .font(.subheadline)
                                .foregroundStyle(.white)
                            Text(song.artist)
                                .font(.caption)
                                .foregroundStyle(Theme.textTertiary)
                        }

                        Spacer()

                        Text(song.duration.timecode)
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(Theme.textTertiary)

                        Button {
                            state.addAudio(kind: kind, title: song.title, duration: song.duration)
                        } label: {
                            Image(systemName: "plus.circle.fill")
                                .font(.system(size: 22))
                                .foregroundStyle(Theme.accent)
                        }
                        .buttonStyle(.plain)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 7)
                }
            }
        }
    }
}

/// Fake voiceover recorder: pulsing record button, running timer, animated
/// level bars; stopping adds a voiceover clip of the recorded length.
private struct VoiceoverRecorder: View {
    var state: EditorState

    @State private var isRecording = false
    @State private var elapsed: TimeInterval = 0
    @State private var ticker: Task<Void, Never>?

    var body: some View {
        VStack(spacing: 14) {
            HStack(spacing: 3) {
                ForEach(0..<24, id: \.self) { index in
                    Capsule()
                        .fill(isRecording ? Theme.waveform : Theme.surfaceElevated)
                        .frame(width: 3, height: barHeight(index))
                }
            }
            .frame(height: 36)
            .animation(.easeInOut(duration: 0.12), value: elapsed)

            Text(isRecording ? elapsed.timecode : "Tap to record a voiceover")
                .font(.footnote.monospacedDigit())
                .foregroundStyle(isRecording ? .white : Theme.textSecondary)

            Button {
                isRecording ? stop() : start()
            } label: {
                ZStack {
                    Circle()
                        .strokeBorder(.white.opacity(0.8), lineWidth: 3)
                        .frame(width: 62, height: 62)
                    RoundedRectangle(cornerRadius: isRecording ? 6 : 24, style: .continuous)
                        .fill(Color(hex: 0xEF4444))
                        .frame(
                            width: isRecording ? 26 : 48,
                            height: isRecording ? 26 : 48
                        )
                }
                .animation(.easeOut(duration: 0.18), value: isRecording)
            }
            .buttonStyle(.plain)
        }
        .padding(.top, 8)
        .onDisappear { stop() }
    }

    private func barHeight(_ index: Int) -> CGFloat {
        guard isRecording else { return 5 }
        let phase = elapsed * 9 + Double(index) * 0.7
        return 6 + 28 * abs(sin(phase)) * (0.4 + 0.6 * abs(sin(Double(index) * 1.3)))
    }

    private func start() {
        isRecording = true
        elapsed = 0
        ticker = Task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(100))
                guard !Task.isCancelled else { return }
                elapsed += 0.1
            }
        }
    }

    private func stop() {
        ticker?.cancel()
        ticker = nil
        guard isRecording else { return }
        isRecording = false
        if elapsed >= 0.5 {
            state.addAudio(kind: .voiceover, title: "Voiceover", duration: elapsed)
        }
        elapsed = 0
    }
}
