//
//  PersistenceUITests.swift
//  cutlass-ios-macosUITests
//
//  Home <-> editor round trip over the real project store: create a project
//  from the samples tab, let auto-save land, return Home to the saved card,
//  and reopen it into a restored session.
//

import XCTest

final class PersistenceUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    @MainActor
    func testProjectSurvivesTheHomeRoundTrip() throws {
        let app = XCUIApplication()
        app.launch()

        // Home -> picker -> one bundled sample -> editor.
        app.buttons["New from photo library"].firstMatch.tap()
        app.buttons["Samples"].tap()

        let sample = app.descendants(matching: .any)["sampleCell-demo1.mp4"].firstMatch
        XCTAssertTrue(sample.waitForExistence(timeout: 5), "samples grid should offer demo1")
        sample.tap()
        app.buttons["pickerAddButton"].tap()

        let readout = app.staticTexts["playheadReadout"]
        XCTAssertTrue(readout.waitForExistence(timeout: 5), "editor should open with the pick")

        // Auto-save debounces one second after the append lands.
        Thread.sleep(forTimeInterval: 2.5)

        // Back Home: the saved project card is listed.
        app.buttons["house"].tap()
        let card = app.buttons["projectCard"].firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 5), "the saved project should have a card")

        // Reopen: the restored session has the clip (scrubbable timeline).
        card.tap()
        XCTAssertTrue(readout.waitForExistence(timeout: 5), "the project should reopen")
        XCTAssertTrue(
            app.otherElements["timeline"].waitForExistence(timeout: 3),
            "the restored timeline should be present")
    }
}
