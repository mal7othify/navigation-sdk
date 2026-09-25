import XCTest
@testable import NavSDK

/// Replays the bundled fixtures through the real native core.
final class ReplayArrivalTests: XCTestCase {
    func testCoreLinks() {
        XCTAssertFalse(coreVersion().isEmpty)
    }

    @MainActor
    func testRouteSimpleReachesArrival() async throws {
        let fixture = try Fixture(name: "route_simple", bundle: .module)
        let session = try NavigationSession(route: fixture.route)
        var last: TripState?
        for raw in fixture.trace {
            last = try await session.updateAndWait(raw)
        }
        XCTAssertEqual(last?.progress, .arrived)
        XCTAssertEqual(session.state, last)
        XCTAssertEqual(session.rerouteRequests, 0)
    }

    @MainActor
    func testRouteDetourRequestsRerouteAndStillArrives() async throws {
        let fixture = try Fixture(name: "route_detour", bundle: .module)
        let session = try NavigationSession(route: fixture.route)
        var sawOffRoute = false
        var last: TripState?
        for raw in fixture.trace {
            last = try await session.updateAndWait(raw)
            sawOffRoute = sawOffRoute || (last?.isOffRoute ?? false)
        }
        XCTAssertTrue(sawOffRoute)
        XCTAssertGreaterThanOrEqual(session.rerouteRequests, 1)
        XCTAssertEqual(last?.progress, .arrived)
    }

    @MainActor
    func testStaleFixIsReportedNotApplied() async throws {
        let fixture = try Fixture(name: "route_simple", bundle: .module)
        let session = try NavigationSession(route: fixture.route)
        let first = try await session.updateAndWait(fixture.trace[5])
        do {
            _ = try await session.updateAndWait(fixture.trace[3])
            XCTFail("stale fix must throw")
        } catch let error as NavError {
            guard case .StaleLocation = error else { return XCTFail("unexpected \(error)") }
        }
        XCTAssertEqual(session.state, first)
    }
}
