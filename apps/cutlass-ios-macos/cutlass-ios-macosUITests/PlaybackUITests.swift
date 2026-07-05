//
//  PlaybackUITests.swift
//  cutlass-ios-macosUITests
//
//  Transport playback smoke: pressing play must advance the playhead while
//  the audio pipeline (engine mixer reader -> ring buffer -> AVAudioEngine)
//  runs alongside. Catches realtime-audio regressions that unit tests can't:
//  the AVAudioSourceNode render callback and session activation only happen
//  in a live app process.
//

import XCTest

final class PlaybackUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// editorLanes seeds video + tone.m4a, so playback mixes real audio.
    @MainActor
    func testPlayAdvancesPlayheadWithAudibleTimeline() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-startScreen", "editorLanes"]
        app.launch()

        let readout = app.staticTexts["playheadReadout"]
        XCTAssertTrue(readout.waitForExistence(timeout: 5), "playhead readout should exist")
        let play = app.buttons["playButton"]
        XCTAssertTrue(play.waitForExistence(timeout: 3), "play button should exist")

        let before = readout.label
        play.tap()
        Thread.sleep(forTimeInterval: 2.0)

        XCTAssertNotEqual(before, readout.label, "playback should move the playhead")

        // Pause and confirm the clock stops (audio teardown doesn't wedge
        // the main actor).
        play.tap()
        Thread.sleep(forTimeInterval: 0.3)
        let paused = readout.label
        Thread.sleep(forTimeInterval: 1.0)
        XCTAssertEqual(paused, readout.label, "pause should freeze the playhead")
    }
}
