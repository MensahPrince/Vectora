//
//  TimelineScrubUITests.swift
//  cutlass-ios-macosUITests
//
//  Scrub-from-anywhere regression tests: dragging horizontally on any
//  timeline row (ruler, main clips, lanes, empty bands) must move the
//  playhead. XCUITest drags use real UIKit touch delivery, so gesture
//  arbitration matches a human finger (unlike synthesized HID swipes).
//

import XCTest

final class TimelineScrubUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    @MainActor
    private func launchEditor(_ variant: String) -> (XCUIApplication, XCUIElement) {
        let app = XCUIApplication()
        app.launchArguments = ["-startScreen", variant]
        app.launch()
        let readout = app.staticTexts["playheadReadout"]
        XCTAssertTrue(readout.waitForExistence(timeout: 5), "playhead readout should exist")
        return (app, readout)
    }

    @MainActor
    private func drag(_ app: XCUIApplication, y: CGFloat, fromX: CGFloat = 0.75, toX: CGFloat = 0.35) {
        let window = app.windows.firstMatch
        let height = window.frame.height
        let from = window.coordinate(withNormalizedOffset: CGVector(dx: fromX, dy: y / height))
        let to = window.coordinate(withNormalizedOffset: CGVector(dx: toX, dy: y / height))
        from.press(forDuration: 0.05, thenDragTo: to, withVelocity: 400, thenHoldForDuration: 0.1)
        Thread.sleep(forTimeInterval: 1.2)
    }

    /// Default editor (empty effect/overlay bands, one audio row from the
    /// project's extracted music? none) — regions: ruler, clips, bands.
    @MainActor
    func testScrubFromEveryTimelineRegion() throws {
        let (app, readout) = launchEditor("editor")

        // Rows on a 956pt window: ruler ~710, main track 750-816,
        // empty overlay band ~830, area below ~860.
        let regions: [(name: String, y: CGFloat)] = [
            ("ruler", 710),
            ("main clip upper", 770),
            ("main clip waveform", 800),
            ("empty overlay band", 830),
            ("below content", 860),
        ]

        for region in regions {
            let before = readout.label
            drag(app, y: region.y)
            XCTAssertNotEqual(
                before, readout.label,
                "scrubbing from \(region.name) (y=\(region.y)pt) should move the playhead"
            )
        }
    }

    /// Collapsed timeline: a vertical drag on the lane area must pan the
    /// lane stack (not scrub, not trigger toolbar buttons below), and the
    /// playhead must hold still.
    @MainActor
    func testVerticalLanePanWhileCollapsed() throws {
        let (app, readout) = launchEditor("editorLanes")

        let timeline = app.otherElements["timeline"]
        XCTAssertTrue(timeline.waitForExistence(timeout: 3), "timeline element should exist")

        // Collapse via the grab bar: drag it down well past minHeight.
        let handle = app.otherElements["timelineHeightHandle"]
        XCTAssertTrue(handle.exists, "height handle should exist")
        let handleStart = handle.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        handleStart.press(forDuration: 0.05, thenDragTo: handleStart.withOffset(CGVector(dx: 0, dy: 400)))
        Thread.sleep(forTimeInterval: 0.5)

        let frame = timeline.frame
        XCTAssertLessThan(frame.height, 130, "timeline should be collapsed near minHeight")

        // Vertical drag up from the main-track row: pans lanes, playhead frozen.
        let before = readout.label
        let window = app.windows.firstMatch
        let midY = frame.minY + frame.height * 0.6
        let from = window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: midY / window.frame.height))
        from.press(forDuration: 0.05, thenDragTo: from.withOffset(CGVector(dx: 0, dy: -60)), withVelocity: 300, thenHoldForDuration: 0.1)
        Thread.sleep(forTimeInterval: 0.8)

        XCTAssertEqual(before, readout.label, "vertical lane pan should not move the playhead")
        // "Faces" only exists inside the stickers panel; the toolbar label
        // "Stickers" itself always exists, so it can't be the probe.
        XCTAssertFalse(
            app.staticTexts["Faces"].exists,
            "vertical pan on the timeline must not open panels from the toolbar below"
        )
    }

    /// Cross-lane drags: a long-pressed main clip dropped on the overlay
    /// band becomes a PiP overlay; dragging that PiP up onto the main track
    /// re-inserts it as a main clip.
    @MainActor
    func testCrossLaneDragConversions() throws {
        let (app, _) = launchEditor("editorLanes")

        let timeline = app.otherElements["timeline"]
        XCTAssertTrue(timeline.waitForExistence(timeout: 3), "timeline element should exist")
        let frame = timeline.frame
        let window = app.windows.firstMatch

        // PiP lane bars are labeled "Overlay"; count is the conversion probe.
        // Scoped to the timeline: the media toolbar below also has an
        // "Overlay" button, and it swaps away when selection changes.
        let overlayBars = timeline.staticTexts.matching(NSPredicate(format: "label == 'Overlay'"))
        let initialCount = overlayBars.count

        func point(_ x: CGFloat, _ y: CGFloat) -> XCUICoordinate {
            window.coordinate(withNormalizedOffset: CGVector(
                dx: x,
                dy: y / window.frame.height
            ))
        }

        // Row centers (one overlay row at launch): ruler 18+5, effects 30+5,
        // main 66+5, overlay 30.
        let mainY = frame.minY + 23 + 35 + 33
        let overlayY = frame.minY + 23 + 35 + 71 + 15

        // 1. Long-press a main clip (right of the centered playhead at t=0)
        //    and drag it down onto the overlay band.
        point(0.65, mainY).press(
            forDuration: 0.7,
            thenDragTo: point(0.75, overlayY),
            withVelocity: 100,
            thenHoldForDuration: 0.4
        )
        Thread.sleep(forTimeInterval: 1.0)

        XCTAssertEqual(
            overlayBars.count, initialCount + 1,
            "dropping a main clip on the overlay band should create a PiP overlay"
        )

        // 2. Drag the new PiP (the lowest 'Overlay' bar; it packed into a new
        //    sub-row) up onto the main track band. The timeline grew a row in
        //    step 1, so the main track's y comes from the fresh frame.
        var newest = overlayBars.element(boundBy: 0)
        for index in 0..<overlayBars.count {
            let candidate = overlayBars.element(boundBy: index)
            if candidate.frame.minY > newest.frame.minY {
                newest = candidate
            }
        }
        let freshMainY = timeline.frame.minY + 23 + 35 + 33
        let from = newest.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let to = point(0.7, freshMainY)
        from.press(forDuration: 0.7, thenDragTo: to, withVelocity: 100, thenHoldForDuration: 0.4)
        Thread.sleep(forTimeInterval: 1.0)

        XCTAssertEqual(
            overlayBars.count, initialCount,
            "dropping a PiP overlay on the main track should convert it back to a main clip"
        )
    }

    /// Fully-populated lanes (effects row above, sticker overlay row and
    /// audio row below the main track): scrubbing must work when the drag
    /// starts on lane clips themselves. Rows are derived from the timeline's
    /// actual frame so the coordinates stay honest as the layout grows.
    @MainActor
    func testScrubFromPopulatedLanes() throws {
        let (app, readout) = launchEditor("editorLanes")

        let timeline = app.otherElements["timeline"]
        XCTAssertTrue(timeline.waitForExistence(timeout: 3), "timeline element should exist")
        let frame = timeline.frame

        // Rows from the timeline's top edge: ruler 18 + 5 spacing, effects
        // row 30 + 5, main track 66 + 5, overlay row 30 + 5, audio row 30.
        let rows: [(name: String, offset: CGFloat)] = [
            ("effects lane clip", 18 + 5 + 15),
            ("main clip", 23 + 35 + 33),
            ("overlay lane clip", 23 + 35 + 71 + 15),
            ("audio lane clip", 23 + 35 + 71 + 35 + 15),
        ]

        for row in rows {
            let y = frame.minY + row.offset
            guard y < frame.maxY - 4 else { continue }
            let before = readout.label
            drag(app, y: y)
            XCTAssertNotEqual(
                before, readout.label,
                "scrubbing from \(row.name) (y=\(y)pt) should move the playhead"
            )
        }
    }
}
