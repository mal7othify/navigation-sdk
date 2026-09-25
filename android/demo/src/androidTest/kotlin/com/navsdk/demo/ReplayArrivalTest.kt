package com.navsdk.demo

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.navsdk.NavigationSession
import com.navsdk.core.TripProgress
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** Replays the bundled fixtures through the real native core on device. */
@RunWith(AndroidJUnit4::class)
class ReplayArrivalTest {
    private val assets = InstrumentationRegistry.getInstrumentation().targetContext.assets

    @Test
    fun routeSimpleReachesArrival() = runTest {
        val fixture = Fixture.load(assets, "route_simple")
        NavigationSession(fixture.route).use { session ->
            var last = session.state.value
            for (raw in fixture.trace) last = session.updateAndAwait(raw)
            assertEquals(TripProgress.ARRIVED, last!!.progress)
            assertTrue(session.rerouteRequests.value == 0)
        }
    }

    @Test
    fun routeDetourRequestsRerouteAndStillArrives() = runTest {
        val fixture = Fixture.load(assets, "route_detour")
        NavigationSession(fixture.route).use { session ->
            var sawOffRoute = false
            var last = session.state.value
            for (raw in fixture.trace) {
                last = session.updateAndAwait(raw)
                sawOffRoute = sawOffRoute || last.isOffRoute
            }
            assertTrue(sawOffRoute)
            assertTrue(session.rerouteRequests.value >= 1)
            assertEquals(TripProgress.ARRIVED, last!!.progress)
        }
    }
}
