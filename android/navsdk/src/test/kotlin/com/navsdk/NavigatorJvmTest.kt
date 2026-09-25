package com.navsdk

import com.navsdk.core.GeoPoint
import com.navsdk.core.RawLocation
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
            assertEquals(TripProgress.ARRIVED, last!!.progress)
            assertEquals(session.state.value, last)
        }
    }
}
