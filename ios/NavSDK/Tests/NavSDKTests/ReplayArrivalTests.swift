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
        var last: FullState?
        var tripChanges = 0
        for raw in fixture.trace {
            last = try await session.updateAndWait(raw)
            if last?.tripChanged == true { tripChanges += 1 }
        }
        XCTAssertEqual(last?.trip.progress, .arrived)
        XCTAssertEqual(last?.position.progress, .arrived)
        XCTAssertEqual(session.state, last)
        XCTAssertEqual(session.position, last?.position)
        XCTAssertEqual(session.trip, last?.trip)
        XCTAssertEqual(session.rerouteRequests, 0)
        // Trip changes: first fix, one per step, at most one more for arrival.
        XCTAssertGreaterThanOrEqual(tripChanges, fixture.route.steps.count)
        XCTAssertLessThanOrEqual(tripChanges, fixture.route.steps.count + 1)
    }

    @MainActor
    func testSimulatorDrivesNavigatorToArrival() async throws {
        let fixture = try Fixture(name: "route_simple", bundle: .module)
        let sim = try RouteSimulator(route: fixture.route, config: SimulatorConfig())
        let session = try NavigationSession(route: fixture.route)
        var last: FullState?
        for _ in 0..<5000 {
            last = try await session.updateAndWait(sim.nextFix())
            if sim.isFinished(), last?.position.progress == .arrived { break }
        }
        XCTAssertEqual(last?.position.progress, .arrived)
    }

    @MainActor
    func testReplannerInstallsRouteOnDetour() async throws {
        let fixture = try Fixture(name: "route_detour", bundle: .module)
        let counter = Counter()
        let route = fixture.route
        let session = try NavigationSession(route: route) { request in
            XCTAssertEqual(request.reason, .offRoute)
            await counter.increment()
            return route
        }
        for raw in fixture.trace {
            _ = try await session.updateAndWait(raw)
        }
        XCTAssertGreaterThanOrEqual(session.rerouteRequests, 1)
        // Let the detached replan task finish.
        for _ in 0..<50 where session.routesInstalled == 0 {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let replans = await counter.value
        XCTAssertGreaterThanOrEqual(replans, 1)
        XCTAssertGreaterThanOrEqual(session.routesInstalled, 1)
    }

    @MainActor
    func testRouteDetourRequestsRerouteAndStillArrives() async throws {
        let fixture = try Fixture(name: "route_detour", bundle: .module)
        let session = try NavigationSession(route: fixture.route)
        var sawOffRoute = false
        var last: FullState?
        for raw in fixture.trace {
            last = try await session.updateAndWait(raw)
            sawOffRoute = sawOffRoute || (last?.position.isOffRoute ?? false)
        }
        XCTAssertTrue(sawOffRoute)
        XCTAssertGreaterThanOrEqual(session.rerouteRequests, 1)
        XCTAssertEqual(last?.trip.progress, .arrived)
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

private actor Counter {
    private(set) var value = 0
    func increment() { value += 1 }
}
