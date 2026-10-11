package com.navsdk.demo

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.navsdk.NavigationSession
import com.navsdk.push.NavigationSessionPush
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import com.navsdk.core.TripProgress as PullProgress
import com.navsdk.push.core.TripProgress as PushProgress

/** Replays the bundled fixtures through both native cores on device. */
@RunWith(AndroidJUnit4::class)
class ReplayArrivalTest {
    private val assets = InstrumentationRegistry.getInstrumentation().targetContext.assets

    @Test
    fun pullRouteSimpleReachesArrival() = runTest {
        val fixture = Fixture.load(assets, "route_simple")
        NavigationSession(fixture.pullRoute).use { session ->
            var last = session.state.value
            for (raw in fixture.pullTrace) last = session.updateAndAwait(raw)
            assertEquals(PullProgress.ARRIVED, last!!.trip.progress)
            assertEquals(0, session.rerouteRequests.value)
            assertEquals(fixture.size, session.positionUpdates.value)
            assertTrue(session.tripUpdates.value <= fixture.pullRoute.steps.size + 1)
        }
    }

    @Test
    fun pullRouteDetourRequestsRerouteAndStillArrives() = runTest {
        val fixture = Fixture.load(assets, "route_detour")
        NavigationSession(fixture.pullRoute).use { session ->
            var sawOffRoute = false
            var last = session.state.value
            for (raw in fixture.pullTrace) {
                last = session.updateAndAwait(raw)
                sawOffRoute = sawOffRoute || last.position.isOffRoute
            }
            assertTrue(sawOffRoute)
            assertTrue(session.rerouteRequests.value >= 1)
            assertEquals(PullProgress.ARRIVED, last!!.trip.progress)
        }
    }

    @Test
    fun pushRouteSimpleReachesArrivalThroughCallbacks() = runTest {
        val fixture = Fixture.load(assets, "route_simple")
        NavigationSessionPush(fixture.pushRoute).use { session ->
            for (raw in fixture.pushTrace) assertNotNull(session.updateAndAwait(raw))
            assertEquals(PushProgress.ARRIVED, session.position.value!!.progress)
            assertEquals(PushProgress.ARRIVED, session.trip.value!!.progress)
            assertEquals(fixture.size, session.positionCallbacks.value)
            assertTrue(session.tripCallbacks.value <= fixture.pushRoute.steps.size + 1)
        }
    }

    @Test
    fun pushRouteDetourIsReplannedByTheCore() = runTest {
        val fixture = Fixture.load(assets, "route_detour")
        NavigationSessionPush(fixture.pushRoute, replanner = { fixture.pushRoute }).use { session ->
            var sawOffRoute = false
            for (raw in fixture.pushTrace) {
                session.updateAndAwait(raw)
                sawOffRoute = sawOffRoute || (session.position.value?.isOffRoute == true)
                if (session.session.value.replanInFlight) {
                    withTimeout(5_000) { while (session.session.value.replanInFlight) delay(10) }
                }
            }
            assertTrue(sawOffRoute)
            assertTrue(session.rerouteRequests.value >= 1)
            assertTrue(session.session.value.routesInstalled >= 1u)
        }
    }

    /** Both approaches over the same fixes must snap to the same place, fix after fix. */
    @Test
    fun pullAndPushNeverDiverge() = runTest {
        val fixture = Fixture.load(assets, "route_simple")
        NavigationSession(fixture.pullRoute).use { pull ->
            NavigationSessionPush(fixture.pushRoute).use { push ->
                for (i in 0 until fixture.size) {
                    val a = pull.updateAndAwait(fixture.pullTrace[i]).position
                    val b = push.updateAndAwait(fixture.pushTrace[i])!!
                    val d = haversineM(a.snapped.point.lat, a.snapped.point.lng, b.snapped.point.lat, b.snapped.point.lng)
                    assertTrue("fix $i diverged by $d m", d < 0.5)
                    assertEquals(a.distanceRemainingM, b.distanceRemainingM, 1e-6)
                    assertEquals(a.isOffRoute, b.isOffRoute)
                }
                assertEquals(pull.tripUpdates.value, push.tripCallbacks.value)
            }
        }
    }
}
