import XCTest

final class DirectInstanceTests: XCTestCase {
    func testPairAndSendThroughDirectGateway() throws {
        continueAfterFailure = false
        let endpoint = try XCTUnwrap(ProcessInfo.processInfo.environment["ZERON_DIRECT_ENDPOINT"])
        let key = try XCTUnwrap(ProcessInfo.processInfo.environment["ZERON_DIRECT_KEY"])
        let app = XCUIApplication()
        app.launchArguments = ["-signedout", "-harness", "mock"]
        app.launch()
        XCTAssertTrue(app.buttons["connect-instance"].waitForExistence(timeout: 10))
        app.buttons["connect-instance"].tap()
        let address = app.textFields["instance-address"]
        XCTAssertTrue(address.waitForExistence(timeout: 5))
        address.tap()
        address.typeText(endpoint)
        let code = app.secureTextFields["instance-key"]
        code.tap()
        code.typeText(key)
        app.buttons["pair-instance"].tap()
        let newSession = app.buttons["new-session"]
        XCTAssertTrue(newSession.waitForExistence(timeout: 20))
        newSession.tap()
        let input = app.textViews["composer-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        input.tap()
        input.typeText("hello from my paired iPhone")
        app.buttons["composer-send"].tap()
        let reply = app.staticTexts.matching(identifier: "row-markdown").firstMatch
        XCTAssertTrue(reply.waitForExistence(timeout: 30))
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "direct-instance-roundtrip"
        shot.lifetime = .keepAlways
        add(shot)
        app.terminate()
        app.launchArguments = ["-harness", "mock"]
        app.launch()
        XCTAssertTrue(app.buttons["new-session"].waitForExistence(timeout: 15), "pairing survives a restart")
        XCTAssertFalse(app.buttons["connect-instance"].exists)
    }
}
