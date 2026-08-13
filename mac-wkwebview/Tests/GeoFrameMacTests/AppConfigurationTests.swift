import XCTest
@testable import GeoFrameMac

final class AppConfigurationTests: XCTestCase {
    func testDefaultsTargetLocalServerAndFullscreen() {
        let configuration = AppConfiguration.load(arguments: ["GeoFrameMac"], environment: [:])

        XCTAssertEqual(configuration.homeURL.absoluteString, "http://127.0.0.1:8080/")
        XCTAssertTrue(configuration.startsFullscreen)
        XCTAssertTrue(configuration.hidesCursor)
        XCTAssertFalse(configuration.developerMode)
    }

    func testEnvironmentConfiguration() {
        let configuration = AppConfiguration.load(
            arguments: ["GeoFrameMac"],
            environment: [
                "GEOFRAME_URL": "http://localhost:9000/?low",
                "GEOFRAME_FULLSCREEN": "false",
                "GEOFRAME_HIDE_CURSOR": "off",
                "GEOFRAME_DEVELOPER": "yes",
            ]
        )

        XCTAssertEqual(configuration.homeURL.absoluteString, "http://localhost:9000/?low")
        XCTAssertFalse(configuration.startsFullscreen)
        XCTAssertFalse(configuration.hidesCursor)
        XCTAssertTrue(configuration.developerMode)
    }

    func testCommandLineOverridesEnvironment() {
        let configuration = AppConfiguration.load(
            arguments: [
                "GeoFrameMac",
                "--url", "http://127.0.0.1:8123/",
                "--windowed",
                "--show-cursor",
                "--developer",
            ],
            environment: [
                "GEOFRAME_URL": "http://localhost:8080/",
                "GEOFRAME_FULLSCREEN": "1",
                "GEOFRAME_HIDE_CURSOR": "1",
            ]
        )

        XCTAssertEqual(configuration.homeURL.absoluteString, "http://127.0.0.1:8123/")
        XCTAssertFalse(configuration.startsFullscreen)
        XCTAssertFalse(configuration.hidesCursor)
        XCTAssertTrue(configuration.developerMode)
    }

    func testInvalidOrNonWebURLFallsBackToLocalServer() {
        let configuration = AppConfiguration.load(
            arguments: ["GeoFrameMac", "--url", "file:///tmp/index.html"],
            environment: ["GEOFRAME_URL": "not a web URL"]
        )

        XCTAssertEqual(configuration.homeURL, AppConfiguration.defaultURL)
    }
}
