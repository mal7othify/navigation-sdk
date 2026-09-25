import NavSDK
import XCTest

/// Replays the fixture bundled with the demo app through the real native core.
final class ReplayArrivalDemoTests: XCTestCase {
    @MainActor
    func testRouteSimpleReachesArrival() async throws {
        let bundle = Bundle(for: ReplayArrivalDemoTests.self)
        let appBundle = Bundle.allBundles.first { $0.bundleIdentifier == "com.navsdk.demo" } ?? bundle
        let fixture = try Fixture(name: "route_simple", bundle: appBundle)
        let session = try NavigationSession(route: fixture.route)
        var last: TripState?
        for raw in fixture.trace {
            last = try await session.updateAndWait(raw)
        }
        XCTAssertEqual(last?.progress, .arrived)
    }
}
