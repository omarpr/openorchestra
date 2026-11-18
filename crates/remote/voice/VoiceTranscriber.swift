import AVFoundation
import Foundation
import Speech

@main
struct VoiceTranscriber {
    static func main() async {
        guard #available(macOS 26.0, *), SpeechTranscriber.isAvailable else {
            exit(2)
        }
        guard let input = CommandLine.arguments.dropFirst().first else { exit(3) }
        guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: .current) else {
            exit(4)
        }
        // Availability includes downloadable models. Defer downloads to transcription.
        if input == "--available" { return }

        let transcriber = SpeechTranscriber(locale: locale, preset: .transcription)
        do {
            if let installation = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                try await installation.downloadAndInstall()
            }
        } catch {
            exit(4)
        }

        do {
            let file = try AVAudioFile(forReading: URL(fileURLWithPath: input))
            let analyzer = SpeechAnalyzer(modules: [transcriber])
            async let text = transcript(from: transcriber)
            do {
                if let end = try await analyzer.analyzeSequence(from: file) {
                    try await analyzer.finalizeAndFinish(through: end)
                } else {
                    await analyzer.cancelAndFinishNow()
                }
            } catch {
                // Close the results sequence before the async child is joined.
                await analyzer.cancelAndFinishNow()
                throw error
            }
            print(try await text)
        } catch {
            exit(3)
        }
    }

    @available(macOS 26.0, *)
    private static func transcript(from transcriber: SpeechTranscriber) async throws -> String {
        var text = AttributedString()
        for try await result in transcriber.results {
            text += result.text
        }
        return String(text.characters)
    }
}
