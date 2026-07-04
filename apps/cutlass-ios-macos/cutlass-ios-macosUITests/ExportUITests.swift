//
//  ExportUITests.swift
//  cutlass-ios-macosUITests
//
//  End-to-end export smoke: settings -> engine export job -> saved to Photos.
//  This is the only automated pass over the full iOS delivery path (render +
//  encode on the Rust thread, then PHPhotoLibrary), so it earns its runtime.
//

import XCTest

final class ExportUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    @MainActor
    func testExportSavesToPhotos() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-startScreen", "editor"]
        app.launch()

        let export = app.buttons["Export"]
        XCTAssertTrue(export.waitForExistence(timeout: 5), "top bar export pill should exist")
        export.tap()

        // Smallest offered render: 720p at 24 fps.
        let resolution = app.buttons["exportResolution-720p"]
        XCTAssertTrue(resolution.waitForExistence(timeout: 5), "export sheet should present")
        resolution.tap()
        app.buttons["exportFps-24"].tap()

        let start = app.buttons["exportStartButton"]
        XCTAssertTrue(start.waitForExistence(timeout: 3))
        start.tap()

        // The render runs on the Rust export thread; when it finishes the app
        // asks for add-only Photos access (system alert, first run per clone)
        // and then flips to the saved screen. Tap the alert from springboard —
        // interruption monitors need app interactions to fire, and stray taps
        // could dismiss the sheet.
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let allow = springboard.buttons.matching(
            NSPredicate(format: "label BEGINSWITH 'Allow' OR label CONTAINS 'Add Photos'")
        ).firstMatch
        let done = app.buttons["exportDoneButton"]

        var waited = 0.0
        while !done.exists, waited < 120 {
            if allow.exists {
                allow.tap()
            }
            Thread.sleep(forTimeInterval: 2)
            waited += 2
        }
        XCTAssertTrue(done.exists, "the export should finish and save to Photos")
        done.tap()
    }
}
