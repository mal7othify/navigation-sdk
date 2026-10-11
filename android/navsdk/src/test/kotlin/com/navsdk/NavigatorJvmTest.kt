package com.navsdk

import com.navsdk.core.GeoPoint
import com.navsdk.core.RawLocation
import com.navsdk.core.RerouteReason
import com.navsdk.core.RouteSimulator
import com.navsdk.core.SimulatorConfig
import com.navsdk.core.TripProgress
import com.navsdk.core.coreVersion
import com.navsdk.core.routeFromJson
import kotlinx.coroutines.test.runTest
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.io.File

/**
 * Runs the real Rust core on the JVM through the generated bindings, using the
 * host-built dylib on `jna.library.path`. Skipped when the library is absent.
 */
class NavigatorJvmTest {
    private val libDir = File(System.getProperty("jna.library.path") ?: "")
    private val fixtures = File(System.getProperty("navsdk.fixtures") ?: "")

    private fun requireNative() {
        val present = libDir.listFiles()?.any { it.name.startsWith("libnavcore_ffi") } == true
        assumeTrue("host navcore_ffi library not built; run cargo build -p navcore-ffi --release", present)
    }

    @Test
    fun coreLinks() {
        requireNative()
        assertTrue(coreVersion().isNotEmpty())
    }

    @Test
    fun detourRequestsRerouteAndReplannerInstallsRoute() = runTest {
        requireNative()
        val fixture = JSONObject(File(fixtures, "route_detour.json").readText())
        val route = routeFromJson(fixture.getJSONObject("route").toString())
        val trace = fixture.getJSONArray("trace")
        var requests = 0
        NavigationSession(route, replanner = { request ->
            requests += 1
            assertEquals(RerouteReason.OFF_ROUTE, request.reason)
            route
        }).use { session ->
            var sawOffRoute = false
            var tripChanges = 0
            for (i in 0 until trace.length()) {
                val fix = trace.getJSONObject(i)
                val point = fix.getJSONObject("point")
                val state = session.updateAndAwait(
                    RawLocation(
                        point = GeoPoint(point.getDouble("lat"), point.getDouble("lng")),
                        accuracyM = fix.optDouble("accuracy_m").takeIf { !it.isNaN() },
                        speedMps = fix.optDouble("speed_mps").takeIf { !it.isNaN() },
                        courseDeg = fix.optDouble("course_deg").takeIf { !it.isNaN() },
                        timestampMs = fix.getLong("timestamp_ms").toULong(),
                    )
                )
                sawOffRoute = sawOffRoute || state.position.isOffRoute
                if (state.tripChanged) tripChanges += 1
            }
            assertTrue(sawOffRoute)
            assertTrue(session.rerouteRequests.value >= 1)
            assertTrue(tripChanges * 5 < trace.length())
        }
        assertTrue(requests >= 1)
    }

    @Test
    fun simulatorDrivesNavigatorToArrival() = runTest {
        requireNative()
        val fixture = JSONObject(File(fixtures, "route_simple.json").readText())
        val route = routeFromJson(fixture.getJSONObject("route").toString())
        val sim = RouteSimulator(route, SimulatorConfig())
        NavigationSession(route).use { session ->
            var last = session.state.value
            var guard = 0
            while (guard++ < 5000) {
                last = session.updateAndAwait(sim.nextFix())
                if (sim.isFinished() && last.position.progress == TripProgress.ARRIVED) break
            }
            assertEquals(TripProgress.ARRIVED, last!!.position.progress)
        }
        sim.destroy()
    }

    @Test
    fun replayReachesArrival() = runTest {
        requireNative()
        val fixture = JSONObject(File(fixtures, "route_simple.json").readText())
        val route = routeFromJson(fixture.getJSONObject("route").toString())
        val trace = fixture.getJSONArray("trace")

        NavigationSession(route).use { session ->
            var last = session.state.value
            for (i in 0 until trace.length()) {
                val fix = trace.getJSONObject(i)
                val point = fix.getJSONObject("point")
                last = session.updateAndAwait(
                    RawLocation(
                        point = GeoPoint(point.getDouble("lat"), point.getDouble("lng")),
                        accuracyM = fix.optDouble("accuracy_m").takeIf { !it.isNaN() },
                        speedMps = fix.optDouble("speed_mps").takeIf { !it.isNaN() },
                        courseDeg = fix.optDouble("course_deg").takeIf { !it.isNaN() },
                        timestampMs = fix.getLong("timestamp_ms").toULong(),
                    )
                )
            }
            assertNotNull(last)
            assertEquals(TripProgress.ARRIVED, last!!.trip.progress)
            assertEquals(TripProgress.ARRIVED, last.position.progress)
            assertEquals(session.state.value, last)
            assertEquals(session.position.value, last.position)
            assertEquals(session.trip.value, last.trip)
            assertEquals(0, session.rerouteRequests.value)
        }
    }
}
