import XCTest
@testable import NavCorePush

/// Replays the bundled fixtures through the real native core via BoltFFI
/// callbacks (Approach B).
final class PushReplayTests: XCTestCase {
    func testCoreLinks() {
        XCTAssertFalse(coreVersion().isEmpty)
    }

    @MainActor
    func testReplayReachesArrivalThroughCallbacks() async throws {
        let fixture = try Fixture(name: "route_simple", bundle: .module)
        let session = try NavigationSessionPush(route: fixture.route)
        for raw in fixture.trace {
            let position = await session.updateAndWait(raw)
            XCTAssertNotNil(position)
        }
        XCTAssertEqual(session.position?.progress, .arrived)
        XCTAssertEqual(session.trip?.progress, .arrived)
        XCTAssertEqual(session.positionCallbacks, fixture.trace.count)
        let steps = fixture.route.steps.count
        XCTAssertGreaterThanOrEqual(session.tripCallbacks, steps)
        XCTAssertLessThanOrEqual(session.tripCallbacks, steps + 1)
        XCTAssertEqual(session.rerouteRequests, 0)
        XCTAssertEqual(session.session.routesInstalled, 0)
    }

    @MainActor
    func testStaleFixIsReportedNotApplied() async throws {
        let fixture = try Fixture(name: "route_simple", bundle: .module)
        let session = try NavigationSessionPush(route: fixture.route)
        let first = await session.updateAndWait(fixture.trace[5])
        XCTAssertNotNil(first)
        let rejected = await session.updateAndWait(fixture.trace[3])
        XCTAssertNil(rejected)
        XCTAssertEqual(session.lastError?.kind, .staleLocation)
        XCTAssertEqual(session.position, first)
    }

    @MainActor
    func testDetourIsReplannedByTheCore() async throws {
        let fixture = try Fixture(name: "route_detour", bundle: .module)
        let counter = Counter()
        let route = fixture.route
        let session = try NavigationSessionPush(route: route) { request in
            XCTAssertEqual(request.reason, .offRoute)
            await counter.increment()
            return route
        }
        var sawOffRoute = false
        for raw in fixture.trace {
            _ = await session.updateAndWait(raw)
            sawOffRoute = sawOffRoute || (session.position?.isOffRoute ?? false)
            var spins = 0
            while session.session.replanInFlight, spins < 500 {
                try await Task.sleep(nanoseconds: 10_000_000)
                await session.settle()
                spins += 1
            }
        }
        XCTAssertTrue(sawOffRoute)
        XCTAssertGreaterThanOrEqual(session.rerouteRequests, 1)
        XCTAssertGreaterThanOrEqual(session.session.routesInstalled, 1)
        XCTAssertGreaterThanOrEqual(session.session.replansSucceeded, 1)
        XCTAssertEqual(session.session.replansFailed, 0)
        let replans = await counter.value
        XCTAssertGreaterThanOrEqual(replans, 1)
    }

    @MainActor
    func testSimulatorDrivesControllerToArrival() async throws {
        let fixture = try Fixture(name: "route_simple", bundle: .module)
        let sim = try RouteSimulator(route: fixture.route, config: SimulatorConfig())
        let session = try NavigationSessionPush(route: fixture.route)
        for _ in 0..<5000 {
            _ = await session.updateAndWait(sim.nextFix())
            if sim.isFinished(), session.position?.progress == .arrived { break }
        }
        XCTAssertEqual(session.position?.progress, .arrived)
    }
}

private actor Counter {
    private(set) var value = 0
    func increment() { value += 1 }
}
