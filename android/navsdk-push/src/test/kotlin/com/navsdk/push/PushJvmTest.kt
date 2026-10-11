package com.navsdk.push

import com.navsdk.push.core.GeoPoint
import com.navsdk.push.core.RawLocation
import com.navsdk.push.core.RerouteReason
import com.navsdk.push.core.Route
import com.navsdk.push.core.RouteSimulator
import com.navsdk.push.core.SimulatorConfig
import com.navsdk.push.core.TripProgress
import com.navsdk.push.core.coreVersion
import com.navsdk.push.core.routeFromJson
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.io.File

/**
 * Runs the real Rust core on the JVM through the BoltFFI bindings. The
 * generated loader extracts the desktop JNI library from test resources
 * (`boltffi/desktopJniLibs`, written by `boltffi pack android`). Skipped when
 * that library is absent.
 */
class PushJvmTest {
    private val fixtures = File(System.getProperty("navsdk.fixtures") ?: "")

    private fun requireNative() {
        val present = PushJvmTest::class.java.getResource("/darwin-arm64/libnavcore_boltffi_jni.dylib") != null ||
            PushJvmTest::class.java.getResource("/darwin-x86_64/libnavcore_boltffi_jni.dylib") != null ||
            PushJvmTest::class.java.getResource("/linux-x86_64/libnavcore_boltffi_jni.so") != null
        assumeTrue("desktop navcore_boltffi_jni library not built; run boltffi pack android in crates/navcore-boltffi", present)
    }

    private fun load(name: String): Pair<Route, List<RawLocation>> {
        val fixture = JSONObject(File(fixtures, "$name.json").readText())
        val route = routeFromJson(fixture.getJSONObject("route").toString())
        val traceJson = fixture.getJSONArray("trace")
        val trace = (0 until traceJson.length()).map { i ->
            val fix = traceJson.getJSONObject(i)
            val point = fix.getJSONObject("point")
            RawLocation(
                point = GeoPoint(point.getDouble("lat"), point.getDouble("lng")),
                accuracyM = fix.optDouble("accuracy_m").takeIf { !it.isNaN() },
                speedMps = fix.optDouble("speed_mps").takeIf { !it.isNaN() },
                courseDeg = fix.optDouble("course_deg").takeIf { !it.isNaN() },
                timestampMs = fix.getLong("timestamp_ms").toULong(),
            )
        }
        return route to trace
    }

    @Test
    fun coreLinks() {
        requireNative()
        assertTrue(coreVersion().isNotEmpty())
    }

    @Test
    fun replayReachesArrivalThroughCallbacks() = runTest {
        requireNative()
        val (route, trace) = load("route_simple")
        NavigationSessionPush(route).use { session ->
            for (raw in trace) assertNotNull(session.updateAndAwait(raw))
            assertEquals(TripProgress.ARRIVED, session.position.value!!.progress)
            assertEquals(TripProgress.ARRIVED, session.trip.value!!.progress)
            assertEquals(trace.size, session.positionCallbacks.value)
            val steps = route.steps.size
            assertTrue(session.tripCallbacks.value in steps..(steps + 1))
            assertEquals(0, session.rerouteRequests.value)
            assertEquals(0u, session.session.value.routesInstalled)
        }
    }

    @Test
    fun staleFixIsReportedNotApplied() = runTest {
        requireNative()
        val (route, trace) = load("route_simple")
        NavigationSessionPush(route).use { session ->
            val first = session.updateAndAwait(trace[5])
            assertNotNull(first)
            assertEquals(null, session.updateAndAwait(trace[3]))
            assertEquals(first, session.position.value)
        }
    }

    @Test
    fun detourIsReplannedByTheCore() = runTest {
        requireNative()
        val (route, trace) = load("route_detour")
        var replans = 0
        NavigationSessionPush(route, replanner = { request ->
            replans += 1
            assertEquals(RerouteReason.OFF_ROUTE, request.reason)
            route
        }).use { session ->
            var sawOffRoute = false
            for (raw in trace) {
                session.updateAndAwait(raw)
                sawOffRoute = sawOffRoute || (session.position.value?.isOffRoute == true)
                if (session.session.value.replanInFlight) {
                    withTimeout(5_000) { while (session.session.value.replanInFlight) delay(10) }
                }
            }
            assertTrue(sawOffRoute)
            assertTrue(session.rerouteRequests.value >= 1)
            assertTrue(session.session.value.routesInstalled >= 1u)
            assertTrue(session.session.value.replansSucceeded >= 1u)
            assertEquals(0u, session.session.value.replansFailed)
        }
        assertTrue(replans >= 1)
    }

    @Test
    fun simulatorDrivesControllerToArrival() = runTest {
        requireNative()
        val (route, _) = load("route_simple")
        val sim = RouteSimulator(route, SimulatorConfig())
        NavigationSessionPush(route).use { session ->
            var guard = 0
            while (guard++ < 5000) {
                session.updateAndAwait(sim.nextFix())
                if (sim.isFinished() && session.position.value?.progress == TripProgress.ARRIVED) break
            }
            assertEquals(TripProgress.ARRIVED, session.position.value?.progress)
        }
        sim.close()
    }
}
